//! Transport-neutral RPC boundary for the daemon.
//!
//! `ateliad` does not currently depend on tonic/prost server generation, so
//! this module keeps the proto-facing shape explicit without forcing a broad
//! dependency migration. A future transport layer should delegate to this
//! boundary rather than reimplementing service behavior.

#![allow(dead_code)]

use crate::service::{
    CheckPolicyRequest as ServiceCheckPolicyRequest, DaemonHealth, DaemonStatus,
    ListRepositoriesRequest as ServiceListRepositoriesRequest,
    ListToolOutputSettingsHistoryRequest as ServiceListToolOutputSettingsHistoryRequest,
    ProtocolMetadata as ServiceProtocolMetadata,
    RegisterRepositoryRequest as ServiceRegisterRepositoryRequest, SecretaryService, ServiceError,
    StorageStatus, SubmitJobRequest as ServiceSubmitJobRequest,
};
use atelia_core::{
    Actor, ApplyBlocklistRequest, BlocklistEntry, CancelJobReceipt, CancellationState,
    ExtensionInstallRecord, ExtensionStatusRequest, InstallExtensionRequest, JobEventKind, JobId,
    JobKind, JobRecord, JobStatus, ListBlocklistRequest, ListExtensionsRequest, OutputFormat,
    OversizeOutputPolicy, PathScope, PolicyOutcome, ProjectId, RenderOptions, RepositoryId,
    RepositoryRecord, RepositoryTrustState, ResourceScope, RiskTier, RollbackExtensionRequest,
    StoreError, ToolOutputDefaults, ToolOutputGranularity, ToolOutputOverrides,
    ToolOutputSettingsChange, ToolOutputSettingsScope, ToolOutputVerbosity,
};
use std::convert::TryFrom;

pub const TRANSPORT_BLOCKER: &str =
    "tonic/prost server dependencies are not configured for ateliad";

#[allow(dead_code)]
pub struct SecretaryRpcServer {
    service: SecretaryService,
}

impl SecretaryRpcServer {
    pub fn new(service: SecretaryService) -> Self {
        Self { service }
    }

    pub fn service_mut(&mut self) -> &mut SecretaryService {
        &mut self.service
    }

    pub fn transport_blocker(&self) -> Option<&'static str> {
        Some(TRANSPORT_BLOCKER)
    }

    pub fn health(&self, _request: HealthRequest) -> HealthResponse {
        HealthResponse::from(self.service.health())
    }

    pub fn register_repository(
        &self,
        request: RegisterRepositoryRequest,
    ) -> RpcResult<RegisterRepositoryResponse> {
        let trust_state = infer_repository_trust_state(&request.allowed_scope);
        let requested_scope = request
            .allowed_scope
            .map(|scope| parse_repository_allowed_scope(scope, &request.root_path))
            .transpose()?
            .map(Some)
            .unwrap_or(None);
        let requester = request.requester.map(Actor::try_from).transpose()?;

        let repository = self
            .service
            .register_repository(ServiceRegisterRepositoryRequest {
                display_name: request.display_name,
                root_path: request.root_path,
                trust_state,
                allowed_scope: requested_scope,
                requester,
            })?;

        Ok(RegisterRepositoryResponse {
            metadata: self.metadata(),
            repository: Repository::from(repository),
            policy: None,
        })
    }

    pub fn list_repositories(
        &self,
        request: ListRepositoriesRequest,
    ) -> RpcResult<ListRepositoriesResponse> {
        let request = parse_list_repositories_request(request)?;
        let page = self.service.list_repositories_page(request)?;

        let repositories = page
            .repositories
            .into_iter()
            .map(Repository::from)
            .collect();

        Ok(ListRepositoriesResponse {
            metadata: self.metadata(),
            repositories,
            next_page_token: page.next_page_token,
        })
    }

    pub fn submit_job(&self, request: SubmitJobRequest) -> RpcResult<SubmitJobResponse> {
        let repository_id = parse_repository_id(&request.repository_id)?;
        let path_scope = request
            .path_scope
            .map(parse_path_scope)
            .transpose()?
            .flatten();
        let receipt = self.service.submit_job(ServiceSubmitJobRequest {
            requester: Actor::try_from(request.requester)?,
            repository_id,
            kind: parse_job_kind(&request.kind)?,
            goal: request.goal,
            resource_scope: path_scope,
            requested_capabilities: request.requested_capabilities,
            idempotency_key: request.idempotency_key,
        })?;

        Ok(SubmitJobResponse {
            metadata: self.metadata(),
            job: Job::from(receipt.job),
            policy: policy_decision_to_rpc(receipt.policy_decision),
        })
    }

    pub fn check_policy(&self, request: CheckPolicyRequest) -> RpcResult<CheckPolicyResponse> {
        if request.tool_result.is_some() {
            return Err(RpcError::invalid_argument(
                "tool_result is not supported for policy-check previews",
            ));
        }

        let repository_id = parse_repository_id(&request.repository_id)?;
        let requester = Actor::try_from(request.requester)?;
        let requested_capability =
            parse_non_empty_field("requested_capability", request.requested_capability)?;
        let action = parse_non_empty_field("action", request.action)?;
        let resource_scope = match request.path_scope {
            Some(path_scope) => {
                parse_path_scope(path_scope)?.unwrap_or_else(default_resource_scope)
            }
            None => default_resource_scope(),
        };
        let policy_decision = self.service.check_policy(ServiceCheckPolicyRequest {
            repository_id,
            requester,
            requested_capability,
            action,
            resource_scope,
        })?;

        Ok(CheckPolicyResponse {
            metadata: self.metadata(),
            decision: policy_decision_to_rpc(policy_decision),
        })
    }

    pub fn install_extension(
        &self,
        request: InstallExtensionRequest,
    ) -> RpcResult<InstallExtensionResponse> {
        let response = self.service.install_extension(request)?;
        Ok(InstallExtensionResponse {
            metadata: self.metadata(),
            record: response.record,
        })
    }

    pub fn extension_status(
        &self,
        request: ExtensionStatusRequest,
    ) -> RpcResult<ExtensionStatusResponse> {
        let extension = self.service.extension_status(request)?;
        Ok(ExtensionStatusResponse {
            metadata: self.metadata(),
            extension,
        })
    }

    pub fn list_extensions(
        &self,
        request: ListExtensionsRequest,
    ) -> RpcResult<ListExtensionsResponse> {
        let response = self.service.list_extensions(request)?;
        Ok(ListExtensionsResponse {
            metadata: self.metadata(),
            extensions: response.extensions,
        })
    }

    pub fn rollback_extension(
        &self,
        request: RollbackExtensionRequest,
    ) -> RpcResult<RollbackExtensionResponse> {
        let response = self.service.rollback_extension(request)?;
        Ok(RollbackExtensionResponse {
            metadata: self.metadata(),
            record: response.record,
        })
    }

    pub fn apply_blocklist(
        &self,
        request: ApplyBlocklistRequest,
    ) -> RpcResult<ApplyBlocklistResponse> {
        let response = self.service.apply_blocklist(request)?;
        Ok(ApplyBlocklistResponse {
            metadata: self.metadata(),
            entry: response.entry,
        })
    }

    pub fn list_blocklist(
        &self,
        request: ListBlocklistRequest,
    ) -> RpcResult<ListBlocklistResponse> {
        let response = self.service.list_blocklist(request)?;
        Ok(ListBlocklistResponse {
            metadata: self.metadata(),
            entries: response.entries,
        })
    }

    pub fn get_job(&self, request: GetJobRequest) -> RpcResult<GetJobResponse> {
        let job_id = parse_job_id(&request.job_id)?;
        let job = self.service.get_job(&job_id)?;
        let cancellation_requester = self.service.cancellation_requester(&job_id);

        Ok(GetJobResponse {
            metadata: self.metadata(),
            job: Job::from_with_cancellation_requester(job, cancellation_requester.as_ref()),
        })
    }

    pub fn list_jobs(&self, request: ListJobsRequest) -> RpcResult<ListJobsResponse> {
        let repository_id = request
            .repository_id
            .as_deref()
            .map(parse_repository_id)
            .transpose()?;
        let status = match request.status {
            None => None,
            Some(status) => job_status_from_rpc(status)?,
        };
        let requester = request.requester.map(Actor::try_from).transpose()?;
        let page = self.service.list_jobs(
            repository_id,
            status,
            requester,
            request.page_size,
            request.page_token,
        )?;
        let jobs = page
            .jobs
            .into_iter()
            .map(|job| {
                let cancellation_requester = self.service.cancellation_requester(&job.id);
                Job::from_with_cancellation_requester(job, cancellation_requester.as_ref())
            })
            .collect();

        Ok(ListJobsResponse {
            metadata: self.metadata(),
            jobs,
            next_page_token: page.next_page_token,
        })
    }

    pub fn cancel_job(&self, request: CancelJobRequest) -> RpcResult<CancelJobResponse> {
        let job_id = parse_job_id(&request.job_id)?;
        let requester = Actor::try_from(request.requester)?;
        let receipt = self
            .service
            .cancel_job(&job_id, request.reason, Some(requester.clone()))?;
        let (job, cancellation) = job_from_cancel_receipt(receipt, Some(&requester));

        Ok(CancelJobResponse {
            metadata: self.metadata(),
            job,
            cancellation,
        })
    }

    pub fn get_tool_output_defaults(
        &self,
        request: GetToolOutputDefaultsRequest,
    ) -> RpcResult<GetToolOutputDefaultsResponse> {
        let scope = parse_tool_output_scope(request.scope.clone())?;
        let defaults = self.service.get_tool_output_defaults(scope.clone())?;

        Ok(GetToolOutputDefaultsResponse {
            metadata: self.metadata(),
            scope: RpcToolOutputScope::from(scope),
            defaults: RpcToolOutputDefaults::from(defaults),
        })
    }

    pub fn update_tool_output_defaults(
        &self,
        request: UpdateToolOutputDefaultsRequest,
    ) -> RpcResult<UpdateToolOutputDefaultsResponse> {
        let scope = parse_tool_output_scope(request.scope)?;
        let actor = Actor::try_from(request.actor)?;
        let overrides = parse_tool_output_overrides(request.overrides)?;
        let change =
            self.service
                .update_tool_output_defaults(actor, scope, overrides, request.reason)?;

        Ok(UpdateToolOutputDefaultsResponse {
            metadata: self.metadata(),
            change: RpcToolOutputSettingsChange::from(change),
        })
    }

    pub fn list_tool_output_settings_history(
        &self,
        request: ListToolOutputSettingsHistoryRequest,
    ) -> RpcResult<ListToolOutputSettingsHistoryResponse> {
        let scope = request.scope.map(parse_tool_output_scope).transpose()?;
        let page = self.service.list_tool_output_settings_history_page(
            ServiceListToolOutputSettingsHistoryRequest {
                scope,
                limit: request.limit,
                offset: request.offset,
                cursor: request.cursor,
            },
        )?;
        let changes = page
            .changes
            .into_iter()
            .map(RpcToolOutputSettingsChange::from)
            .collect();

        Ok(ListToolOutputSettingsHistoryResponse {
            metadata: self.metadata(),
            changes,
            next_page_token: page.next_page_token,
        })
    }

    fn metadata(&self) -> ProtocolMetadata {
        ProtocolMetadata::from(self.service.protocol_metadata())
    }
}

pub type RpcResult<T> = Result<T, RpcError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: RpcErrorCode,
    pub reason: String,
}

impl RpcError {
    fn invalid_argument(reason: impl Into<String>) -> Self {
        Self {
            code: RpcErrorCode::InvalidArgument,
            reason: reason.into(),
        }
    }
}

impl From<ServiceError> for RpcError {
    fn from(error: ServiceError) -> Self {
        match error {
            ServiceError::InvalidArgument { reason } => Self {
                code: RpcErrorCode::InvalidArgument,
                reason,
            },
            ServiceError::Conflict { reason } => Self {
                code: RpcErrorCode::Conflict,
                reason,
            },
            ServiceError::Settings(err) => Self {
                code: RpcErrorCode::InvalidArgument,
                reason: err.to_string(),
            },
            ServiceError::Store(error) => store_error_to_rpc(error),
            ServiceError::Runtime(error) => Self {
                code: RpcErrorCode::Internal,
                reason: error.to_string(),
            },
            ServiceError::ExtensionRegistry(error) => registry_error_to_rpc(error),
            ServiceError::Internal { reason } => Self {
                code: RpcErrorCode::Internal,
                reason,
            },
        }
    }
}

fn registry_error_to_rpc(error: atelia_core::RegistryError) -> RpcError {
    match error {
        atelia_core::RegistryError::Validation(error) => RpcError {
            code: RpcErrorCode::InvalidArgument,
            reason: error.to_string(),
        },
        atelia_core::RegistryError::UnsupportedBlocklistKey { key } => RpcError {
            code: RpcErrorCode::InvalidArgument,
            reason: format!("unsupported blocklist key: {key:?}"),
        },
        atelia_core::RegistryError::Blocked { .. }
        | atelia_core::RegistryError::DigestConflict { .. }
        | atelia_core::RegistryError::RollbackUnavailable { .. } => RpcError {
            code: RpcErrorCode::Conflict,
            reason: error.to_string(),
        },
        atelia_core::RegistryError::ServiceDenied { .. } => RpcError {
            code: RpcErrorCode::InvalidArgument,
            reason: error.to_string(),
        },
        atelia_core::RegistryError::NotInstalled { .. } => RpcError {
            code: RpcErrorCode::NotFound,
            reason: error.to_string(),
        },
        atelia_core::RegistryError::ServiceUnavailable { .. } => RpcError {
            code: RpcErrorCode::NotFound,
            reason: error.to_string(),
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcErrorCode {
    InvalidArgument,
    NotFound,
    Conflict,
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthResponse {
    pub status: String,
    pub daemon_version: String,
    pub protocol_version: String,
    pub storage_version: String,
    pub storage_status: String,
    pub daemon_status: String,
    pub capabilities: Vec<String>,
}

impl From<DaemonHealth> for HealthResponse {
    fn from(health: DaemonHealth) -> Self {
        Self {
            status: health_status_label(health.daemon_status, health.storage_status).to_string(),
            daemon_version: health.daemon_version,
            protocol_version: health.protocol_version,
            storage_version: health.storage_version,
            storage_status: storage_status_label(health.storage_status).to_string(),
            daemon_status: daemon_status_label(health.daemon_status).to_string(),
            capabilities: health.capabilities,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolMetadata {
    pub protocol_version: String,
    pub daemon_version: String,
    pub storage_version: String,
    pub capabilities: Vec<String>,
}

impl From<ServiceProtocolMetadata> for ProtocolMetadata {
    fn from(metadata: ServiceProtocolMetadata) -> Self {
        Self {
            protocol_version: metadata.protocol_version,
            daemon_version: metadata.daemon_version,
            storage_version: metadata.storage_version,
            capabilities: metadata.capabilities,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterRepositoryRequest {
    pub display_name: String,
    pub root_path: String,
    pub allowed_scope: Option<RpcPathScope>,
    pub requester: Option<RpcActorDto>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterRepositoryResponse {
    pub metadata: ProtocolMetadata,
    pub repository: Repository,
    pub policy: Option<PolicyDecision>,
}

// Mirrors proto's `ListRepositoriesRequest` contract.
// `trust_state` is optional so callers can omit it instead of sending
// an explicit unspecified enum value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRepositoriesRequest {
    pub trust_state: Option<RpcRepositoryTrustState>,
    pub page_size: Option<usize>,
    pub page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRepositoriesResponse {
    pub metadata: ProtocolMetadata,
    pub repositories: Vec<Repository>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetToolOutputDefaultsRequest {
    pub scope: RpcToolOutputScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetToolOutputDefaultsResponse {
    pub metadata: ProtocolMetadata,
    pub scope: RpcToolOutputScope,
    pub defaults: RpcToolOutputDefaults,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateToolOutputDefaultsRequest {
    pub scope: RpcToolOutputScope,
    pub actor: RpcActorDto,
    pub reason: String,
    pub overrides: RpcToolOutputOverrides,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateToolOutputDefaultsResponse {
    pub metadata: ProtocolMetadata,
    pub change: RpcToolOutputSettingsChange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListToolOutputSettingsHistoryRequest {
    pub scope: Option<RpcToolOutputScope>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListToolOutputSettingsHistoryResponse {
    pub metadata: ProtocolMetadata,
    pub changes: Vec<RpcToolOutputSettingsChange>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcToolOutputScope {
    pub level: RpcToolOutputScopeLevel,
    pub tool_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcToolOutputScopeLevel {
    Workspace,
    Repository { repository_id: String },
    Project { project_id: String },
    Session { session_id: String },
    AgentProfile { agent_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcToolOutputDefaults {
    pub render_options: RpcToolOutputRenderOptions,
    pub max_inline_bytes: u64,
    pub max_inline_lines: u32,
    pub verbosity: RpcToolOutputVerbosity,
    pub granularity: RpcToolOutputGranularity,
    pub oversize_policy: RpcOversizeOutputPolicy,
}

impl From<ToolOutputDefaults> for RpcToolOutputDefaults {
    fn from(defaults: ToolOutputDefaults) -> Self {
        Self {
            render_options: RpcToolOutputRenderOptions::from(defaults.render_options),
            max_inline_bytes: defaults.max_inline_bytes,
            max_inline_lines: defaults.max_inline_lines,
            verbosity: RpcToolOutputVerbosity::from(defaults.verbosity),
            granularity: RpcToolOutputGranularity::from(defaults.granularity),
            oversize_policy: RpcOversizeOutputPolicy::from(defaults.oversize_policy),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcToolOutputRenderOptions {
    pub format: RpcOutputFormat,
    pub include_policy: bool,
    pub include_diagnostics: bool,
    pub include_cost: bool,
}

impl From<RenderOptions> for RpcToolOutputRenderOptions {
    fn from(options: RenderOptions) -> Self {
        Self {
            format: RpcOutputFormat::from(options.format),
            include_policy: options.include_policy,
            include_diagnostics: options.include_diagnostics,
            include_cost: options.include_cost,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RpcToolOutputOverrides {
    pub format: Option<RpcOutputFormat>,
    pub include_policy: Option<bool>,
    pub include_diagnostics: Option<bool>,
    pub include_cost: Option<bool>,
    pub max_inline_bytes: Option<u64>,
    pub max_inline_lines: Option<u32>,
    pub verbosity: Option<RpcToolOutputVerbosity>,
    pub granularity: Option<RpcToolOutputGranularity>,
    pub oversize_policy: Option<RpcOversizeOutputPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcToolOutputSettingsChange {
    pub schema_version: u32,
    pub actor: RpcActorDto,
    pub scope: RpcToolOutputScope,
    pub old_defaults: RpcToolOutputDefaults,
    pub new_defaults: RpcToolOutputDefaults,
    pub reason: String,
    pub changed_at_unix_ms: i64,
}

impl From<ToolOutputSettingsChange> for RpcToolOutputSettingsChange {
    fn from(change: ToolOutputSettingsChange) -> Self {
        Self {
            schema_version: change.schema_version,
            actor: RpcActorDto::from(change.actor),
            scope: RpcToolOutputScope::from(change.scope),
            old_defaults: RpcToolOutputDefaults::from(change.old_defaults),
            new_defaults: RpcToolOutputDefaults::from(change.new_defaults),
            reason: change.reason,
            changed_at_unix_ms: change.changed_at.unix_millis,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcOutputFormat {
    Toon,
    Json,
    Text,
}

impl From<OutputFormat> for RpcOutputFormat {
    fn from(format: OutputFormat) -> Self {
        match format {
            OutputFormat::Toon => Self::Toon,
            OutputFormat::Json => Self::Json,
            OutputFormat::Text => Self::Text,
        }
    }
}

impl TryFrom<RpcOutputFormat> for OutputFormat {
    type Error = RpcError;

    fn try_from(value: RpcOutputFormat) -> Result<Self, Self::Error> {
        Ok(match value {
            RpcOutputFormat::Toon => Self::Toon,
            RpcOutputFormat::Json => Self::Json,
            RpcOutputFormat::Text => Self::Text,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcToolOutputVerbosity {
    Minimal,
    Normal,
    Expanded,
    Debug,
}

impl From<ToolOutputVerbosity> for RpcToolOutputVerbosity {
    fn from(verbosity: ToolOutputVerbosity) -> Self {
        match verbosity {
            ToolOutputVerbosity::Minimal => Self::Minimal,
            ToolOutputVerbosity::Normal => Self::Normal,
            ToolOutputVerbosity::Expanded => Self::Expanded,
            ToolOutputVerbosity::Debug => Self::Debug,
        }
    }
}

impl TryFrom<RpcToolOutputVerbosity> for ToolOutputVerbosity {
    type Error = RpcError;

    fn try_from(value: RpcToolOutputVerbosity) -> Result<Self, Self::Error> {
        Ok(match value {
            RpcToolOutputVerbosity::Minimal => Self::Minimal,
            RpcToolOutputVerbosity::Normal => Self::Normal,
            RpcToolOutputVerbosity::Expanded => Self::Expanded,
            RpcToolOutputVerbosity::Debug => Self::Debug,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcToolOutputGranularity {
    Summary,
    KeyFields,
    Full,
}

impl From<ToolOutputGranularity> for RpcToolOutputGranularity {
    fn from(granularity: ToolOutputGranularity) -> Self {
        match granularity {
            ToolOutputGranularity::Summary => Self::Summary,
            ToolOutputGranularity::KeyFields => Self::KeyFields,
            ToolOutputGranularity::Full => Self::Full,
        }
    }
}

impl TryFrom<RpcToolOutputGranularity> for ToolOutputGranularity {
    type Error = RpcError;

    fn try_from(value: RpcToolOutputGranularity) -> Result<Self, Self::Error> {
        Ok(match value {
            RpcToolOutputGranularity::Summary => Self::Summary,
            RpcToolOutputGranularity::KeyFields => Self::KeyFields,
            RpcToolOutputGranularity::Full => Self::Full,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcOversizeOutputPolicy {
    TruncateWithMetadata,
    SpillToArtifactRef,
    RejectOversize,
}

impl From<OversizeOutputPolicy> for RpcOversizeOutputPolicy {
    fn from(policy: OversizeOutputPolicy) -> Self {
        match policy {
            OversizeOutputPolicy::TruncateWithMetadata => Self::TruncateWithMetadata,
            OversizeOutputPolicy::SpillToArtifactRef => Self::SpillToArtifactRef,
            OversizeOutputPolicy::RejectOversize => Self::RejectOversize,
        }
    }
}

impl TryFrom<RpcOversizeOutputPolicy> for OversizeOutputPolicy {
    type Error = RpcError;

    fn try_from(value: RpcOversizeOutputPolicy) -> Result<Self, Self::Error> {
        Ok(match value {
            RpcOversizeOutputPolicy::TruncateWithMetadata => Self::TruncateWithMetadata,
            RpcOversizeOutputPolicy::SpillToArtifactRef => Self::SpillToArtifactRef,
            RpcOversizeOutputPolicy::RejectOversize => Self::RejectOversize,
        })
    }
}

impl From<ToolOutputSettingsScope> for RpcToolOutputScope {
    fn from(scope: ToolOutputSettingsScope) -> Self {
        Self {
            level: match scope.level {
                atelia_core::ToolOutputSettingsLevel::Workspace => {
                    RpcToolOutputScopeLevel::Workspace
                }
                atelia_core::ToolOutputSettingsLevel::Repository { repository_id } => {
                    RpcToolOutputScopeLevel::Repository {
                        repository_id: repository_id.as_str().to_string(),
                    }
                }
                atelia_core::ToolOutputSettingsLevel::Project { project_id } => {
                    RpcToolOutputScopeLevel::Project {
                        project_id: project_id_to_string(&project_id),
                    }
                }
                atelia_core::ToolOutputSettingsLevel::Session { session_id } => {
                    RpcToolOutputScopeLevel::Session { session_id }
                }
                atelia_core::ToolOutputSettingsLevel::AgentProfile { agent_id } => {
                    RpcToolOutputScopeLevel::AgentProfile { agent_id }
                }
            },
            tool_id: scope.tool_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub repository_id: String,
    pub display_name: String,
    pub root_path: String,
    pub allowed_scope: RpcPathScope,
    pub trust_state: RpcRepositoryTrustState,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

impl From<RepositoryRecord> for Repository {
    fn from(record: RepositoryRecord) -> Self {
        Self {
            repository_id: record.id.as_str().to_string(),
            display_name: record.display_name,
            root_path: record.root_path,
            allowed_scope: rpc_path_scope_from_record(
                &record.allowed_path_scope,
                record.trust_state.clone(),
            ),
            trust_state: record.trust_state.into(),
            created_at_unix_ms: record.created_at.unix_millis,
            updated_at_unix_ms: record.updated_at.unix_millis,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitJobRequest {
    pub repository_id: String,
    pub requester: RpcActorDto,
    pub kind: String,
    pub goal: String,
    pub path_scope: Option<RpcPathScope>,
    pub requested_capabilities: Vec<String>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitJobResponse {
    pub metadata: ProtocolMetadata,
    pub job: Job,
    pub policy: PolicyDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetJobRequest {
    pub job_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetJobResponse {
    pub metadata: ProtocolMetadata,
    pub job: Job,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListJobsRequest {
    pub repository_id: Option<String>,
    pub status: Option<RpcJobStatus>,
    pub requester: Option<RpcActorDto>,
    pub page_size: Option<usize>,
    pub page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListJobsResponse {
    pub metadata: ProtocolMetadata,
    pub jobs: Vec<Job>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelJobRequest {
    pub job_id: String,
    pub requester: RpcActorDto,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelJobResponse {
    pub metadata: ProtocolMetadata,
    pub job: Job,
    pub cancellation: JobCancellation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckPolicyRequest {
    pub repository_id: String,
    pub requester: RpcActorDto,
    pub requested_capability: String,
    pub action: String,
    pub path_scope: Option<RpcPathScope>,
    pub tool_result: Option<ToolResultRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckPolicyResponse {
    pub metadata: ProtocolMetadata,
    pub decision: PolicyDecision,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallExtensionResponse {
    pub metadata: ProtocolMetadata,
    pub record: ExtensionInstallRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionStatusResponse {
    pub metadata: ProtocolMetadata,
    pub extension: atelia_core::ExtensionStatusResponse,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListExtensionsResponse {
    pub metadata: ProtocolMetadata,
    pub extensions: Vec<atelia_core::ExtensionStatusResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackExtensionResponse {
    pub metadata: ProtocolMetadata,
    pub record: ExtensionInstallRecord,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyBlocklistResponse {
    pub metadata: ProtocolMetadata,
    pub entry: BlocklistEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListBlocklistResponse {
    pub metadata: ProtocolMetadata,
    pub entries: Vec<BlocklistEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResultRef {
    pub tool_result_id: String,
    pub tool_invocation_id: String,
    pub job_id: String,
    pub repository_id: String,
    pub content_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcPathScope {
    pub kind: RpcPathScopeKind,
    pub roots: Vec<String>,
    pub include_patterns: Vec<String>,
    pub exclude_patterns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcPathScopeKind {
    Unspecified,
    Repository,
    ExplicitPaths,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub job_id: String,
    pub repository_id: String,
    pub requester: RpcActorDto,
    pub kind: String,
    pub goal: String,
    pub status: String,
    pub policy_summary: Option<PolicySummary>,
    pub created_at_unix_ms: i64,
    pub started_at_unix_ms: Option<i64>,
    pub completed_at_unix_ms: Option<i64>,
    pub latest_event_id: Option<String>,
    pub cancellation: JobCancellation,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCancellation {
    pub state: String,
    pub requested_by: Option<RpcActorDto>,
    pub reason: Option<String>,
    pub requested_at_unix_ms: Option<i64>,
    pub completed_at_unix_ms: Option<i64>,
}

impl From<JobRecord> for Job {
    fn from(record: JobRecord) -> Self {
        Job::from_with_cancellation_requester(record, None)
    }
}

impl Job {
    fn from_with_cancellation_requester(
        record: JobRecord,
        cancellation_requester: Option<&Actor>,
    ) -> Self {
        let cancellation = job_cancellation_from_record(&record, cancellation_requester);
        Self {
            job_id: record.id.as_str().to_string(),
            repository_id: record.repository_id.as_str().to_string(),
            requester: RpcActorDto::from(record.requester),
            kind: job_kind_label(&record.kind).to_string(),
            goal: record.goal,
            status: job_status_label(record.status).to_string(),
            policy_summary: record.policy_summary.map(PolicySummary::from),
            created_at_unix_ms: record.created_at.unix_millis,
            started_at_unix_ms: record.started_at.map(|ts| ts.unix_millis),
            completed_at_unix_ms: record.completed_at.map(|ts| ts.unix_millis),
            latest_event_id: record.latest_event_id.map(|id| id.as_str().to_string()),
            cancellation,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySummary {
    pub decision_id: Option<String>,
    pub outcome: String,
    pub risk_tier: String,
    pub reason_code: String,
}

impl From<atelia_core::PolicySummary> for PolicySummary {
    fn from(summary: atelia_core::PolicySummary) -> Self {
        Self {
            decision_id: summary.decision_id.map(|id| id.as_str().to_string()),
            outcome: policy_outcome_label(summary.outcome).to_string(),
            risk_tier: risk_tier_label(summary.risk_tier).to_string(),
            reason_code: summary.reason_code,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyDecision {
    pub decision_id: String,
    pub outcome: String,
    pub risk_tier: String,
    pub requested_capability: String,
    pub reason_code: String,
    pub reason: String,
    pub approval_request_ref: Option<String>,
    pub audit_ref: Option<String>,
}

fn parse_repository_id(value: &str) -> RpcResult<RepositoryId> {
    RepositoryId::try_from_string(value.to_string())
        .map_err(|_| RpcError::invalid_argument("repository_id must be a valid repository id"))
}

fn parse_non_empty_field(field_name: &str, value: String) -> RpcResult<String> {
    let normalized = value.trim().to_string();
    if normalized.is_empty() {
        return Err(RpcError::invalid_argument(format!(
            "{field_name} must not be empty"
        )));
    }

    Ok(normalized)
}

fn validate_tool_result_ref(tool_result: ToolResultRef) -> RpcResult<ToolResultRef> {
    if tool_result.tool_result_id.trim().is_empty()
        && tool_result.tool_invocation_id.trim().is_empty()
        && tool_result.job_id.trim().is_empty()
        && tool_result.repository_id.trim().is_empty()
        && tool_result.content_type.trim().is_empty()
    {
        return Err(RpcError::invalid_argument(
            "tool_result must include at least one identifier",
        ));
    }

    Ok(tool_result)
}

fn parse_list_repositories_request(
    request: ListRepositoriesRequest,
) -> RpcResult<ServiceListRepositoriesRequest> {
    let trust_state = match request.trust_state {
        Some(RpcRepositoryTrustState::Unspecified) | None => None,
        Some(state) => Some(state.try_into()?),
    };

    Ok(ServiceListRepositoriesRequest {
        trust_state,
        page_size: request.page_size,
        page_token: request.page_token,
    })
}

fn parse_tool_output_scope(request: RpcToolOutputScope) -> RpcResult<ToolOutputSettingsScope> {
    let tool_id = request.tool_id.map(|id| {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return Err(RpcError::invalid_argument(
                "tool_output scope.tool_id must not be empty when provided",
            ));
        }

        Ok(trimmed.to_string())
    });
    let level = match request.level {
        RpcToolOutputScopeLevel::Workspace => ToolOutputSettingsScope::workspace(),
        RpcToolOutputScopeLevel::Repository { repository_id } => {
            let repository_id = parse_repository_id(repository_id.trim())?;
            ToolOutputSettingsScope::repository(repository_id)
        }
        RpcToolOutputScopeLevel::Project { project_id } => {
            let project_id = parse_project_id(project_id)?;
            ToolOutputSettingsScope::project(project_id)
        }
        RpcToolOutputScopeLevel::Session { session_id } => {
            let session_id = session_id.trim();
            if session_id.is_empty() {
                return Err(RpcError::invalid_argument(
                    "tool_output session scope requires a non-empty session_id",
                ));
            }

            ToolOutputSettingsScope::session(session_id.to_string())
        }
        RpcToolOutputScopeLevel::AgentProfile { agent_id } => {
            let agent_id = agent_id.trim();
            if agent_id.is_empty() {
                return Err(RpcError::invalid_argument(
                    "tool_output agent_profile scope requires a non-empty agent_id",
                ));
            }

            ToolOutputSettingsScope::agent_profile(agent_id.to_string())
        }
    };

    let mut scope = level;
    if let Some(tool_id) = tool_id.transpose()? {
        scope = scope.for_tool(tool_id);
    }
    Ok(scope)
}

fn parse_tool_output_overrides(request: RpcToolOutputOverrides) -> RpcResult<ToolOutputOverrides> {
    Ok(ToolOutputOverrides {
        format: request.format.map(OutputFormat::try_from).transpose()?,
        include_policy: request.include_policy,
        include_diagnostics: request.include_diagnostics,
        include_cost: request.include_cost,
        max_inline_bytes: request.max_inline_bytes,
        max_inline_lines: request.max_inline_lines,
        verbosity: request
            .verbosity
            .map(ToolOutputVerbosity::try_from)
            .transpose()?,
        granularity: request
            .granularity
            .map(ToolOutputGranularity::try_from)
            .transpose()?,
        oversize_policy: request
            .oversize_policy
            .map(OversizeOutputPolicy::try_from)
            .transpose()?,
    })
}

fn parse_project_id(value: String) -> RpcResult<ProjectId> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(RpcError::invalid_argument(
            "tool_output project scope requires a non-empty project_id",
        ));
    }

    let project_id_json = format!("\"{trimmed}\"");
    let project_id: ProjectId = serde_json::from_str(&project_id_json)
        .map_err(|_| RpcError::invalid_argument("tool_output project_id must be a valid UUID"))?;

    Ok(project_id)
}

fn project_id_to_string(project_id: &ProjectId) -> String {
    serde_json::to_string(project_id)
        .unwrap_or_else(|_| "\"\"".to_string())
        .trim_matches('"')
        .to_string()
}

fn parse_repository_allowed_scope(
    value: RpcPathScope,
    repository_root: &str,
) -> RpcResult<PathScope> {
    if !value.include_patterns.is_empty() || !value.exclude_patterns.is_empty() {
        return Err(RpcError::invalid_argument(
            "repository allowed_scope include_patterns/exclude_patterns are not yet supported",
        ));
    }

    if value.roots.len() > 1 {
        return Err(RpcError::invalid_argument(
            "repository allowed_scope currently supports only one root entry",
        ));
    }

    let first_root = value
        .roots
        .first()
        .cloned()
        .unwrap_or_else(|| ".".to_string());
    Ok(PathScope {
        root_path: repository_root.to_string(),
        allowed_paths: vec![first_root],
    })
}

fn rpc_path_scope_from_record(
    allowed_scope: &PathScope,
    trust_state: RepositoryTrustState,
) -> RpcPathScope {
    RpcPathScope {
        kind: rpc_path_scope_kind(allowed_scope, trust_state),
        roots: if allowed_scope.allowed_paths.is_empty() {
            vec![allowed_scope.root_path.clone()]
        } else {
            allowed_scope.allowed_paths.clone()
        },
        include_patterns: Vec::new(),
        exclude_patterns: Vec::new(),
    }
}

fn rpc_path_scope_kind(
    allowed_scope: &PathScope,
    trust_state: RepositoryTrustState,
) -> RpcPathScopeKind {
    if matches!(trust_state, RepositoryTrustState::ReadOnly) {
        return RpcPathScopeKind::ReadOnly;
    }

    if allowed_scope.allowed_paths.is_empty() {
        return RpcPathScopeKind::Repository;
    }

    if let [path] = &allowed_scope.allowed_paths[..] {
        if path == "." || path == &allowed_scope.root_path {
            return RpcPathScopeKind::Repository;
        }
    }

    RpcPathScopeKind::ExplicitPaths
}

fn job_from_cancel_receipt(
    receipt: CancelJobReceipt,
    cancellation_requester: Option<&Actor>,
) -> (Job, JobCancellation) {
    let cancellation = job_cancellation_from_receipt(&receipt, cancellation_requester);
    let mut job = Job::from_with_cancellation_requester(receipt.job, cancellation_requester);
    job.cancellation = cancellation.clone();
    (job, cancellation)
}

fn default_resource_scope() -> ResourceScope {
    ResourceScope {
        kind: "repository".to_string(),
        value: ".".to_string(),
    }
}

fn policy_decision_to_rpc(decision: atelia_core::PolicyDecision) -> PolicyDecision {
    PolicyDecision {
        decision_id: decision.id.as_str().to_string(),
        outcome: policy_outcome_label(decision.outcome).to_string(),
        risk_tier: risk_tier_label(decision.risk_tier).to_string(),
        requested_capability: decision.requested_capability,
        reason_code: decision.reason_code,
        reason: decision.user_reason,
        approval_request_ref: decision
            .approval_request_ref
            .map(|approval_request_ref| approval_request_ref.id.as_str().to_string()),
        audit_ref: decision
            .audit_ref
            .map(|audit_ref| audit_ref.as_str().to_string()),
    }
}

fn parse_path_scope(value: RpcPathScope) -> RpcResult<Option<ResourceScope>> {
    if value.include_patterns.is_empty() && value.exclude_patterns.is_empty() {
        let scope_kind = match value.kind {
            RpcPathScopeKind::Unspecified => {
                if value.roots.is_empty() {
                    return Ok(None);
                }

                return Err(RpcError::invalid_argument(
                    "path_scope kind is required when roots are provided",
                ));
            }
            RpcPathScopeKind::Repository => "repository",
            RpcPathScopeKind::ExplicitPaths => "explicit_paths",
            RpcPathScopeKind::ReadOnly => "read_only",
        };

        if value.roots.len() > 1 {
            return Err(RpcError::invalid_argument(
                "path_scope currently supports only one root entry",
            ));
        }

        Ok(Some(ResourceScope {
            kind: scope_kind.to_string(),
            value: value
                .roots
                .first()
                .cloned()
                .unwrap_or_else(|| ".".to_string()),
        }))
    } else {
        Err(RpcError::invalid_argument(
            "path_scope include_patterns and exclude_patterns are not supported yet",
        ))
    }
}

fn parse_job_id(value: &str) -> RpcResult<JobId> {
    JobId::try_from_string(value.to_string())
        .map_err(|_| RpcError::invalid_argument("job_id must be a valid job id"))
}

fn parse_job_kind(value: &str) -> RpcResult<JobKind> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(RpcError::invalid_argument("kind must not be empty"));
    }

    Ok(match trimmed.to_ascii_lowercase().as_str() {
        "read" => JobKind::Read,
        "mutate" => JobKind::Mutate,
        "process" => JobKind::Process,
        "maintenance" => JobKind::Maintenance,
        _ => JobKind::Other {
            name: trimmed.to_string(),
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcRepositoryTrustState {
    Unspecified,
    Trusted,
    ReadOnly,
    Blocked,
}

impl From<RepositoryTrustState> for RpcRepositoryTrustState {
    fn from(value: RepositoryTrustState) -> Self {
        match value {
            RepositoryTrustState::Trusted => RpcRepositoryTrustState::Trusted,
            RepositoryTrustState::ReadOnly => RpcRepositoryTrustState::ReadOnly,
            RepositoryTrustState::Blocked => RpcRepositoryTrustState::Blocked,
        }
    }
}

impl TryFrom<RpcRepositoryTrustState> for RepositoryTrustState {
    type Error = RpcError;

    fn try_from(value: RpcRepositoryTrustState) -> Result<Self, Self::Error> {
        match value {
            RpcRepositoryTrustState::Unspecified => {
                Err(RpcError::invalid_argument("trust_state is required"))
            }
            RpcRepositoryTrustState::Trusted => Ok(RepositoryTrustState::Trusted),
            RpcRepositoryTrustState::ReadOnly => Ok(RepositoryTrustState::ReadOnly),
            RpcRepositoryTrustState::Blocked => Ok(RepositoryTrustState::Blocked),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcActorDto {
    User {
        id: String,
        display_name: Option<String>,
    },
    Agent {
        id: String,
        display_name: Option<String>,
    },
    Extension {
        id: String,
    },
    System {
        id: String,
    },
}

impl From<Actor> for RpcActorDto {
    fn from(actor: Actor) -> Self {
        match actor {
            Actor::User { id, display_name } => Self::User { id, display_name },
            Actor::Agent { id, display_name } => Self::Agent { id, display_name },
            Actor::Extension { id } => Self::Extension { id },
            Actor::System { id } => Self::System { id },
        }
    }
}

impl TryFrom<RpcActorDto> for Actor {
    type Error = RpcError;

    fn try_from(value: RpcActorDto) -> Result<Self, Self::Error> {
        match value {
            RpcActorDto::User { id, display_name } => {
                if id.trim().is_empty() {
                    return Err(RpcError::invalid_argument("actor.id must not be empty"));
                }
                Ok(Actor::User { id, display_name })
            }
            RpcActorDto::Agent { id, display_name } => {
                if id.trim().is_empty() {
                    return Err(RpcError::invalid_argument("actor.id must not be empty"));
                }
                Ok(Actor::Agent { id, display_name })
            }
            RpcActorDto::Extension { id } => {
                if id.trim().is_empty() {
                    return Err(RpcError::invalid_argument("actor.id must not be empty"));
                }
                Ok(Actor::Extension { id })
            }
            RpcActorDto::System { id } => {
                if id.trim().is_empty() {
                    return Err(RpcError::invalid_argument("actor.id must not be empty"));
                }
                Ok(Actor::System { id })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcJobStatus {
    Unspecified,
    Queued,
    Running,
    Succeeded,
    Failed,
    Blocked,
    Canceled,
}

impl TryFrom<RpcJobStatus> for JobStatus {
    type Error = RpcError;

    fn try_from(value: RpcJobStatus) -> Result<Self, Self::Error> {
        match value {
            RpcJobStatus::Unspecified => Err(RpcError::invalid_argument("status is required")),
            RpcJobStatus::Queued => Ok(JobStatus::Queued),
            RpcJobStatus::Running => Ok(JobStatus::Running),
            RpcJobStatus::Succeeded => Ok(JobStatus::Succeeded),
            RpcJobStatus::Failed => Ok(JobStatus::Failed),
            RpcJobStatus::Blocked => Ok(JobStatus::Blocked),
            RpcJobStatus::Canceled => Ok(JobStatus::Canceled),
        }
    }
}

fn job_status_from_rpc(status: RpcJobStatus) -> RpcResult<Option<JobStatus>> {
    if let RpcJobStatus::Unspecified = status {
        return Ok(None);
    }

    Ok(Some(status.try_into()?))
}

fn health_status_label(daemon_status: DaemonStatus, storage_status: StorageStatus) -> &'static str {
    match (storage_status, daemon_status) {
        (StorageStatus::Unavailable, _) => "unavailable",
        (StorageStatus::Migrating | StorageStatus::ReadOnly, _) => "degraded",
        (_, DaemonStatus::Degraded) => "degraded",
        _ => daemon_status_label(daemon_status),
    }
}

fn store_error_to_rpc(error: StoreError) -> RpcError {
    let code = match error {
        StoreError::NotFound { .. } => RpcErrorCode::NotFound,
        StoreError::DuplicateId { .. } | StoreError::Conflict { .. } => RpcErrorCode::Conflict,
        StoreError::InvalidReference { .. }
        | StoreError::InvalidCursor { .. }
        | StoreError::InvalidRecord { .. } => RpcErrorCode::InvalidArgument,
        StoreError::SequenceOverflow => RpcErrorCode::Internal,
    };

    RpcError {
        code,
        reason: error.to_string(),
    }
}

fn daemon_status_label(status: DaemonStatus) -> &'static str {
    match status {
        DaemonStatus::Starting => "starting",
        DaemonStatus::Running => "running",
        DaemonStatus::Ready => "ready",
        DaemonStatus::Degraded => "degraded",
        DaemonStatus::Stopping => "stopping",
    }
}

fn storage_status_label(status: StorageStatus) -> &'static str {
    match status {
        StorageStatus::Ready => "ready",
        StorageStatus::Migrating => "migrating",
        StorageStatus::ReadOnly => "read_only",
        StorageStatus::Unavailable => "unavailable",
    }
}

fn job_kind_label(kind: &JobKind) -> &str {
    match kind {
        JobKind::Read => "read",
        JobKind::Mutate => "mutate",
        JobKind::Process => "process",
        JobKind::Maintenance => "maintenance",
        JobKind::Other { name } => name,
    }
}

fn job_status_label(status: JobStatus) -> &'static str {
    match status {
        JobStatus::Queued => "queued",
        JobStatus::Running => "running",
        JobStatus::Succeeded => "succeeded",
        JobStatus::Failed => "failed",
        JobStatus::Blocked => "blocked",
        JobStatus::Canceled => "canceled",
    }
}

fn cancellation_state_label(state: CancellationState) -> &'static str {
    match state {
        CancellationState::NotRequested => "not_requested",
        CancellationState::Requested => "requested",
        CancellationState::CooperativeStop => "cooperative_stop",
        CancellationState::ForceStop => "force_stop",
        CancellationState::Completed => "completed",
    }
}

fn job_cancellation_from_record(
    record: &JobRecord,
    requested_by: Option<&Actor>,
) -> JobCancellation {
    JobCancellation {
        state: cancellation_state_label(record.cancellation_state).to_string(),
        requested_by: requested_by.map(|actor| RpcActorDto::from(actor.clone())),
        reason: None,
        requested_at_unix_ms: None,
        completed_at_unix_ms: match record.cancellation_state {
            CancellationState::Completed => record.completed_at.map(|ts| ts.unix_millis),
            _ => None,
        },
    }
}

fn infer_repository_trust_state(allowed_scope: &Option<RpcPathScope>) -> RepositoryTrustState {
    // The current protocol does not carry explicit repository trust state; until
    // policy-based derivation exists, infer a safe default from the requested
    // scope and keep this logic centralized in the RPC boundary.
    match allowed_scope.as_ref().map(|scope| &scope.kind) {
        Some(RpcPathScopeKind::ReadOnly) => RepositoryTrustState::ReadOnly,
        _ => RepositoryTrustState::Trusted,
    }
}

fn job_cancellation_from_receipt(
    receipt: &CancelJobReceipt,
    requested_by: Option<&Actor>,
) -> JobCancellation {
    let requested_event = receipt
        .events
        .iter()
        .find(|event| matches!(event.kind, JobEventKind::CancellationRequested));

    JobCancellation {
        state: cancellation_state_label(receipt.job.cancellation_state).to_string(),
        requested_by: requested_by.map(|actor| RpcActorDto::from(actor.clone())),
        reason: requested_event.map(|event| event.public_message.clone()),
        requested_at_unix_ms: requested_event.map(|event| event.created_at.unix_millis),
        completed_at_unix_ms: receipt.job.completed_at.map(|ts| ts.unix_millis),
    }
}

fn policy_outcome_label(outcome: PolicyOutcome) -> &'static str {
    match outcome {
        PolicyOutcome::Allowed => "allowed",
        PolicyOutcome::Audited => "audited",
        PolicyOutcome::NeedsApproval => "needs_approval",
        PolicyOutcome::Blocked => "blocked",
    }
}

fn risk_tier_label(risk_tier: RiskTier) -> &'static str {
    match risk_tier {
        RiskTier::R0 => "r0",
        RiskTier::R1 => "r1",
        RiskTier::R2 => "r2",
        RiskTier::R3 => "r3",
        RiskTier::R4 => "r4",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelia_core::{
        BlockKey, BlockReason, DegradeBehavior, ExtensionCompatibility, ExtensionEntrypoints,
        ExtensionFailure, ExtensionKind, ExtensionManifest, ExtensionPermission,
        ExtensionPublisher, ExtensionRealm, ExtensionRuntime, ExtensionServices, ProvenanceSource,
        RetryPolicy, EXTENSION_MANIFEST_SCHEMA, EXTENSION_RPC_PROTOCOL,
    };
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn ready_server() -> SecretaryRpcServer {
        let mut service = SecretaryService::new();
        service.set_ready();
        SecretaryRpcServer::new(service)
    }

    fn actor() -> RpcActorDto {
        RpcActorDto::Agent {
            id: "agent:test".to_string(),
            display_name: Some("Test Agent".to_string()),
        }
    }

    fn tool_output_scope_workspace() -> RpcToolOutputScope {
        RpcToolOutputScope {
            level: RpcToolOutputScopeLevel::Workspace,
            tool_id: None,
        }
    }

    fn tool_output_scope_repository() -> RpcToolOutputScope {
        RpcToolOutputScope {
            level: RpcToolOutputScopeLevel::Repository {
                repository_id: RepositoryId::new().as_str().to_string(),
            },
            tool_id: None,
        }
    }

    fn tool_output_scope_project() -> RpcToolOutputScope {
        RpcToolOutputScope {
            level: RpcToolOutputScopeLevel::Project {
                project_id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
            },
            tool_id: None,
        }
    }

    fn extension_manifest(
        id: &str,
        version: &str,
        artifact_digest: &str,
        manifest_digest: &str,
    ) -> ExtensionManifest {
        let mut permissions = BTreeMap::new();
        permissions.insert(
            "service.review.comments".to_string(),
            ExtensionPermission {
                description: "allows review comment summaries".to_string(),
                risk_tier: Some("R2".to_string()),
            },
        );

        ExtensionManifest {
            schema: EXTENSION_MANIFEST_SCHEMA.to_string(),
            id: id.to_string(),
            name: "Test Extension".to_string(),
            version: version.to_string(),
            publisher: ExtensionPublisher {
                name: "Example Publisher".to_string(),
                url: Some("https://example.com".to_string()),
            },
            description: "A focused test extension".to_string(),
            types: vec![ExtensionKind::MemoryStrategy],
            compatibility: ExtensionCompatibility {
                atelia_protocol: ">=0.1 <0.3".to_string(),
                atelia_secretary: ">=0.1 <0.2".to_string(),
            },
            entrypoints: ExtensionEntrypoints {
                realm: ExtensionRealm::Backend,
                runtime: ExtensionRuntime::WasmRust,
                command: None,
                image: None,
                wasm: Some("extension.wasm".to_string()),
                protocol: EXTENSION_RPC_PROTOCOL.to_string(),
            },
            permissions,
            tools: Vec::new(),
            services: ExtensionServices::default(),
            failure: ExtensionFailure {
                degrade: DegradeBehavior::ReturnUnavailable,
                retry_policy: RetryPolicy::Bounded,
            },
            provenance: atelia_core::ExtensionProvenance {
                source: ProvenanceSource::Registry,
                repository: Some("https://github.com/example/extensions".to_string()),
                commit: Some("deadbeef".to_string()),
                registry_identity: Some("third-party-registry".to_string()),
                artifact_digest: artifact_digest.to_string(),
                manifest_digest: manifest_digest.to_string(),
                signature: Some("signature".to_string()),
                signer: Some("signer@example.com".to_string()),
            },
            bundle: None,
        }
    }

    fn actor_record() -> Actor {
        Actor::Agent {
            id: "agent:test".to_string(),
            display_name: Some("Test Agent".to_string()),
        }
    }

    fn repository_record(
        root_path: &str,
        trust_state: RepositoryTrustState,
        allowed_paths: Vec<&str>,
    ) -> RepositoryRecord {
        let record = RepositoryRecord::new(
            "repo-scope-test",
            root_path,
            trust_state,
            atelia_core::LedgerTimestamp::from_unix_millis(1_000),
        );
        RepositoryRecord {
            allowed_path_scope: PathScope {
                root_path: root_path.to_string(),
                allowed_paths: allowed_paths
                    .into_iter()
                    .map(std::string::ToString::to_string)
                    .collect(),
            },
            ..record
        }
    }

    fn test_repo_dir(name: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "atelia-rpc-test-{}-{}-{name}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    #[test]
    fn transport_blocker_is_explicit_while_tonic_is_absent() {
        let server = ready_server();
        assert_eq!(server.transport_blocker(), Some(TRANSPORT_BLOCKER));
    }

    #[test]
    fn health_exposes_daemon_service_capabilities() {
        let server = ready_server();

        let response = server.health(HealthRequest);

        assert_eq!(response.status, "ready");
        assert!(response.capabilities.contains(&"health.v1".to_string()));
        assert!(response
            .capabilities
            .contains(&"repositories.v1".to_string()));
        assert!(response.capabilities.contains(&"jobs.v1".to_string()));
        assert!(response.capabilities.contains(&"policy.v1".to_string()));
    }

    #[test]
    fn health_status_reflects_storage_failure_non_ready_states() {
        let unavailable = HealthResponse::from(DaemonHealth {
            daemon_status: DaemonStatus::Ready,
            storage_status: StorageStatus::Unavailable,
            daemon_version: "daemon".to_string(),
            protocol_version: "protocol".to_string(),
            storage_version: "storage".to_string(),
            capabilities: Vec::new(),
            repository_count: 0,
            started_at: atelia_core::LedgerTimestamp::from_unix_millis(0),
        });
        assert_eq!(unavailable.status, "unavailable");

        let read_only = HealthResponse::from(DaemonHealth {
            daemon_status: DaemonStatus::Ready,
            storage_status: StorageStatus::ReadOnly,
            daemon_version: "daemon".to_string(),
            protocol_version: "protocol".to_string(),
            storage_version: "storage".to_string(),
            capabilities: Vec::new(),
            repository_count: 0,
            started_at: atelia_core::LedgerTimestamp::from_unix_millis(0),
        });
        assert_eq!(read_only.status, "degraded");
    }

    #[test]
    fn health_includes_tool_output_settings_capability() {
        let server = ready_server();
        let response = server.health(HealthRequest);
        assert!(response
            .capabilities
            .contains(&"tool_output_settings.v1".to_string()));
    }

    #[test]
    fn tool_output_defaults_exposed_via_scope_round_trip() {
        let server = ready_server();
        let response = server
            .get_tool_output_defaults(GetToolOutputDefaultsRequest {
                scope: tool_output_scope_workspace(),
            })
            .expect("workspace defaults lookup should succeed");
        let baseline = atelia_core::ToolOutputDefaults::default();

        assert_eq!(
            response.scope,
            RpcToolOutputScope {
                level: RpcToolOutputScopeLevel::Workspace,
                tool_id: None
            }
        );
        assert_eq!(
            response.defaults.max_inline_lines,
            baseline.max_inline_lines
        );
        assert_eq!(
            response.defaults.max_inline_bytes,
            baseline.max_inline_bytes
        );
    }

    #[test]
    fn tool_output_defaults_return_canonical_scope() {
        let server = ready_server();
        let response = server
            .get_tool_output_defaults(GetToolOutputDefaultsRequest {
                scope: RpcToolOutputScope {
                    level: RpcToolOutputScopeLevel::Workspace,
                    tool_id: Some("  tool.fs-read  ".to_string()),
                },
            })
            .expect("tool output defaults lookup should succeed");

        assert_eq!(
            response.scope,
            RpcToolOutputScope {
                level: RpcToolOutputScopeLevel::Workspace,
                tool_id: Some("tool.fs-read".to_string()),
            }
        );
    }

    #[test]
    fn tool_output_project_scope_round_trips_and_preserved_in_history() {
        let server = ready_server();
        let scope = tool_output_scope_project();
        let change = server
            .update_tool_output_defaults(UpdateToolOutputDefaultsRequest {
                scope: scope.clone(),
                actor: actor(),
                reason: "Project-level override for tests".to_string(),
                overrides: RpcToolOutputOverrides {
                    max_inline_lines: Some(222),
                    ..Default::default()
                },
            })
            .expect("project update should succeed");

        let defaults = server
            .get_tool_output_defaults(GetToolOutputDefaultsRequest { scope })
            .expect("project defaults lookup should succeed");

        let history = server
            .list_tool_output_settings_history(ListToolOutputSettingsHistoryRequest {
                scope: Some(RpcToolOutputScope {
                    level: RpcToolOutputScopeLevel::Project {
                        project_id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
                    },
                    tool_id: None,
                }),
                limit: None,
                offset: None,
                cursor: None,
            })
            .expect("history should return project-scoped entries");

        let expected_scope = RpcToolOutputScope {
            level: RpcToolOutputScopeLevel::Project {
                project_id: "123e4567-e89b-12d3-a456-426614174000".to_string(),
            },
            tool_id: None,
        };

        assert_eq!(change.change.scope, expected_scope);
        assert_eq!(defaults.scope, expected_scope);
        assert_eq!(history.changes.len(), 1);
        assert_eq!(history.changes[0].scope, expected_scope);
        assert_eq!(defaults.defaults.max_inline_lines, 222);
    }

    #[test]
    fn update_tool_output_defaults_records_audit_and_updates_lookup() {
        let server = ready_server();
        let change = server
            .update_tool_output_defaults(UpdateToolOutputDefaultsRequest {
                scope: RpcToolOutputScope {
                    level: RpcToolOutputScopeLevel::Session {
                        session_id: "session-1".to_string(),
                    },
                    tool_id: Some("tool.fs-read".to_string()),
                },
                actor: actor(),
                reason: "Long output trim for inspection mode".to_string(),
                overrides: RpcToolOutputOverrides {
                    max_inline_lines: Some(120),
                    format: Some(RpcOutputFormat::Json),
                    ..Default::default()
                },
            })
            .expect("tool output defaults update should succeed");

        let defaults = server
            .get_tool_output_defaults(GetToolOutputDefaultsRequest {
                scope: RpcToolOutputScope {
                    level: RpcToolOutputScopeLevel::Session {
                        session_id: "session-1".to_string(),
                    },
                    tool_id: Some("tool.fs-read".to_string()),
                },
            })
            .expect("tool output defaults lookup should succeed");

        let history = server
            .list_tool_output_settings_history(ListToolOutputSettingsHistoryRequest {
                scope: Some(RpcToolOutputScope {
                    level: RpcToolOutputScopeLevel::Session {
                        session_id: "session-1".to_string(),
                    },
                    tool_id: Some("tool.fs-read".to_string()),
                }),
                limit: None,
                offset: None,
                cursor: None,
            })
            .expect("settings history should return filterable list");

        assert_eq!(change.change.new_defaults.max_inline_lines, 120);
        assert_eq!(
            change.change.new_defaults.render_options.format,
            RpcOutputFormat::Json
        );
        assert_eq!(change.change.actor, actor());
        assert_eq!(
            defaults.scope,
            RpcToolOutputScope {
                level: RpcToolOutputScopeLevel::Session {
                    session_id: "session-1".to_string(),
                },
                tool_id: Some("tool.fs-read".to_string()),
            }
        );
        assert_eq!(defaults.defaults.max_inline_lines, 120);
        assert_eq!(history.changes.len(), 1);
        assert_eq!(history.changes[0].actor, actor());
    }

    #[test]
    fn list_tool_output_settings_history_forwards_pagination_and_token() {
        let server = ready_server();
        for i in 0..3 {
            server
                .update_tool_output_defaults(UpdateToolOutputDefaultsRequest {
                    scope: RpcToolOutputScope {
                        level: RpcToolOutputScopeLevel::Workspace,
                        tool_id: None,
                    },
                    actor: actor(),
                    reason: format!("history-page {i}"),
                    overrides: RpcToolOutputOverrides {
                        max_inline_lines: Some(300 + i),
                        ..Default::default()
                    },
                })
                .expect("tool output defaults update should succeed");
        }

        let first = server
            .list_tool_output_settings_history(ListToolOutputSettingsHistoryRequest {
                scope: None,
                limit: Some(2),
                offset: None,
                cursor: None,
            })
            .expect("first history page should succeed");
        assert_eq!(first.changes.len(), 2);
        assert_eq!(first.next_page_token, Some("2".to_string()));

        let second = server
            .list_tool_output_settings_history(ListToolOutputSettingsHistoryRequest {
                scope: None,
                limit: Some(2),
                offset: None,
                cursor: first.next_page_token,
            })
            .expect("second history page should succeed");
        assert_eq!(second.changes.len(), 1);
        assert_eq!(second.next_page_token, None);
    }

    #[test]
    fn update_tool_output_defaults_rejects_missing_reason_or_invalid_scope() {
        let server = ready_server();

        let empty_reason = server
            .update_tool_output_defaults(UpdateToolOutputDefaultsRequest {
                scope: tool_output_scope_workspace(),
                actor: actor(),
                reason: "  ".to_string(),
                overrides: RpcToolOutputOverrides {
                    max_inline_lines: Some(220),
                    ..Default::default()
                },
            })
            .unwrap_err();

        assert_eq!(empty_reason.code, RpcErrorCode::InvalidArgument);

        let invalid_scope = server
            .get_tool_output_defaults(GetToolOutputDefaultsRequest {
                scope: RpcToolOutputScope {
                    level: RpcToolOutputScopeLevel::Repository {
                        repository_id: "not-a-repo-id".to_string(),
                    },
                    tool_id: None,
                },
            })
            .unwrap_err();
        assert_eq!(invalid_scope.code, RpcErrorCode::InvalidArgument);
    }

    #[test]
    fn dto_conversion_roundtrip_for_actor_and_trust_state() {
        let rpc_trust_state = RpcRepositoryTrustState::Trusted;
        let domain_state = RepositoryTrustState::try_from(rpc_trust_state.clone())
            .expect("trusted trust state maps to core enum");
        let rpc_round_trip = RpcRepositoryTrustState::from(domain_state);

        assert_eq!(rpc_round_trip, rpc_trust_state);

        let rpc_actor = actor();
        let domain_actor = Actor::try_from(rpc_actor.clone()).expect("actor maps to core enum");
        let actor_round_trip = RpcActorDto::from(domain_actor);

        assert_eq!(actor_round_trip, rpc_actor);
    }

    #[test]
    fn register_list_submit_and_get_job_round_trip() {
        let server = ready_server();
        let root = test_repo_dir("round-trip");

        let registered = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "rpc-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let repositories = server
            .list_repositories(ListRepositoriesRequest {
                trust_state: None,
                page_size: None,
                page_token: None,
            })
            .expect("list repositories should succeed");
        assert_eq!(repositories.repositories.len(), 1);
        assert_eq!(
            repositories.repositories[0].repository_id,
            registered.repository.repository_id
        );

        let submitted = server
            .submit_job(SubmitJobRequest {
                repository_id: registered.repository.repository_id.clone(),
                requester: actor(),
                kind: "read".to_string(),
                goal: "summarize repository state".to_string(),
                path_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .expect("submit job should succeed");
        assert_eq!(submitted.job.status, "succeeded");
        assert_eq!(submitted.policy.outcome, "allowed");

        let fetched = server
            .get_job(GetJobRequest {
                job_id: submitted.job.job_id.clone(),
            })
            .expect("get job should succeed");
        assert_eq!(fetched.job.job_id, submitted.job.job_id);

        let jobs = server
            .list_jobs(ListJobsRequest {
                repository_id: Some(registered.repository.repository_id),
                status: Some(RpcJobStatus::Succeeded),
                requester: None,
                page_size: None,
                page_token: None,
            })
            .expect("list jobs should succeed");
        assert_eq!(jobs.jobs.len(), 1);
        assert_eq!(jobs.jobs[0].job_id, submitted.job.job_id);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn register_repository_response_policy_is_not_an_authz_result() {
        let server = ready_server();
        let root = test_repo_dir("register-policy-null");

        let registered = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "rpc-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        assert!(registered.policy.is_none());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn check_policy_returns_preview_decision() {
        let server = ready_server();
        let root = test_repo_dir("check-policy-success");

        let registered = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "check-policy-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let decision_response = server
            .check_policy(CheckPolicyRequest {
                repository_id: registered.repository.repository_id,
                requester: actor(),
                requested_capability: "filesystem.read".to_string(),
                action: "inspect current files".to_string(),
                path_scope: None,
                tool_result: None,
            })
            .expect("check policy should succeed");

        assert_eq!(decision_response.decision.outcome, "allowed");
        assert_eq!(
            decision_response.decision.requested_capability,
            "filesystem.read"
        );
        assert_eq!(decision_response.decision.approval_request_ref, None);
        assert_eq!(decision_response.decision.audit_ref, None);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn check_policy_rejects_invalid_capability() {
        let server = ready_server();
        let root = test_repo_dir("check-policy-invalid-capability");

        let registered = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "check-policy-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let error = server
            .check_policy(CheckPolicyRequest {
                repository_id: registered.repository.repository_id,
                requester: actor(),
                requested_capability: "   ".to_string(),
                action: "inspect".to_string(),
                path_scope: None,
                tool_result: None,
            })
            .unwrap_err();

        assert_eq!(error.code, RpcErrorCode::InvalidArgument);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn check_policy_rejects_tool_result_ref_if_provided() {
        let server = ready_server();
        let root = test_repo_dir("check-policy-empty-tool-result");

        let registered = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "check-policy-tool-ref-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let error = server
            .check_policy(CheckPolicyRequest {
                repository_id: registered.repository.repository_id,
                requester: actor(),
                requested_capability: "filesystem.read".to_string(),
                action: "inspect".to_string(),
                path_scope: None,
                tool_result: Some(ToolResultRef {
                    tool_result_id: "tool_result_id".to_string(),
                    tool_invocation_id: "tool_invocation_id".to_string(),
                    job_id: "job_id".to_string(),
                    repository_id: "repository_id".to_string(),
                    content_type: "application/json".to_string(),
                }),
            })
            .unwrap_err();

        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn register_repository_defaults_to_trusted_without_allowed_scope() {
        let server = ready_server();
        let root = test_repo_dir("trusted-default");

        let registered = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "rpc-trust-default".to_string(),
                root_path: root.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        assert_eq!(
            registered.repository.trust_state,
            RpcRepositoryTrustState::Trusted
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn register_repository_infers_read_only_from_allowed_scope() {
        let server = ready_server();
        let root = test_repo_dir("readonly-by-scope");

        let registered = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "rpc-trust-readonly".to_string(),
                root_path: root.to_string_lossy().to_string(),
                allowed_scope: Some(RpcPathScope {
                    kind: RpcPathScopeKind::ReadOnly,
                    roots: vec![".".to_string()],
                    include_patterns: Vec::new(),
                    exclude_patterns: Vec::new(),
                }),
                requester: None,
            })
            .expect("register should succeed");

        assert_eq!(
            registered.repository.trust_state,
            RpcRepositoryTrustState::ReadOnly
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rpc_path_scope_from_record_maps_read_only_from_repository_trust_state() {
        let root = test_repo_dir("scope-kind-readonly");
        let record = repository_record(
            root.to_string_lossy().as_ref(),
            RepositoryTrustState::ReadOnly,
            vec!["."],
        );
        let repository = Repository::from(record);

        assert_eq!(repository.allowed_scope.kind, RpcPathScopeKind::ReadOnly);
        assert_eq!(repository.allowed_scope.roots, vec![".".to_string()]);
        assert!(repository.allowed_scope.include_patterns.is_empty());
        assert!(repository.allowed_scope.exclude_patterns.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rpc_path_scope_from_record_preserves_roots_and_infers_kind() {
        let root = test_repo_dir("scope-kind-explicit");
        let repository_root = root.to_string_lossy();

        let repository = Repository::from(repository_record(
            repository_root.as_ref(),
            RepositoryTrustState::Trusted,
            vec![repository_root.as_ref()],
        ));
        assert_eq!(repository.allowed_scope.kind, RpcPathScopeKind::Repository);
        assert_eq!(
            repository.allowed_scope.roots,
            vec![repository_root.as_ref().to_string()]
        );

        let explicit = Repository::from(repository_record(
            repository_root.as_ref(),
            RepositoryTrustState::Trusted,
            vec!["subdir", "other"],
        ));
        assert_eq!(explicit.allowed_scope.kind, RpcPathScopeKind::ExplicitPaths);
        assert_eq!(
            explicit.allowed_scope.roots,
            vec!["subdir".to_string(), "other".to_string()]
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_repositories_request_forwards_filters_and_pagination() {
        let server = ready_server();
        let root_a = test_repo_dir("list-filter-a");
        let root_b = test_repo_dir("list-filter-b");
        let root_c = test_repo_dir("list-filter-c");

        server
            .register_repository(RegisterRepositoryRequest {
                display_name: "repo-a".to_string(),
                root_path: root_a.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register trusted should succeed");
        server
            .register_repository(RegisterRepositoryRequest {
                display_name: "repo-b".to_string(),
                root_path: root_b.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register read-only should succeed");
        server
            .register_repository(RegisterRepositoryRequest {
                display_name: "repo-c".to_string(),
                root_path: root_c.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register trusted should succeed");

        let first = server
            .list_repositories(ListRepositoriesRequest {
                trust_state: Some(RpcRepositoryTrustState::Trusted),
                page_size: Some(1),
                page_token: None,
            })
            .expect("list repositories should succeed");
        assert_eq!(first.repositories.len(), 1);
        assert_eq!(
            first.repositories[0].trust_state,
            RpcRepositoryTrustState::Trusted
        );
        assert_eq!(first.next_page_token, Some("1".to_string()));

        let second = server
            .list_repositories(ListRepositoriesRequest {
                trust_state: Some(RpcRepositoryTrustState::Trusted),
                page_size: Some(1),
                page_token: first.next_page_token,
            })
            .expect("list repositories should succeed");
        assert_eq!(second.repositories.len(), 1);
        assert_eq!(
            second.repositories[0].trust_state,
            RpcRepositoryTrustState::Trusted
        );
        assert_ne!(
            first.repositories[0].repository_id,
            second.repositories[0].repository_id
        );

        let _ = fs::remove_dir_all(root_a);
        let _ = fs::remove_dir_all(root_b);
        let _ = fs::remove_dir_all(root_c);
    }

    #[test]
    fn list_repositories_request_rejects_invalid_page_token() {
        let server = ready_server();

        let error = server
            .list_repositories(ListRepositoriesRequest {
                trust_state: None,
                page_size: None,
                page_token: Some("not-a-number".to_string()),
            })
            .unwrap_err();

        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    }

    #[test]
    fn list_jobs_request_forwards_pagination() {
        let server = ready_server();
        let root = test_repo_dir("pagination");

        let registered = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "pagination-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        server
            .submit_job(SubmitJobRequest {
                repository_id: registered.repository.repository_id.clone(),
                requester: actor(),
                kind: "read".to_string(),
                goal: "one".to_string(),
                path_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .expect("submit job should succeed");
        server
            .submit_job(SubmitJobRequest {
                repository_id: registered.repository.repository_id.clone(),
                requester: actor(),
                kind: "read".to_string(),
                goal: "two".to_string(),
                path_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .expect("submit job should succeed");

        let first = server
            .list_jobs(ListJobsRequest {
                repository_id: Some(registered.repository.repository_id.clone()),
                status: Some(RpcJobStatus::Succeeded),
                requester: None,
                page_size: Some(1),
                page_token: None,
            })
            .expect("list jobs should succeed");
        assert_eq!(first.jobs.len(), 1);
        assert!(first.next_page_token.is_some());

        let second = server
            .list_jobs(ListJobsRequest {
                repository_id: Some(registered.repository.repository_id),
                status: Some(RpcJobStatus::Succeeded),
                requester: None,
                page_size: Some(1),
                page_token: first.next_page_token,
            })
            .expect("list jobs should succeed");
        assert_eq!(second.jobs.len(), 1);
        assert_ne!(second.jobs[0].job_id, first.jobs[0].job_id);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cancel_terminal_job_maps_to_conflict() {
        let server = ready_server();
        let root = test_repo_dir("cancel-terminal");
        let registered = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "cancel-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        let submitted = server
            .submit_job(SubmitJobRequest {
                repository_id: registered.repository.repository_id,
                requester: actor(),
                kind: "read".to_string(),
                goal: "finish immediately".to_string(),
                path_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .expect("submit job should succeed");

        let error = server
            .cancel_job(CancelJobRequest {
                job_id: submitted.job.job_id,
                requester: actor(),
                reason: "too late".to_string(),
            })
            .unwrap_err();

        assert_eq!(error.code, RpcErrorCode::Conflict);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_repository_id_maps_to_invalid_argument() {
        let server = ready_server();

        let error = server
            .submit_job(SubmitJobRequest {
                repository_id: "not-a-repo-id".to_string(),
                requester: actor(),
                kind: "read".to_string(),
                goal: "test".to_string(),
                path_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .unwrap_err();

        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
    }

    #[test]
    fn submit_job_rejects_unsupported_path_scope_patterns() {
        let server = ready_server();
        let root = test_repo_dir("unsupported-path-scope");

        let registered = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "scope-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let error = server
            .submit_job(SubmitJobRequest {
                repository_id: registered.repository.repository_id,
                requester: actor(),
                kind: "read".to_string(),
                goal: "ignored".to_string(),
                path_scope: Some(RpcPathScope {
                    kind: RpcPathScopeKind::Repository,
                    roots: vec![".".to_string()],
                    include_patterns: vec!["src/**/*.rs".to_string()],
                    exclude_patterns: Vec::new(),
                }),
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .unwrap_err();

        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_requested_capabilities_and_idempotency_key() {
        let server = ready_server();
        let root = test_repo_dir("unsupported-submission-fields");
        let registered = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "feature-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let no_capabilities = server
            .submit_job(SubmitJobRequest {
                repository_id: registered.repository.repository_id.clone(),
                requester: actor(),
                kind: "read".to_string(),
                goal: "first".to_string(),
                path_scope: None,
                requested_capabilities: vec!["capability.discovery".to_string()],
                idempotency_key: None,
            })
            .unwrap_err();
        assert_eq!(no_capabilities.code, RpcErrorCode::InvalidArgument);

        let with_idempotency = server
            .submit_job(SubmitJobRequest {
                repository_id: registered.repository.repository_id.clone(),
                requester: actor(),
                kind: "read".to_string(),
                goal: "second".to_string(),
                path_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("request-key".to_string()),
            })
            .expect("submit job should succeed");
        let replayed = server
            .submit_job(SubmitJobRequest {
                repository_id: registered.repository.repository_id.clone(),
                requester: actor(),
                kind: "read".to_string(),
                goal: "second".to_string(),
                path_scope: None,
                idempotency_key: Some("request-key".to_string()),
                requested_capabilities: Vec::new(),
            })
            .expect("replay should return same job");
        assert_eq!(with_idempotency.job.job_id, replayed.job.job_id);

        let conflicting = server
            .submit_job(SubmitJobRequest {
                repository_id: registered.repository.repository_id,
                requester: actor(),
                kind: "read".to_string(),
                goal: "conflicting goal".to_string(),
                path_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("request-key".to_string()),
            })
            .unwrap_err();
        assert_eq!(conflicting.code, RpcErrorCode::Conflict);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn duplicate_repository_maps_to_conflict() {
        let server = ready_server();
        let root = test_repo_dir("duplicate-conflict");

        server
            .register_repository(RegisterRepositoryRequest {
                display_name: "first-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let error = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "second-repo".to_string(),
                root_path: root.join(".").to_string_lossy().to_string(),
                allowed_scope: None,
                requester: None,
            })
            .unwrap_err();

        assert_eq!(error.code, RpcErrorCode::Conflict);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn job_dto_maps_absent_timestamps_and_event_id_to_none() {
        let record = JobRecord::new(
            actor_record(),
            RepositoryId::new(),
            JobKind::Read,
            "dry run".to_string(),
            atelia_core::LedgerTimestamp::from_unix_millis(1_000),
        );

        let dto = Job::from(record);

        assert_eq!(dto.requester, actor());
        assert_eq!(dto.started_at_unix_ms, None);
        assert_eq!(dto.completed_at_unix_ms, None);
        assert_eq!(dto.latest_event_id, None);
        assert_eq!(dto.cancellation.state, "not_requested");
        assert_eq!(dto.cancellation.requested_by, None);
        assert_eq!(dto.cancellation.reason, None);
        assert_eq!(dto.cancellation.requested_at_unix_ms, None);
        assert_eq!(dto.cancellation.completed_at_unix_ms, None);
    }

    #[test]
    fn job_dto_maps_present_timestamps_and_event_id_to_some() {
        let started = atelia_core::LedgerTimestamp::from_unix_millis(1_100);
        let completed = atelia_core::LedgerTimestamp::from_unix_millis(1_200);
        let event_id = atelia_core::JobEventId::new();
        let event_id_str = event_id.as_str().to_string();
        let decision_id = atelia_core::PolicyDecisionId::new();
        let decision_id_str = decision_id.as_str().to_string();

        let mut record = JobRecord::new(
            actor_record(),
            RepositoryId::new(),
            JobKind::Read,
            "dry run".to_string(),
            atelia_core::LedgerTimestamp::from_unix_millis(1_000),
        );
        record.started_at = Some(started);
        record.completed_at = Some(completed);
        record.latest_event_id = Some(event_id);
        record.policy_summary = Some(atelia_core::PolicySummary {
            decision_id: Some(decision_id),
            outcome: PolicyOutcome::Allowed,
            risk_tier: RiskTier::R1,
            reason_code: "policy-checked".to_string(),
        });

        let dto = Job::from(record);

        assert_eq!(dto.started_at_unix_ms, Some(started.unix_millis));
        assert_eq!(dto.completed_at_unix_ms, Some(completed.unix_millis));
        assert_eq!(dto.latest_event_id, Some(event_id_str));
        assert_eq!(
            dto.policy_summary
                .expect("policy summary should be present")
                .decision_id,
            Some(decision_id_str)
        );

        assert_eq!(dto.requester, actor());
        assert_eq!(dto.cancellation.state, "not_requested");
        assert_eq!(dto.cancellation.requested_by, None);
        assert_eq!(dto.cancellation.reason, None);
        assert_eq!(dto.cancellation.requested_at_unix_ms, None);
        assert_eq!(dto.cancellation.completed_at_unix_ms, None);
    }

    #[test]
    fn job_dto_maps_cancellation_state_to_structured_cancellation() {
        let mut record = JobRecord::new(
            actor_record(),
            RepositoryId::new(),
            JobKind::Read,
            "dry run".to_string(),
            atelia_core::LedgerTimestamp::from_unix_millis(1_000),
        );
        record.cancellation_state = CancellationState::CooperativeStop;
        let dto = Job::from(record);
        assert_eq!(dto.cancellation.state, "cooperative_stop");
        assert_eq!(dto.cancellation.requested_by, None);
        assert_eq!(dto.cancellation.reason, None);
        assert_eq!(dto.cancellation.requested_at_unix_ms, None);
        assert_eq!(dto.cancellation.completed_at_unix_ms, None);
    }

    #[test]
    fn cancellation_dto_from_receipt_maps_reason_and_requested_at() {
        let timestamp = atelia_core::LedgerTimestamp::from_unix_millis(5_000);
        let completion_timestamp = atelia_core::LedgerTimestamp::from_unix_millis(5_123);
        let event = atelia_core::JobEvent {
            id: atelia_core::JobEventId::new(),
            schema_version: 1,
            sequence_number: 1,
            created_at: timestamp,
            subject: atelia_core::EventSubject::job(&atelia_core::JobId::new()),
            kind: JobEventKind::CancellationRequested,
            severity: atelia_core::EventSeverity::Warning,
            public_message: "operator requested cancellation".to_string(),
            refs: atelia_core::EventRefs::default(),
            redactions: Vec::new(),
        };
        let job = JobRecord::new(
            actor_record(),
            RepositoryId::new(),
            JobKind::Read,
            "dry run".to_string(),
            atelia_core::LedgerTimestamp::from_unix_millis(4_900),
        );
        let receipt = CancelJobReceipt {
            job: JobRecord {
                completed_at: Some(completion_timestamp),
                cancellation_state: CancellationState::Requested,
                ..job
            },
            events: vec![event],
        };

        let cancellation = job_cancellation_from_receipt(&receipt, None);

        assert_eq!(cancellation.state, "requested");
        assert_eq!(
            cancellation.reason,
            Some("operator requested cancellation".to_string())
        );
        assert_eq!(
            cancellation.requested_at_unix_ms,
            Some(timestamp.unix_millis)
        );
        assert_eq!(
            cancellation.completed_at_unix_ms,
            Some(completion_timestamp.unix_millis)
        );
    }

    #[test]
    fn cancellation_dto_from_receipt_preserves_requester() {
        let requested_by = actor_record();
        let timestamp = atelia_core::LedgerTimestamp::from_unix_millis(8_000);
        let receipt = CancelJobReceipt {
            job: JobRecord::new(
                requested_by.clone(),
                RepositoryId::new(),
                JobKind::Read,
                "cleanup".to_string(),
                atelia_core::LedgerTimestamp::from_unix_millis(7_000),
            ),
            events: vec![atelia_core::JobEvent {
                id: atelia_core::JobEventId::new(),
                schema_version: 1,
                sequence_number: 1,
                created_at: timestamp,
                subject: atelia_core::EventSubject::job(&JobId::new()),
                kind: JobEventKind::CancellationRequested,
                severity: atelia_core::EventSeverity::Warning,
                public_message: "operator requested".to_string(),
                refs: atelia_core::EventRefs::default(),
                redactions: Vec::new(),
            }],
        };

        let cancellation = job_cancellation_from_receipt(&receipt, Some(&requested_by));

        assert_eq!(
            cancellation.requested_by,
            Some(RpcActorDto::from(requested_by))
        );
    }

    #[test]
    fn cancel_job_response_payload_matches_receipt_cancellation() {
        let requested_by = actor_record();
        let requested_at = atelia_core::LedgerTimestamp::from_unix_millis(9_000);
        let completed_at = atelia_core::LedgerTimestamp::from_unix_millis(9_500);
        let receipt = CancelJobReceipt {
            job: JobRecord {
                cancellation_state: CancellationState::Requested,
                completed_at: Some(completed_at),
                ..JobRecord::new(
                    requested_by.clone(),
                    RepositoryId::new(),
                    JobKind::Read,
                    "cleanup".to_string(),
                    atelia_core::LedgerTimestamp::from_unix_millis(8_500),
                )
            },
            events: vec![atelia_core::JobEvent {
                id: atelia_core::JobEventId::new(),
                schema_version: 1,
                sequence_number: 1,
                created_at: requested_at,
                subject: atelia_core::EventSubject::job(&JobId::new()),
                kind: JobEventKind::CancellationRequested,
                severity: atelia_core::EventSeverity::Warning,
                public_message: "operator requested cancellation".to_string(),
                refs: atelia_core::EventRefs::default(),
                redactions: Vec::new(),
            }],
        };

        let expected = job_cancellation_from_receipt(&receipt, Some(&requested_by));
        let (job, cancellation) = job_from_cancel_receipt(receipt, Some(&requested_by));

        assert_eq!(job.cancellation, cancellation);
        assert_eq!(job.cancellation, expected);
        assert_eq!(cancellation.state, "requested");
        assert_eq!(
            cancellation.requested_by,
            Some(RpcActorDto::from(requested_by))
        );
        assert_eq!(
            cancellation.reason,
            Some("operator requested cancellation".to_string())
        );
        assert_eq!(
            cancellation.requested_at_unix_ms,
            Some(requested_at.unix_millis)
        );
        assert_eq!(
            cancellation.completed_at_unix_ms,
            Some(completed_at.unix_millis)
        );
    }

    #[test]
    fn extension_registry_round_trip_through_rpc_surface() {
        const ARTIFACT_V1: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const MANIFEST_V1: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        const ARTIFACT_V2: &str =
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        const MANIFEST_V2: &str =
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

        let server = ready_server();
        let manifest_v1 = extension_manifest(
            "com.example.review.extension",
            "1.0.0",
            ARTIFACT_V1,
            MANIFEST_V1,
        );
        let manifest_v2 = extension_manifest(
            "com.example.review.extension",
            "2.0.0",
            ARTIFACT_V2,
            MANIFEST_V2,
        );

        let install = server
            .install_extension(InstallExtensionRequest {
                manifest: manifest_v1,
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
            })
            .expect("install should succeed");
        assert_eq!(install.record.version, "1.0.0");

        server
            .install_extension(InstallExtensionRequest {
                manifest: manifest_v2,
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
            })
            .expect("update should succeed");

        let status = server
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect("status should succeed");
        assert_eq!(
            status.extension.extension_id,
            "com.example.review.extension"
        );
        assert_eq!(status.extension.record.as_ref().unwrap().version, "2.0.0");

        let list = server
            .list_extensions(ListExtensionsRequest {
                include_blocked: true,
            })
            .expect("list should succeed");
        assert_eq!(list.extensions.len(), 1);

        let rolled_back = server
            .rollback_extension(RollbackExtensionRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect("rollback should succeed");
        assert_eq!(rolled_back.record.version, "1.0.0");

        server
            .apply_blocklist(ApplyBlocklistRequest {
                entry: BlocklistEntry {
                    key: BlockKey::ExtensionId("com.example.review.extension".to_string()),
                    reason: BlockReason::UserBlocked,
                    note: Some("policy review".to_string()),
                },
            })
            .expect("apply blocklist should succeed");

        let blocked_status = server
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect("blocked status should succeed");
        assert!(blocked_status.extension.block.is_some());
        assert_eq!(
            blocked_status.extension.record.as_ref().unwrap().status,
            atelia_core::ExtensionInstallStatus::Blocked
        );

        let blocklist = server
            .list_blocklist(ListBlocklistRequest {})
            .expect("list blocklist should succeed");
        assert_eq!(blocklist.entries.len(), 1);
    }

    #[test]
    fn missing_job_maps_to_not_found() {
        let server = ready_server();
        let missing = JobId::new();

        let error = server
            .get_job(GetJobRequest {
                job_id: missing.as_str().to_string(),
            })
            .unwrap_err();

        assert_eq!(error.code, RpcErrorCode::NotFound);
    }

    #[test]
    fn registry_service_denied_maps_to_invalid_argument() {
        let error = RpcError::from(ServiceError::ExtensionRegistry(
            atelia_core::RegistryError::ServiceDenied {
                reason: "caller did not declare services.consumes".to_string(),
            },
        ));

        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
        assert!(error.reason.contains("services.consumes"));
    }

    #[test]
    fn registry_service_unavailable_maps_to_not_found() {
        let error = RpcError::from(ServiceError::ExtensionRegistry(
            atelia_core::RegistryError::ServiceUnavailable {
                reason: "callee did not declare services.provides".to_string(),
            },
        ));

        assert_eq!(error.code, RpcErrorCode::NotFound);
        assert!(error.reason.contains("services.provides"));
    }
}
