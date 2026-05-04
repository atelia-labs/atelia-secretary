//! Daemon service skeleton for Atelia Secretary (Slice 4).
//!
//! Owns daemon health/status metadata, an in-memory job lifecycle runtime, and
//! exposes a synchronous service API for health checks, repository
//! registration/listing, and the first supported job lifecycle calls.

use atelia_core::{
    canonicalize_job_requested_capability, canonicalize_within_scope,
    render_tool_result_with_policy, Actor, ApplyBlocklistRequest, ApplyBlocklistResponse,
    CancelJobReceipt, DefaultPolicyEngine, DisableExtensionRequest, DisableExtensionResponse,
    EnableExtensionRequest, EnableExtensionResponse, EventCursor, EventPage, EventQuery,
    ExtensionRegistryService, ExtensionStatusRequest, ExtensionStatusResponse, InMemoryStore,
    InMemoryToolOutputSettingsService, InstallExtensionRequest, InstallExtensionResponse, JobEvent,
    JobId, JobKind, JobLifecycleService, JobPage, JobQuery, JobRecord, JobStatus, LedgerTimestamp,
    ListBlocklistRequest, ListBlocklistResponse, ListExtensionsRequest, ListExtensionsResponse,
    OutputFormat, PathScope, PolicyDecision, PolicyEngine, PolicyInput, RegistryError,
    RemoveExtensionRequest, RemoveExtensionResponse, RenderedToolOutput, RepositoryId,
    RepositoryRecord, RepositoryTrustState, ResourceScope, RollbackExtensionRequest,
    RollbackExtensionResponse, RuntimeError, RuntimeJobReceipt, RuntimeJobRequest, SecretaryStore,
    StoreError, ToolInvocationId, ToolOutputDefaults, ToolOutputOverrides,
    ToolOutputSettingsChange, ToolOutputSettingsError, ToolOutputSettingsScope, ToolResultId,
    TruncationMetadata, UpdateExtensionRequest, UpdateExtensionResponse,
};
use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "1.0.0";
const STORAGE_VERSION: &str = "0.1.0";
const DAEMON_CAPABILITIES: &[&str] = &[
    "health.v1",
    "repositories.v1",
    "jobs.v1",
    "events.v1",
    "policy.v1",
    "extensions.registry.v1",
    "tool_output_settings.v1",
    "tool_output_render.v1",
    "project_status.v1",
];
const MAX_HISTORY_PAGE: usize = 1000;
const SECRETARY_ECHO_TOOL_ID: &str = "secretary.echo";

fn daemon_capabilities() -> Vec<String> {
    DAEMON_CAPABILITIES
        .iter()
        .map(|capability| capability.to_string())
        .collect()
}
const PROJECT_STATUS_RECENT_LIMIT: usize = 5;

// ---------------------------------------------------------------------------
// Health types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum DaemonStatus {
    Starting,
    Running,
    Ready,
    Degraded,
    Stopping,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum StorageStatus {
    Ready,
    Migrating,
    ReadOnly,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonHealth {
    pub daemon_status: DaemonStatus,
    pub storage_status: StorageStatus,
    pub daemon_version: String,
    pub protocol_version: String,
    pub storage_version: String,
    pub capabilities: Vec<String>,
    pub repository_count: usize,
    pub started_at: LedgerTimestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolMetadata {
    pub protocol_version: String,
    pub daemon_version: String,
    pub storage_version: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GetProjectStatusRequest {
    pub repository_id: RepositoryId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectStatusSnapshot {
    pub repository: RepositoryRecord,
    pub recent_jobs: Vec<JobRecord>,
    pub recent_policy_decisions: Vec<PolicyDecision>,
    pub latest_event: Option<JobEvent>,
    pub daemon_status: DaemonStatus,
    pub storage_status: StorageStatus,
}

// ---------------------------------------------------------------------------
// Service errors
// ---------------------------------------------------------------------------

#[derive(Debug)]
#[allow(dead_code)]
pub enum ServiceError {
    Conflict { reason: String },
    Store(atelia_core::StoreError),
    Runtime(RuntimeError),
    ExtensionRegistry(RegistryError),
    Settings(ToolOutputSettingsError),
    InvalidArgument { reason: String },
    Internal { reason: String },
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { reason } => write!(f, "conflict: {reason}"),
            Self::Store(err) => write!(f, "{err}"),
            Self::Runtime(err) => write!(f, "{err}"),
            Self::ExtensionRegistry(err) => write!(f, "{err}"),
            Self::Settings(err) => write!(f, "tool output settings: {err}"),
            Self::InvalidArgument { reason } => write!(f, "invalid argument: {reason}"),
            Self::Internal { reason } => write!(f, "internal error: {reason}"),
        }
    }
}

impl std::error::Error for ServiceError {}

impl From<atelia_core::StoreError> for ServiceError {
    fn from(err: atelia_core::StoreError) -> Self {
        Self::Store(err)
    }
}

impl From<RuntimeError> for ServiceError {
    fn from(err: RuntimeError) -> Self {
        match err {
            RuntimeError::Store(err) => Self::Store(err),
            err => Self::Runtime(err),
        }
    }
}

impl From<RegistryError> for ServiceError {
    fn from(err: RegistryError) -> Self {
        Self::ExtensionRegistry(err)
    }
}

impl From<ToolOutputSettingsError> for ServiceError {
    fn from(err: ToolOutputSettingsError) -> Self {
        Self::Settings(err)
    }
}

#[allow(dead_code)]
pub type ServiceResult<T> = Result<T, ServiceError>;

// ---------------------------------------------------------------------------
// Register-request DTO
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RegisterRepositoryRequest {
    pub display_name: String,
    pub root_path: String,
    pub trust_state: RepositoryTrustState,
    pub allowed_scope: Option<PathScope>,
    pub requester: Option<Actor>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SubmitJobRequest {
    pub requester: Actor,
    pub repository_id: RepositoryId,
    pub kind: JobKind,
    pub goal: String,
    pub resource_scope: Option<ResourceScope>,
    pub requested_capabilities: Vec<String>,
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CheckPolicyRequest {
    pub repository_id: RepositoryId,
    pub requester: Actor,
    pub requested_capability: String,
    pub action: String,
    pub resource_scope: ResourceScope,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct RenderToolOutputRequest {
    pub tool_result_id: ToolResultId,
    pub repository_id: Option<RepositoryId>,
    pub format: OutputFormat,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderToolOutputResult {
    pub tool_result: CanonicalToolResultRef,
    pub rendered_output: RenderedToolOutput,
    pub truncation: Option<TruncationMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalToolResultRef {
    pub tool_result_id: ToolResultId,
    pub tool_invocation_id: ToolInvocationId,
    pub job_id: JobId,
    pub repository_id: RepositoryId,
    pub content_type: String,
}

#[derive(Debug, Clone, Default)]
pub struct ListRepositoriesRequest {
    pub trust_state: Option<RepositoryTrustState>,
    pub page_size: Option<usize>,
    pub page_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ListRepositoriesPage {
    pub repositories: Vec<RepositoryRecord>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ListToolOutputSettingsHistoryRequest {
    pub scope: Option<ToolOutputSettingsScope>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ListToolOutputSettingsHistoryPage {
    pub changes: Vec<ToolOutputSettingsChange>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone)]
struct IdempotentSubmitJob {
    signature: String,
    receipt: RuntimeJobReceipt,
}

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Thin service facade over an in-memory job lifecycle runtime.
///
/// All methods are synchronous because the underlying store and policy engine
/// are synchronous. The Tokio entrypoint in `main.rs` wraps this in an async
/// runtime for signal handling only.
pub struct SecretaryService {
    lifecycle: JobLifecycleService<InMemoryStore, DefaultPolicyEngine>,
    started_at: LedgerTimestamp,
    daemon_status: DaemonStatus,
    extension_registry: Mutex<ExtensionRegistryService>,
    tool_output_settings: Mutex<InMemoryToolOutputSettingsService>,
    idempotent_submissions: Mutex<HashMap<String, IdempotentSubmitJob>>,
    cancellation_requesters: Mutex<HashMap<JobId, Actor>>,
}

impl SecretaryService {
    /// Create a new service backed by an in-memory store and default policy.
    pub fn new() -> Self {
        Self {
            lifecycle: JobLifecycleService::in_memory(),
            started_at: LedgerTimestamp::now(),
            daemon_status: DaemonStatus::Starting,
            extension_registry: Mutex::new(ExtensionRegistryService::new()),
            tool_output_settings: Mutex::new(InMemoryToolOutputSettingsService::new(
                LedgerTimestamp::now(),
            )),
            idempotent_submissions: HashMap::new().into(),
            cancellation_requesters: HashMap::new().into(),
        }
    }

    fn lock_tool_output_settings(
        &self,
    ) -> ServiceResult<std::sync::MutexGuard<'_, InMemoryToolOutputSettingsService>> {
        self.tool_output_settings
            .lock()
            .map_err(|err| ServiceError::Internal {
                reason: format!("tool output settings lock poisoned: {err}"),
            })
    }

    fn lock_extension_registry(
        &self,
    ) -> ServiceResult<std::sync::MutexGuard<'_, ExtensionRegistryService>> {
        self.extension_registry
            .lock()
            .map_err(|err| ServiceError::Internal {
                reason: format!("extension registry lock poisoned: {err}"),
            })
    }

    /// Transition the daemon into [`DaemonStatus::Running`].
    pub fn set_running(&mut self) {
        self.daemon_status = DaemonStatus::Running;
    }

    /// Transition the daemon into [`DaemonStatus::Ready`].
    #[allow(dead_code)]
    pub fn set_ready(&mut self) {
        self.daemon_status = DaemonStatus::Ready;
    }

    /// Transition the daemon into [`DaemonStatus::Stopping`].
    pub fn set_stopping(&mut self) {
        self.daemon_status = DaemonStatus::Stopping;
    }

    /// Return the current daemon health snapshot.
    pub fn health(&self) -> DaemonHealth {
        let (repository_count, storage_status) =
            match self.lifecycle.runtime().store().list_repositories() {
                Ok(repos) => (repos.len(), StorageStatus::Ready),
                Err(err) => {
                    tracing::warn!("storage health check failed: {err}");
                    (0, StorageStatus::Unavailable)
                }
            };

        DaemonHealth {
            daemon_status: self.daemon_status,
            storage_status,
            daemon_version: DAEMON_VERSION.to_string(),
            protocol_version: PROTOCOL_VERSION.to_string(),
            storage_version: STORAGE_VERSION.to_string(),
            capabilities: daemon_capabilities(),
            repository_count,
            started_at: self.started_at,
        }
    }

    pub fn protocol_metadata(&self) -> ProtocolMetadata {
        ProtocolMetadata {
            protocol_version: PROTOCOL_VERSION.to_string(),
            daemon_version: DAEMON_VERSION.to_string(),
            storage_version: STORAGE_VERSION.to_string(),
            capabilities: daemon_capabilities(),
        }
    }

    /// Register a new repository and persist it in the store.
    #[allow(dead_code)]
    pub fn register_repository(
        &self,
        request: RegisterRepositoryRequest,
    ) -> ServiceResult<RepositoryRecord> {
        if request.display_name.trim().is_empty() {
            return Err(ServiceError::InvalidArgument {
                reason: "display_name must not be empty".to_string(),
            });
        }

        if request.root_path.trim().is_empty() {
            return Err(ServiceError::InvalidArgument {
                reason: "root_path must not be empty".to_string(),
            });
        }
        let root_path = canonical_repository_root(&request.root_path)?;

        let mut record = RepositoryRecord::new(
            request.display_name,
            root_path,
            request.trust_state,
            LedgerTimestamp::now(),
        );
        if let Some(mut requested_scope) = request.allowed_scope {
            if requested_scope.allowed_paths.is_empty() {
                requested_scope.allowed_paths = vec![record.root_path.clone()];
            }
            validate_repository_allowed_scope(&record.root_path, &requested_scope)?;
            requested_scope.root_path = record.root_path.clone();
            record.allowed_path_scope = requested_scope;
        }
        let _requester = request.requester;

        self.lifecycle
            .runtime()
            .store()
            .create_repository(record.clone())
            .map_err(|err| match err {
                StoreError::DuplicateId {
                    collection: "repositories",
                    ..
                } => ServiceError::Conflict {
                    reason: "root_path is already registered".to_string(),
                },
                StoreError::Conflict { .. } => ServiceError::Conflict {
                    reason: "repository root path conflicts".to_string(),
                },
                err => ServiceError::Store(err),
            })?;
        Ok(record)
    }

    /// List all registered repositories.
    #[allow(dead_code)]
    pub fn list_repositories(&self) -> ServiceResult<Vec<RepositoryRecord>> {
        Ok(self
            .list_repositories_page(ListRepositoriesRequest::default())?
            .repositories)
    }

    pub fn list_repositories_page(
        &self,
        request: ListRepositoriesRequest,
    ) -> ServiceResult<ListRepositoriesPage> {
        Ok(self.lifecycle.runtime().store().list_repositories()?)
            .and_then(|repos| {
                let mut records = repos;
                records.retain(|repository| {
                    request
                        .trust_state
                        .as_ref()
                        .map(|state| repository.trust_state == *state)
                        .unwrap_or(true)
                });

                records.sort_by(|left, right| left.id.cmp(&right.id));

                let page_size = request.page_size.unwrap_or(usize::MAX);
                let start = parse_page_token(request.page_token.as_deref(), "repositories")?;
                let page = paginate_records(records, start, page_size);

                Ok(page)
            })
            .map(|(repositories, next_page_token)| ListRepositoriesPage {
                repositories,
                next_page_token,
            })
    }

    /// Look up a single repository by id.
    #[allow(dead_code)]
    pub fn get_repository(&self, id: &RepositoryId) -> ServiceResult<RepositoryRecord> {
        Ok(self.lifecycle.runtime().store().get_repository(id)?)
    }

    /// Resolve effective tool output defaults for an explicit scope.
    pub fn get_tool_output_defaults(
        &self,
        scope: ToolOutputSettingsScope,
    ) -> ServiceResult<ToolOutputDefaults> {
        Ok(self.lock_tool_output_settings()?.resolve_defaults(&scope))
    }

    /// Update tool output defaults for a specific scope and record an audit change.
    pub fn update_tool_output_defaults(
        &self,
        actor: Actor,
        scope: ToolOutputSettingsScope,
        update: ToolOutputOverrides,
        reason: String,
    ) -> ServiceResult<ToolOutputSettingsChange> {
        let mut tool_output_settings = self.lock_tool_output_settings()?;
        Ok(tool_output_settings.apply_update(
            actor,
            scope,
            update,
            reason,
            LedgerTimestamp::now(),
        )?)
    }

    /// Return a snapshot of tool output settings changes, optionally filtered by scope.
    #[allow(dead_code)]
    pub fn list_tool_output_settings_history(
        &self,
        scope: Option<ToolOutputSettingsScope>,
    ) -> ServiceResult<ListToolOutputSettingsHistoryPage> {
        self.list_tool_output_settings_history_page(ListToolOutputSettingsHistoryRequest {
            scope,
            ..Default::default()
        })
    }

    /// Return a bounded page of tool output settings change records.
    pub fn list_tool_output_settings_history_page(
        &self,
        request: ListToolOutputSettingsHistoryRequest,
    ) -> ServiceResult<ListToolOutputSettingsHistoryPage> {
        let ListToolOutputSettingsHistoryRequest {
            scope,
            limit: requested_limit,
            offset,
            cursor,
        } = request;
        let requested_limit = requested_limit.unwrap_or(MAX_HISTORY_PAGE);
        let limit = requested_limit.min(MAX_HISTORY_PAGE);
        let start = list_history_page_start(offset, cursor)?;
        if limit == 0 {
            return Ok(ListToolOutputSettingsHistoryPage {
                changes: Vec::new(),
                next_page_token: None,
            });
        }
        let changes = self
            .lock_tool_output_settings()?
            .changes()
            .iter()
            .filter(|change| scope.as_ref().is_none_or(|scope| change.scope == *scope))
            .skip(start)
            .take(limit.saturating_add(1))
            .cloned()
            .collect::<Vec<_>>();

        if changes.len() <= limit {
            return Ok(ListToolOutputSettingsHistoryPage {
                changes,
                next_page_token: None,
            });
        }

        let next_page_token = (start + limit).to_string();
        let mut changes = changes;
        changes.truncate(limit);
        Ok(ListToolOutputSettingsHistoryPage {
            changes,
            next_page_token: Some(next_page_token),
        })
    }

    /// Return a compact snapshot for one repository.
    #[allow(dead_code)]
    pub fn get_project_status(
        &self,
        request: GetProjectStatusRequest,
    ) -> ServiceResult<ProjectStatusSnapshot> {
        let repository_id = request.repository_id;
        let repository = self.get_repository(&repository_id)?;
        let atelia_core::ProjectStatusSnapshot {
            recent_jobs,
            recent_policy_decisions,
            latest_event,
        } = self.lifecycle.runtime().store().project_status_snapshot(
            &repository_id,
            PROJECT_STATUS_RECENT_LIMIT,
            PROJECT_STATUS_RECENT_LIMIT,
        )?;

        Ok(ProjectStatusSnapshot {
            repository,
            recent_jobs,
            recent_policy_decisions,
            latest_event,
            daemon_status: self.daemon_status,
            storage_status: self.health().storage_status,
        })
    }

    /// Submit the first supported daemon job, backed by the core echo tool.
    #[allow(dead_code)]
    pub fn submit_job(&self, request: SubmitJobRequest) -> ServiceResult<RuntimeJobReceipt> {
        if request.goal.trim().is_empty() {
            return Err(ServiceError::InvalidArgument {
                reason: "goal must not be empty".to_string(),
            });
        }

        let requested_capabilities =
            normalize_requested_capabilities(&request.requested_capabilities)?;

        let normalized_idempotency_key = request
            .idempotency_key
            .as_ref()
            .map(|key| key.trim())
            .filter(|key| !key.is_empty())
            .map(str::to_string);
        let request_signature = submit_job_request_signature(&request, &requested_capabilities);
        let mut idempotent_cache_lock = if let Some(idempotency_key) =
            normalized_idempotency_key.as_deref()
        {
            let cache = self
                .idempotent_submissions
                .lock()
                .expect("idempotency cache lock poisoned");

            if let Some(cached) = cache.get(idempotency_key) {
                if cached.signature == request_signature {
                    return Ok(cached.receipt.clone());
                }
                return Err(ServiceError::Conflict {
                    reason: "idempotency_key was previously used for a different submit request"
                        .to_string(),
                });
            }

            Some((idempotency_key.to_string(), cache))
        } else {
            None
        };

        let tool_output_defaults = self.get_tool_output_defaults(
            ToolOutputSettingsScope::repository(request.repository_id.clone())
                .for_tool(SECRETARY_ECHO_TOOL_ID),
        )?;

        let runtime_request = RuntimeJobRequest::new(
            request.requester,
            request.repository_id,
            request.kind,
            request.goal,
        )
        .with_requested_capabilities(requested_capabilities)
        .with_tool_output_defaults(tool_output_defaults)
        .with_resource_scope(
            request
                .resource_scope
                .as_ref()
                .map(|scope| scope.kind.clone())
                .unwrap_or_else(|| "repository".to_string()),
            request
                .resource_scope
                .as_ref()
                .map(|scope| scope.value.clone())
                .unwrap_or_else(|| ".".to_string()),
        );

        let receipt = self.lifecycle.submit_echo_job(runtime_request)?;

        if let Some((idempotency_key, mut cache)) = idempotent_cache_lock.take() {
            cache.insert(
                idempotency_key,
                IdempotentSubmitJob {
                    signature: request_signature,
                    receipt: receipt.clone(),
                },
            );
        }

        Ok(receipt)
    }

    pub fn check_policy(
        &self,
        request: CheckPolicyRequest,
    ) -> ServiceResult<atelia_core::PolicyDecision> {
        if request.requested_capability.trim().is_empty() {
            return Err(ServiceError::InvalidArgument {
                reason: "requested_capability must not be empty".to_string(),
            });
        }
        if request.action.trim().is_empty() {
            return Err(ServiceError::InvalidArgument {
                reason: "action must not be empty".to_string(),
            });
        }
        let requested_capability = request.requested_capability.trim().to_string();
        let action = request.action.trim().to_string();

        let repository = self.get_repository(&request.repository_id)?;
        let policy_input = PolicyInput::new(
            request.requester,
            request.repository_id,
            requested_capability,
            request.resource_scope,
            action,
            repository.trust_state,
            true,
            atelia_core::DEFAULT_POLICY_VERSION.to_string(),
        );
        let policy_engine = DefaultPolicyEngine::new();

        Ok(policy_engine.evaluate(policy_input))
    }

    /// Render a stored canonical tool result with the daemon's current settings.
    pub fn render_tool_output(
        &self,
        request: RenderToolOutputRequest,
    ) -> ServiceResult<RenderToolOutputResult> {
        let tool_result = self
            .lifecycle
            .runtime()
            .store()
            .get_tool_result(&request.tool_result_id)?;
        let tool_invocation = self
            .lifecycle
            .runtime()
            .store()
            .get_tool_invocation(&tool_result.invocation_id)?;
        let repository = self.get_repository(&tool_invocation.repository_id)?;
        let render_scope = ToolOutputSettingsScope::repository(repository.id.clone())
            .for_tool(tool_result.tool_id.clone());
        let defaults = self
            .lock_tool_output_settings()?
            .resolve_defaults(&render_scope);
        let mut render_options = defaults.render_options();
        render_options.format = request.format;
        let render_policy = defaults.render_policy_with_render_options(Some(&render_options));

        let rendered_output = render_tool_result_with_policy(&tool_result, &render_policy)
            .map_err(|error| ServiceError::Internal {
                reason: error.to_string(),
            })?;
        let truncation = rendered_output.truncation.clone();

        Ok(RenderToolOutputResult {
            tool_result: CanonicalToolResultRef {
                tool_result_id: tool_result.id,
                tool_invocation_id: tool_invocation.id,
                job_id: tool_invocation.job_id,
                repository_id: repository.id,
                content_type: "application/json".to_string(),
            },
            rendered_output,
            truncation,
        })
    }

    /// List jobs with optional repository/status filtering.
    #[allow(dead_code)]
    pub fn list_jobs(
        &self,
        repository_id: Option<RepositoryId>,
        status: Option<JobStatus>,
        requester: Option<Actor>,
        page_size: Option<usize>,
        page_token: Option<String>,
    ) -> ServiceResult<JobPage> {
        Ok(self.lifecycle.query_jobs(JobQuery {
            repository_id,
            status,
            requester,
            page_size,
            page_token,
        })?)
    }

    /// Look up a single job by id.
    #[allow(dead_code)]
    pub fn get_job(&self, id: &JobId) -> ServiceResult<JobRecord> {
        Ok(self.lifecycle.get_job(id)?)
    }

    pub fn cancellation_requester(&self, id: &JobId) -> Option<Actor> {
        self.cancellation_requesters
            .lock()
            .expect("cancellation requesters cache lock poisoned")
            .get(id)
            .cloned()
    }

    /// Query events with cursor, severity, and subject filters.
    #[allow(dead_code)]
    pub fn list_events_page(&self, query: EventQuery) -> ServiceResult<EventPage> {
        if query.page_size == Some(0) {
            return Err(ServiceError::InvalidArgument {
                reason: "page_size must be greater than 0".to_string(),
            });
        }
        Ok(self.lifecycle.runtime().store().query_job_events(query)?)
    }

    /// Replay events from a cursor for watch-style clients.
    #[allow(dead_code)]
    pub fn watch_events(
        &self,
        cursor: EventCursor,
        limit: Option<usize>,
    ) -> ServiceResult<Vec<atelia_core::JobEvent>> {
        Ok(self
            .lifecycle
            .runtime()
            .store()
            .replay_job_events(cursor, limit)?)
    }

    /// Request cancellation for a queued/running job.
    #[allow(dead_code)]
    pub fn cancel_job(
        &self,
        id: &JobId,
        reason: impl Into<String>,
        requester: Option<Actor>,
    ) -> ServiceResult<CancelJobReceipt> {
        let receipt = self.lifecycle.cancel_job(id, reason)?;
        if let Some(requester) = requester {
            self.cancellation_requesters
                .lock()
                .expect("cancellation requesters cache lock poisoned")
                .insert(id.clone(), requester);
        }
        Ok(receipt)
    }

    pub fn install_extension(
        &self,
        request: InstallExtensionRequest,
    ) -> ServiceResult<InstallExtensionResponse> {
        self.lock_extension_registry()?
            .install_extension(request)
            .map_err(ServiceError::from)
    }

    pub fn update_extension(
        &self,
        request: UpdateExtensionRequest,
    ) -> ServiceResult<UpdateExtensionResponse> {
        self.lock_extension_registry()?
            .update_extension(request)
            .map_err(ServiceError::from)
    }

    pub fn extension_status(
        &self,
        request: ExtensionStatusRequest,
    ) -> ServiceResult<ExtensionStatusResponse> {
        self.lock_extension_registry()?
            .extension_status(request)
            .map_err(ServiceError::from)
    }

    pub fn list_extensions(
        &self,
        request: ListExtensionsRequest,
    ) -> ServiceResult<ListExtensionsResponse> {
        self.lock_extension_registry()?
            .list_extensions(request)
            .map_err(ServiceError::from)
    }

    pub fn rollback_extension(
        &self,
        request: RollbackExtensionRequest,
    ) -> ServiceResult<RollbackExtensionResponse> {
        self.lock_extension_registry()?
            .rollback_extension(request)
            .map_err(ServiceError::from)
    }

    pub fn disable_extension(
        &self,
        request: DisableExtensionRequest,
    ) -> ServiceResult<DisableExtensionResponse> {
        self.lock_extension_registry()?
            .disable_extension(request)
            .map_err(ServiceError::from)
    }

    pub fn enable_extension(
        &self,
        request: EnableExtensionRequest,
    ) -> ServiceResult<EnableExtensionResponse> {
        self.lock_extension_registry()?
            .enable_extension(request)
            .map_err(ServiceError::from)
    }

    pub fn remove_extension(
        &self,
        request: RemoveExtensionRequest,
    ) -> ServiceResult<RemoveExtensionResponse> {
        self.lock_extension_registry()?
            .remove_extension(request)
            .map_err(ServiceError::from)
    }

    pub fn apply_blocklist(
        &self,
        request: ApplyBlocklistRequest,
    ) -> ServiceResult<ApplyBlocklistResponse> {
        self.lock_extension_registry()?
            .apply_blocklist(request)
            .map_err(ServiceError::from)
    }

    pub fn list_blocklist(
        &self,
        request: ListBlocklistRequest,
    ) -> ServiceResult<ListBlocklistResponse> {
        self.lock_extension_registry()?
            .list_blocklist(request)
            .map_err(ServiceError::from)
    }
}

fn parse_page_token(page_token: Option<&str>, collection: &'static str) -> ServiceResult<usize> {
    match page_token {
        Some(token) if !token.is_empty() => {
            token
                .parse::<usize>()
                .map_err(|_| ServiceError::InvalidArgument {
                    reason: format!("{collection} page token is not a numeric offset"),
                })
        }
        _ => Ok(0),
    }
}

fn list_history_page_start(offset: Option<usize>, cursor: Option<String>) -> ServiceResult<usize> {
    let has_offset = offset.is_some();
    let has_cursor = cursor.is_some();
    let cursors_specified = usize::from(has_offset) + usize::from(has_cursor);

    if cursors_specified > 1 {
        return Err(ServiceError::InvalidArgument {
            reason: "only one of offset or cursor may be set".to_string(),
        });
    }

    if let Some(cursor) = cursor {
        return parse_page_token(Some(cursor.as_str()), "tool output settings history");
    }

    if let Some(offset) = offset {
        return Ok(offset);
    }

    Ok(0)
}

fn paginate_records<T>(
    records: Vec<T>,
    start: usize,
    page_size: usize,
) -> (Vec<T>, Option<String>) {
    let mut records = records.into_iter();
    let mut skipped = 0usize;
    let mut retained = Vec::new();
    let mut has_next = false;

    if page_size == 0 {
        records.by_ref().take(start).for_each(|_| {});
        return (retained, None);
    }

    for record in records {
        if skipped < start {
            skipped += 1;
            continue;
        }

        if retained.len() == page_size {
            has_next = true;
            break;
        }

        retained.push(record);
    }

    let next_page_token = if has_next {
        Some((start + retained.len()).to_string())
    } else {
        None
    };

    (retained, next_page_token)
}

fn canonical_repository_root(root_path: &str) -> ServiceResult<String> {
    let path = PathBuf::from(root_path.trim());
    let canonical = path
        .canonicalize()
        .map_err(|_| ServiceError::InvalidArgument {
            reason: "root_path must identify an existing repository directory".to_string(),
        })?;
    if !canonical.is_dir() {
        return Err(ServiceError::InvalidArgument {
            reason: "root_path must identify an existing repository directory".to_string(),
        });
    }
    if !canonical.join(".git").exists() {
        return Err(ServiceError::InvalidArgument {
            reason: "root_path must identify a repository root with .git metadata".to_string(),
        });
    }

    Ok(canonical.to_string_lossy().to_string())
}

fn validate_repository_allowed_scope(
    root_path: &str,
    allowed_scope: &PathScope,
) -> ServiceResult<()> {
    let root_path = Path::new(root_path);
    for allowed_path in &allowed_scope.allowed_paths {
        canonicalize_within_scope(root_path, Path::new(allowed_path)).map_err(|err| {
            ServiceError::InvalidArgument {
                reason: format!("allowed_scope path {allowed_path:?} is invalid: {err}"),
            }
        })?;
    }

    Ok(())
}

fn actor_signature(actor: &Actor) -> String {
    match actor {
        Actor::User { id, display_name } => {
            format!("user:{id}:{:?}", display_name)
        }
        Actor::Agent { id, display_name } => {
            format!("agent:{id}:{:?}", display_name)
        }
        Actor::Extension { id } => format!("extension:{id}"),
        Actor::System { id } => format!("system:{id}"),
    }
}

fn normalize_requested_capabilities(
    requested_capabilities: &[String],
) -> ServiceResult<Vec<String>> {
    let mut normalized = Vec::new();

    for capability in requested_capabilities {
        let trimmed = capability.trim();
        if trimmed.is_empty() {
            return Err(ServiceError::InvalidArgument {
                reason: "requested_capabilities must not contain empty entries".to_string(),
            });
        }

        let canonical = canonicalize_job_requested_capability(trimmed).ok_or_else(|| {
            ServiceError::InvalidArgument {
                reason: format!(
                    "requested_capabilities contains unsupported capability: {trimmed}"
                ),
            }
        })?;

        if !normalized.iter().any(|existing| existing == canonical) {
            normalized.push(canonical.to_string());
        }
    }

    if normalized.is_empty() {
        normalized.push(
            canonicalize_job_requested_capability("capability.discovery")
                .expect("capability.discovery must be canonicalizable")
                .to_string(),
        );
    }

    normalized.sort();
    Ok(normalized)
}

fn submit_job_request_signature(
    request: &SubmitJobRequest,
    requested_capabilities: &[String],
) -> String {
    let resource_scope = request.resource_scope.as_ref().map_or_else(
        || "repository:.".to_string(),
        |scope| format!("{}:{}", scope.kind, scope.value),
    );
    let kind = match &request.kind {
        JobKind::Read => "read",
        JobKind::Mutate => "mutate",
        JobKind::Process => "process",
        JobKind::Maintenance => "maintenance",
        JobKind::Other { name } => name.as_str(),
    };

    format!(
        "requester={};repository={};kind={};goal={};resource_scope={};capabilities={};",
        actor_signature(&request.requester),
        request.repository_id.as_str(),
        kind,
        request.goal.trim(),
        resource_scope,
        requested_capabilities.join(","),
    )
}

impl Default for SecretaryService {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use atelia_core::{
        ApplyBlocklistRequest, BlockKey, BlockReason, ExtensionCompatibility, ExtensionEntrypoints,
        ExtensionFailure, ExtensionKind, ExtensionManifest, ExtensionPermission,
        ExtensionPublisher, ExtensionRealm, ExtensionRuntime, ExtensionServices,
        InstallExtensionRequest, ListBlocklistRequest, ListExtensionsRequest, ProvenanceSource,
        RepositoryTrustState, RetryPolicy, RollbackExtensionRequest, EXTENSION_MANIFEST_SCHEMA,
        EXTENSION_RPC_PROTOCOL,
    };
    use std::collections::BTreeMap;
    use std::collections::HashSet;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;
    use std::thread;

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn ready_service() -> SecretaryService {
        let mut svc = SecretaryService::new();
        svc.set_ready();
        svc
    }

    fn test_repo_dir(name: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "atelia-service-test-{}-{}-{name}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    fn plain_test_dir(name: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "atelia-service-plain-test-{}-{}-{name}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
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
            tool_output: Vec::new(),
            hooks: Vec::new(),
            webhooks: Vec::new(),
            composition: Default::default(),
            failure: ExtensionFailure {
                degrade: atelia_core::DegradeBehavior::ReturnUnavailable,
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
            migration: Default::default(),
        }
    }

    // -- health tests -------------------------------------------------------

    #[test]
    fn health_returns_ready_after_set_ready() {
        let svc = ready_service();
        let health = svc.health();
        assert_eq!(health.daemon_status, DaemonStatus::Ready);
        assert_eq!(health.storage_status, StorageStatus::Ready);
        assert_eq!(health.daemon_version, DAEMON_VERSION);
        assert_eq!(health.protocol_version, PROTOCOL_VERSION);
        assert_eq!(health.storage_version, STORAGE_VERSION);
    }

    #[test]
    fn health_starts_starting() {
        let svc = SecretaryService::new();
        assert_eq!(svc.health().daemon_status, DaemonStatus::Starting);
    }

    #[test]
    fn health_returns_running_after_set_running() {
        let mut svc = SecretaryService::new();
        svc.set_running();
        assert_eq!(svc.health().daemon_status, DaemonStatus::Running);
    }

    #[test]
    fn health_reflects_stopping() {
        let mut svc = SecretaryService::new();
        svc.set_stopping();
        assert_eq!(svc.health().daemon_status, DaemonStatus::Stopping);
    }

    #[test]
    fn health_reports_capabilities() {
        let health = ready_service().health();
        assert!(health.capabilities.contains(&"health.v1".to_string()));
        assert!(health.capabilities.contains(&"repositories.v1".to_string()));
        assert!(health.capabilities.contains(&"jobs.v1".to_string()));
        assert!(health.capabilities.contains(&"policy.v1".to_string()));
        assert!(health
            .capabilities
            .contains(&"extensions.registry.v1".to_string()));
        assert!(health
            .capabilities
            .contains(&"tool_output_settings.v1".to_string()));
        assert!(health
            .capabilities
            .contains(&"tool_output_render.v1".to_string()));
        assert!(health
            .capabilities
            .contains(&"project_status.v1".to_string()));
    }

    #[test]
    fn extension_registry_supports_install_status_list_blocklist_and_rollback() {
        const ARTIFACT_V1: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const MANIFEST_V1: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        const ARTIFACT_V2: &str =
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        const MANIFEST_V2: &str =
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

        let svc = ready_service();
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

        let installed_v1 = svc
            .install_extension(InstallExtensionRequest {
                manifest: manifest_v1.clone(),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
            })
            .expect("first install should succeed");
        assert_eq!(installed_v1.record.version, "1.0.0");

        let installed_v2 = svc
            .install_extension(InstallExtensionRequest {
                manifest: manifest_v2.clone(),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
            })
            .expect("second install should succeed");
        assert_eq!(installed_v2.record.version, "2.0.0");

        let status = svc
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect("status should succeed");
        assert_eq!(status.record.as_ref().unwrap().version, "2.0.0");
        assert!(status.block.is_none());

        let list = svc
            .list_extensions(ListExtensionsRequest {
                include_blocked: true,
            })
            .expect("list should succeed");
        assert_eq!(list.extensions.len(), 1);
        assert_eq!(list.extensions[0].record.as_ref().unwrap().version, "2.0.0");

        let rolled_back = svc
            .rollback_extension(RollbackExtensionRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect("rollback should succeed");
        assert_eq!(rolled_back.record.version, "1.0.0");

        let disabled = svc
            .disable_extension(DisableExtensionRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect("disable should succeed");
        assert_eq!(
            disabled.record.status,
            atelia_core::ExtensionInstallStatus::Disabled
        );

        let enabled = svc
            .enable_extension(EnableExtensionRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect("enable should succeed");
        assert_eq!(
            enabled.record.status,
            atelia_core::ExtensionInstallStatus::Installed
        );

        let block = svc
            .apply_blocklist(ApplyBlocklistRequest {
                entry: atelia_core::BlocklistEntry {
                    key: BlockKey::ExtensionId("com.example.review.extension".to_string()),
                    reason: BlockReason::UserBlocked,
                    note: Some("disabled for review".to_string()),
                },
            })
            .expect("apply blocklist should succeed");
        assert_eq!(block.entry.reason, BlockReason::UserBlocked);

        let blocked_status = svc
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect("blocked status should succeed");
        assert!(blocked_status.block.is_some());
        assert_eq!(
            blocked_status.record.as_ref().unwrap().status,
            atelia_core::ExtensionInstallStatus::Blocked
        );

        let blocked_enable = svc
            .enable_extension(EnableExtensionRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect_err("enable should fail while extension is blocklisted");
        assert!(matches!(
            blocked_enable,
            ServiceError::ExtensionRegistry(RegistryError::Blocked { .. })
        ));

        let blocked_list = svc
            .list_extensions(ListExtensionsRequest {
                include_blocked: false,
            })
            .expect("filtered list should succeed");
        assert!(blocked_list.extensions.is_empty());

        let blocklist = svc
            .list_blocklist(ListBlocklistRequest {})
            .expect("list blocklist should succeed");
        assert_eq!(blocklist.entries.len(), 1);

        let final_status = svc
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect("final status should succeed");
        assert_eq!(final_status.record.as_ref().unwrap().version, "1.0.0");

        let removed = svc
            .remove_extension(RemoveExtensionRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect("remove should succeed");
        assert_eq!(
            removed.record.status,
            atelia_core::ExtensionInstallStatus::Disabled
        );

        let missing_after_remove = svc
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect_err("removed extension should not have active status");
        assert!(matches!(
            missing_after_remove,
            ServiceError::ExtensionRegistry(RegistryError::NotInstalled { .. })
        ));
    }

    #[test]
    fn extension_registry_lock_poisoning_returns_service_error() {
        let svc = Arc::new(ready_service());
        let poisoned = Arc::clone(&svc);

        thread::spawn(move || {
            let _guard = poisoned.extension_registry.lock().unwrap();
            panic!("poison extension registry lock");
        })
        .join()
        .expect_err("poisoning thread should panic");

        let manifest = extension_manifest(
            "com.example.review.extension",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );

        for result in [
            svc.install_extension(InstallExtensionRequest {
                manifest: manifest.clone(),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
            })
            .map(|_| "install_extension"),
            svc.extension_status(atelia_core::ExtensionStatusRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .map(|_| "extension_status"),
            svc.list_extensions(ListExtensionsRequest {
                include_blocked: true,
            })
            .map(|_| "list_extensions"),
            svc.rollback_extension(RollbackExtensionRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .map(|_| "rollback_extension"),
            svc.apply_blocklist(ApplyBlocklistRequest {
                entry: atelia_core::BlocklistEntry {
                    key: BlockKey::ExtensionId("com.example.review.extension".to_string()),
                    reason: BlockReason::UserBlocked,
                    note: Some("poison check".to_string()),
                },
            })
            .map(|_| "apply_blocklist"),
            svc.list_blocklist(ListBlocklistRequest {})
                .map(|_| "list_blocklist"),
        ] {
            let err = result.unwrap_err();
            assert!(matches!(
                err,
                ServiceError::Internal { reason } if reason.contains("extension registry lock poisoned")
            ));
        }
    }

    #[test]
    fn health_reports_zero_repositories_initially() {
        assert_eq!(ready_service().health().repository_count, 0);
    }

    #[test]
    fn health_records_started_at() {
        let svc = SecretaryService::new();
        let health = svc.health();
        assert!(health.started_at.unix_millis > 0);
    }

    #[test]
    fn protocol_metadata_matches_protocol_versions() {
        let metadata = ready_service().protocol_metadata();

        assert_eq!(metadata.protocol_version, PROTOCOL_VERSION);
        assert_eq!(metadata.daemon_version, DAEMON_VERSION);
        assert_eq!(metadata.storage_version, STORAGE_VERSION);
        assert!(metadata.capabilities.contains(&"health.v1".to_string()));
        assert!(metadata
            .capabilities
            .contains(&"repositories.v1".to_string()));
        assert!(metadata.capabilities.contains(&"jobs.v1".to_string()));
        assert!(metadata.capabilities.contains(&"policy.v1".to_string()));
        assert!(metadata
            .capabilities
            .contains(&"extensions.registry.v1".to_string()));
        assert!(metadata
            .capabilities
            .contains(&"tool_output_settings.v1".to_string()));
        assert!(metadata
            .capabilities
            .contains(&"tool_output_render.v1".to_string()));
        assert!(metadata
            .capabilities
            .contains(&"project_status.v1".to_string()));
    }

    // -- register / list round trip -----------------------------------------

    #[test]
    fn register_repository_returns_record() {
        let svc = ready_service();
        let root = test_repo_dir("register");
        let record = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "test-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        assert_eq!(record.display_name, "test-repo");
        assert_eq!(
            record.root_path,
            root.canonicalize().unwrap().to_string_lossy()
        );
        assert_eq!(record.trust_state, RepositoryTrustState::Trusted);
        assert!(record.id.has_valid_prefix());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn register_rejects_empty_display_name() {
        let svc = ready_service();
        let err = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "".to_string(),
                root_path: "/tmp/test".to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
    }

    #[test]
    fn register_rejects_empty_root_path() {
        let svc = ready_service();
        let err = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "test-repo".to_string(),
                root_path: "".to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
    }

    #[test]
    fn register_rejects_existing_directory_without_git_metadata() {
        let svc = ready_service();
        let root = plain_test_dir("not-repo");
        let err = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "not-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn register_rejects_allowed_scope_outside_repository_root() {
        let svc = ready_service();
        let root = test_repo_dir("scope-outside-root");
        let sibling = root.parent().unwrap().join("scope-outside-sibling");
        fs::create_dir_all(&sibling).unwrap();

        let err = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "scope-outside".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: Some(PathScope {
                    root_path: root.to_string_lossy().to_string(),
                    allowed_paths: vec!["../scope-outside-sibling".to_string()],
                }),
                requester: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(sibling);
    }

    #[test]
    fn register_rejects_duplicate_canonical_root() {
        let svc = ready_service();
        let root = test_repo_dir("duplicate");

        svc.register_repository(RegisterRepositoryRequest {
            display_name: "repo-a".to_string(),
            root_path: root.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::Trusted,
            allowed_scope: None,
            requester: None,
        })
        .expect("first register should succeed");

        let err = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "repo-b".to_string(),
                root_path: root.join(".").to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::ReadOnly,
                allowed_scope: None,
                requester: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::Conflict { .. }));
        assert_eq!(svc.health().repository_count, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn register_list_round_trip() {
        let svc = ready_service();
        let root_a = test_repo_dir("round-a");
        let root_b = test_repo_dir("round-b");

        let r1 = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "repo-a".to_string(),
                root_path: root_a.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register a");
        let r2 = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "repo-b".to_string(),
                root_path: root_b.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::ReadOnly,
                allowed_scope: None,
                requester: None,
            })
            .expect("register b");

        let repos = svc.list_repositories().expect("list should succeed");
        assert_eq!(repos.len(), 2);

        let ids: Vec<_> = repos.iter().map(|r| r.id.clone()).collect();
        assert!(ids.contains(&r1.id));
        assert!(ids.contains(&r2.id));
        let _ = fs::remove_dir_all(root_a);
        let _ = fs::remove_dir_all(root_b);
    }

    #[test]
    fn list_repositories_page_filters_and_paginates() {
        let svc = ready_service();
        let root_a = test_repo_dir("list-page-a");
        let root_b = test_repo_dir("list-page-b");
        let root_c = test_repo_dir("list-page-c");

        svc.register_repository(RegisterRepositoryRequest {
            display_name: "repo-a".to_string(),
            root_path: root_a.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::Trusted,
            allowed_scope: None,
            requester: None,
        })
        .expect("register trusted should succeed");
        svc.register_repository(RegisterRepositoryRequest {
            display_name: "repo-b".to_string(),
            root_path: root_b.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::ReadOnly,
            allowed_scope: None,
            requester: None,
        })
        .expect("register read-only should succeed");
        svc.register_repository(RegisterRepositoryRequest {
            display_name: "repo-c".to_string(),
            root_path: root_c.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::Trusted,
            allowed_scope: None,
            requester: None,
        })
        .expect("register trusted should succeed");

        let first = svc
            .list_repositories_page(ListRepositoriesRequest {
                trust_state: Some(RepositoryTrustState::Trusted),
                page_size: Some(1),
                page_token: None,
            })
            .expect("list repositories should succeed");
        assert_eq!(first.repositories.len(), 1);
        assert_eq!(
            first.repositories[0].trust_state,
            RepositoryTrustState::Trusted
        );
        assert_eq!(first.next_page_token, Some("1".to_string()));

        let second = svc
            .list_repositories_page(ListRepositoriesRequest {
                trust_state: Some(RepositoryTrustState::Trusted),
                page_size: Some(1),
                page_token: first.next_page_token,
            })
            .expect("list repositories should succeed");
        assert_eq!(second.repositories.len(), 1);
        assert_ne!(second.repositories[0].id, first.repositories[0].id);
        assert_eq!(second.next_page_token, None);

        let _ = fs::remove_dir_all(root_a);
        let _ = fs::remove_dir_all(root_b);
        let _ = fs::remove_dir_all(root_c);
    }

    #[test]
    fn list_repositories_request_rejects_invalid_page_token() {
        let svc = ready_service();
        let err = svc
            .list_repositories_page(ListRepositoriesRequest {
                trust_state: None,
                page_size: None,
                page_token: Some("not-a-number".to_string()),
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
    }

    #[test]
    fn list_events_page_rejects_zero_page_size() {
        let svc = ready_service();
        let err = svc
            .list_events_page(EventQuery {
                page_size: Some(0),
                ..EventQuery::default()
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
    }

    #[test]
    fn get_project_status_returns_repository_jobs_policies_and_latest_event() {
        let svc = ready_service();
        let root = test_repo_dir("project-status");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "project-status-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let submitted = svc
            .submit_job(SubmitJobRequest {
                requester: Actor::Agent {
                    id: "agent:test".to_string(),
                    display_name: Some("Test Agent".to_string()),
                },
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "summarize current repository status".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .expect("submit should succeed");

        let status = svc
            .get_project_status(GetProjectStatusRequest {
                repository_id: repository.id.clone(),
            })
            .expect("project status should succeed");

        assert_eq!(status.repository.id, repository.id);
        assert_eq!(status.recent_jobs.len(), 1);
        assert_eq!(status.recent_jobs[0].id, submitted.job.id);
        assert_eq!(status.recent_policy_decisions.len(), 1);
        assert!(status.latest_event.is_some());
        assert_eq!(status.daemon_status, DaemonStatus::Ready);
        assert_eq!(status.storage_status, StorageStatus::Ready);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn get_project_status_keeps_latest_event_scoped_to_repository() {
        let svc = ready_service();
        let root_a = test_repo_dir("project-status-a");
        let root_b = test_repo_dir("project-status-b");

        let repository_a = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "project-status-repo-a".to_string(),
                root_path: root_a.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register repository a should succeed");
        let repository_b = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "project-status-repo-b".to_string(),
                root_path: root_b.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register repository b should succeed");

        let submitted_a = svc
            .submit_job(SubmitJobRequest {
                requester: Actor::Agent {
                    id: "agent:a".to_string(),
                    display_name: Some("Agent A".to_string()),
                },
                repository_id: repository_a.id.clone(),
                kind: JobKind::Read,
                goal: "summarize repository a".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .expect("submit a should succeed");
        let _submitted_b = svc
            .submit_job(SubmitJobRequest {
                requester: Actor::Agent {
                    id: "agent:b".to_string(),
                    display_name: Some("Agent B".to_string()),
                },
                repository_id: repository_b.id.clone(),
                kind: JobKind::Read,
                goal: "summarize repository b".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .expect("submit b should succeed");

        let status = svc
            .get_project_status(GetProjectStatusRequest {
                repository_id: repository_a.id.clone(),
            })
            .expect("project status should succeed");

        assert_eq!(status.repository.id, repository_a.id);
        let latest_event = status
            .latest_event
            .expect("project status should include latest event");
        assert_eq!(latest_event.refs.job_id, Some(submitted_a.job.id));
        assert_eq!(
            latest_event.refs.repository_id,
            Some(repository_a.id.clone())
        );

        let _ = fs::remove_dir_all(root_a);
        let _ = fs::remove_dir_all(root_b);
    }

    #[test]
    fn health_updates_repository_count() {
        let svc = ready_service();
        let root_a = test_repo_dir("health-a");
        let root_b = test_repo_dir("health-b");

        assert_eq!(svc.health().repository_count, 0);

        svc.register_repository(RegisterRepositoryRequest {
            display_name: "repo-a".to_string(),
            root_path: root_a.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::Trusted,
            allowed_scope: None,
            requester: None,
        })
        .expect("register a");

        assert_eq!(svc.health().repository_count, 1);

        svc.register_repository(RegisterRepositoryRequest {
            display_name: "repo-b".to_string(),
            root_path: root_b.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::Trusted,
            allowed_scope: None,
            requester: None,
        })
        .expect("register b");

        assert_eq!(svc.health().repository_count, 2);
        let _ = fs::remove_dir_all(root_a);
        let _ = fs::remove_dir_all(root_b);
    }

    #[test]
    fn get_repository_after_register() {
        let svc = ready_service();
        let root = test_repo_dir("lookup");
        let record = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "lookup-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register");

        let fetched = svc.get_repository(&record.id).expect("get should succeed");
        assert_eq!(fetched.id, record.id);
        assert_eq!(fetched.display_name, "lookup-repo");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn get_repository_not_found() {
        let svc = ready_service();
        let missing_id = RepositoryId::new();
        let err = svc.get_repository(&missing_id).unwrap_err();
        assert!(matches!(err, ServiceError::Store(_)));
    }

    // -- policy checks API -------------------------------------------------

    #[test]
    fn check_policy_runs_preview_for_allowed_capability() {
        let svc = ready_service();
        let root = test_repo_dir("policy-check-allowed");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "check-policy-allowed".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let decision = svc
            .check_policy(CheckPolicyRequest {
                repository_id: repository.id,
                requester: actor(),
                requested_capability: "filesystem.read".to_string(),
                action: "inspect".to_string(),
                resource_scope: ResourceScope {
                    kind: "repository".to_string(),
                    value: ".".to_string(),
                },
            })
            .expect("check policy should succeed");

        assert_eq!(decision.outcome, atelia_core::PolicyOutcome::Allowed);
        assert_eq!(decision.risk_tier, atelia_core::RiskTier::R1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn check_policy_blocks_repository_blocked_trust_state() {
        let svc = ready_service();
        let root = test_repo_dir("policy-check-blocked");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "check-policy-blocked".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Blocked,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let decision = svc
            .check_policy(CheckPolicyRequest {
                repository_id: repository.id,
                requester: actor(),
                requested_capability: "filesystem.read".to_string(),
                action: "inspect".to_string(),
                resource_scope: ResourceScope {
                    kind: "repository".to_string(),
                    value: ".".to_string(),
                },
            })
            .expect("check policy should succeed");

        assert_eq!(decision.outcome, atelia_core::PolicyOutcome::Blocked);
        assert_eq!(decision.reason_code, "repository_blocked");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn check_policy_rejects_empty_requested_capability() {
        let svc = ready_service();
        let root = test_repo_dir("policy-check-empty-capability");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "policy-check-empty-capability".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let err = svc
            .check_policy(CheckPolicyRequest {
                repository_id: repository.id,
                requester: actor(),
                requested_capability: "".to_string(),
                action: "inspect".to_string(),
                resource_scope: ResourceScope {
                    kind: "repository".to_string(),
                    value: ".".to_string(),
                },
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn check_policy_rejects_whitespace_requested_capability() {
        let svc = ready_service();
        let root = test_repo_dir("policy-check-whitespace-capability");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "policy-check-whitespace-capability".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let err = svc
            .check_policy(CheckPolicyRequest {
                repository_id: repository.id,
                requester: actor(),
                requested_capability: " \t".to_string(),
                action: "inspect".to_string(),
                resource_scope: ResourceScope {
                    kind: "repository".to_string(),
                    value: ".".to_string(),
                },
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn check_policy_rejects_empty_action() {
        let svc = ready_service();
        let root = test_repo_dir("policy-check-empty-action");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "policy-check-empty-action".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let err = svc
            .check_policy(CheckPolicyRequest {
                repository_id: repository.id,
                requester: actor(),
                requested_capability: "filesystem.read".to_string(),
                action: "".to_string(),
                resource_scope: ResourceScope {
                    kind: "repository".to_string(),
                    value: ".".to_string(),
                },
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn check_policy_rejects_whitespace_action() {
        let svc = ready_service();
        let root = test_repo_dir("policy-check-whitespace-action");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "policy-check-whitespace-action".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let err = svc
            .check_policy(CheckPolicyRequest {
                repository_id: repository.id,
                requester: actor(),
                requested_capability: "filesystem.read".to_string(),
                action: "\n".to_string(),
                resource_scope: ResourceScope {
                    kind: "repository".to_string(),
                    value: ".".to_string(),
                },
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    // -- tool output settings API -------------------------------------------

    #[test]
    fn tool_output_defaults_initially_resolve_from_workspace_baseline() {
        let svc = ready_service();
        let defaults = svc
            .get_tool_output_defaults(ToolOutputSettingsScope::workspace())
            .expect("defaults lookup should succeed");
        let baseline = ToolOutputDefaults::default();

        assert_eq!(defaults.max_inline_bytes, baseline.max_inline_bytes);
        assert_eq!(defaults.max_inline_lines, baseline.max_inline_lines);
        assert_eq!(defaults.verbosity, baseline.verbosity);
        assert_eq!(defaults.granularity, baseline.granularity);
        assert_eq!(defaults.oversize_policy, baseline.oversize_policy);
        assert_eq!(defaults.render_options, baseline.render_options);
    }

    #[test]
    fn render_tool_output_uses_settings_and_rendered_result() {
        let svc = ready_service();
        let root = test_repo_dir("render-tool-output");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "render-tool-output".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("repository registration should succeed");
        let long_goal = "x".repeat(300);

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: long_goal.clone(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .expect("job submission should succeed");
        let tool_result = receipt.tool_result.expect("tool result should be recorded");

        svc.update_tool_output_defaults(
            actor(),
            ToolOutputSettingsScope::workspace().for_tool(tool_result.tool_id.clone()),
            ToolOutputOverrides {
                max_inline_bytes: Some(256),
                granularity: Some(atelia_core::ToolOutputGranularity::Summary),
                ..ToolOutputOverrides::default()
            },
            "Compact rendered stored results".to_string(),
        )
        .expect("tool output settings update should succeed");

        let tool_result_id = tool_result.id.clone();
        let tool_invocation_id = tool_result.invocation_id.clone();
        let job_id = receipt.job.id.clone();
        let repository_id = repository.id.clone();
        let rendered = svc
            .render_tool_output(RenderToolOutputRequest {
                tool_result_id: tool_result_id.clone(),
                repository_id: Some(repository_id.clone()),
                format: OutputFormat::Json,
            })
            .expect("rendering should succeed");
        let rendered_json: serde_json::Value =
            serde_json::from_str(&rendered.rendered_output.body).expect("rendered json");

        assert_eq!(rendered.rendered_output.format, OutputFormat::Json);
        assert_eq!(rendered.tool_result.tool_result_id, tool_result_id);
        assert_eq!(rendered.tool_result.tool_invocation_id, tool_invocation_id);
        assert_eq!(rendered.tool_result.job_id, job_id);
        assert_eq!(rendered.tool_result.repository_id, repository_id);
        assert_eq!(rendered.tool_result.content_type, "application/json");
        assert_eq!(rendered_json["fields"].as_array().unwrap().len(), 1);
        assert_eq!(rendered_json["fields"][0]["key"], "summary");
        assert!(rendered
            .rendered_output
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("render policy compacted output"));
        assert!(rendered
            .truncation
            .as_ref()
            .unwrap()
            .reason
            .contains("max_inline_bytes=256"));
        assert_eq!(rendered.truncation, rendered.rendered_output.truncation);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn render_tool_output_uses_canonical_repository_scope_for_settings() {
        let svc = ready_service();
        let root = test_repo_dir("render-tool-output-canonical-scope");
        let wrong_root = test_repo_dir("render-tool-output-wrong-scope");
        let canonical_repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "canonical-scope".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("repository registration should succeed");
        let wrong_repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "wrong-scope".to_string(),
                root_path: wrong_root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("second repository registration should succeed");

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: canonical_repository.id.clone(),
                kind: JobKind::Read,
                goal: "render tool output".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .expect("job submission should succeed");
        let tool_result = receipt.tool_result.expect("tool result should be recorded");

        svc.update_tool_output_defaults(
            actor(),
            ToolOutputSettingsScope::repository(canonical_repository.id.clone())
                .for_tool(tool_result.tool_id.clone()),
            ToolOutputOverrides {
                verbosity: Some(atelia_core::ToolOutputVerbosity::Debug),
                include_policy: Some(true),
                ..ToolOutputOverrides::default()
            },
            "Canonical repository override".to_string(),
        )
        .expect("canonical repository settings should update");
        svc.update_tool_output_defaults(
            actor(),
            ToolOutputSettingsScope::repository(wrong_repository.id.clone())
                .for_tool(tool_result.tool_id.clone()),
            ToolOutputOverrides {
                verbosity: Some(atelia_core::ToolOutputVerbosity::Normal),
                include_policy: Some(false),
                ..ToolOutputOverrides::default()
            },
            "Wrong repository override".to_string(),
        )
        .expect("wrong repository settings should update");

        let tool_result_id = tool_result.id.clone();
        let wrong_repository_id = wrong_repository.id.clone();
        let rendered = svc
            .render_tool_output(RenderToolOutputRequest {
                tool_result_id: tool_result_id.clone(),
                repository_id: Some(wrong_repository_id),
                format: OutputFormat::Json,
            })
            .expect("rendering should succeed");

        assert!(rendered.rendered_output.body.contains("policy.state"));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(wrong_root);
    }

    #[test]
    fn tool_output_update_updates_workspace_defaults_and_records_change() {
        let svc = ready_service();
        let change = svc
            .update_tool_output_defaults(
                actor(),
                ToolOutputSettingsScope::workspace(),
                ToolOutputOverrides {
                    max_inline_lines: Some(120),
                    ..ToolOutputOverrides::default()
                },
                "Adjust workspace defaults for concise responses".to_string(),
            )
            .expect("workspace output update should succeed");

        let defaults = svc
            .get_tool_output_defaults(ToolOutputSettingsScope::workspace())
            .expect("defaults lookup should succeed");

        assert_eq!(defaults.max_inline_lines, 120);
        assert_eq!(
            change.scope.level,
            ToolOutputSettingsScope::workspace().level
        );
        assert_eq!(change.new_defaults.max_inline_lines, 120);
        assert_eq!(
            change.reason,
            "Adjust workspace defaults for concise responses"
        );
        assert_eq!(change.actor, actor());
        let history = svc
            .list_tool_output_settings_history(None)
            .expect("history should be available");
        assert_eq!(history.changes.len(), 1);
        assert_eq!(
            history.changes[0].scope.level,
            ToolOutputSettingsScope::workspace().level
        );
    }

    #[test]
    fn tool_output_update_rejects_empty_update() {
        let svc = ready_service();
        let err = svc
            .update_tool_output_defaults(
                actor(),
                ToolOutputSettingsScope::workspace(),
                ToolOutputOverrides::default(),
                "Empty update should fail".to_string(),
            )
            .unwrap_err();

        assert!(matches!(
            err,
            ServiceError::Settings(ToolOutputSettingsError::EmptyUpdate)
        ));
    }

    #[test]
    fn tool_output_update_rejects_missing_reason() {
        let svc = ready_service();
        let err = svc
            .update_tool_output_defaults(
                actor(),
                ToolOutputSettingsScope::workspace(),
                ToolOutputOverrides {
                    max_inline_lines: Some(250),
                    ..ToolOutputOverrides::default()
                },
                "   ".to_string(),
            )
            .unwrap_err();

        assert!(matches!(
            err,
            ServiceError::Settings(ToolOutputSettingsError::MissingReason)
        ));
    }

    #[test]
    fn tool_output_settings_lock_poisoning_returns_service_error() {
        let svc = Arc::new(ready_service());
        let poisoned = Arc::clone(&svc);

        thread::spawn(move || {
            let _guard = poisoned.tool_output_settings.lock().unwrap();
            panic!("poison tool output settings lock");
        })
        .join()
        .expect_err("poisoning thread should panic");

        let defaults_err = svc
            .get_tool_output_defaults(ToolOutputSettingsScope::workspace())
            .unwrap_err();
        assert!(matches!(
            defaults_err,
            ServiceError::Internal { reason } if reason.contains("tool output settings lock poisoned")
        ));

        let update_err = svc
            .update_tool_output_defaults(
                actor(),
                ToolOutputSettingsScope::workspace(),
                ToolOutputOverrides {
                    max_inline_lines: Some(32),
                    ..ToolOutputOverrides::default()
                },
                "attempt after poison".to_string(),
            )
            .unwrap_err();
        assert!(matches!(
            update_err,
            ServiceError::Internal { reason } if reason.contains("tool output settings lock poisoned")
        ));

        let history_err = svc
            .list_tool_output_settings_history_page(ListToolOutputSettingsHistoryRequest {
                ..Default::default()
            })
            .unwrap_err();
        assert!(matches!(
            history_err,
            ServiceError::Internal { reason } if reason.contains("tool output settings lock poisoned")
        ));
    }

    #[test]
    fn tool_output_history_can_filter_by_scope() {
        let svc = ready_service();
        svc.update_tool_output_defaults(
            actor(),
            ToolOutputSettingsScope::workspace(),
            ToolOutputOverrides {
                max_inline_bytes: Some(16_384),
                ..ToolOutputOverrides::default()
            },
            "workspace baseline".to_string(),
        )
        .expect("workspace output update should succeed");

        let repository_scope = ToolOutputSettingsScope::repository(RepositoryId::new());
        svc.update_tool_output_defaults(
            actor(),
            repository_scope.clone(),
            ToolOutputOverrides {
                max_inline_lines: Some(80),
                ..ToolOutputOverrides::default()
            },
            "repository override".to_string(),
        )
        .expect("repository output update should succeed");

        let all = svc
            .list_tool_output_settings_history(None)
            .expect("history should be available");
        let workspace = svc
            .list_tool_output_settings_history(Some(ToolOutputSettingsScope::workspace()))
            .expect("workspace history should be available");
        let repository = svc
            .list_tool_output_settings_history(Some(repository_scope.clone()))
            .expect("repository history should be available");

        assert_eq!(all.changes.len(), 2);
        assert_eq!(workspace.changes.len(), 1);
        assert_eq!(repository.changes.len(), 1);
        assert_eq!(
            workspace.changes[0].scope.level,
            ToolOutputSettingsScope::workspace().level
        );
        assert_eq!(repository.changes[0].scope.level, repository_scope.level);
    }

    #[test]
    fn tool_output_history_pages_are_hard_capped() {
        let svc = ready_service();
        for i in 0..(MAX_HISTORY_PAGE + 25) {
            svc.update_tool_output_defaults(
                actor(),
                ToolOutputSettingsScope::workspace(),
                ToolOutputOverrides {
                    max_inline_lines: Some(200 + (i % 20) as u32),
                    ..ToolOutputOverrides::default()
                },
                format!("cap limit test {i}"),
            )
            .expect("workspace output update should succeed");
        }

        let first = svc
            .list_tool_output_settings_history_page(ListToolOutputSettingsHistoryRequest {
                limit: Some(MAX_HISTORY_PAGE + 1),
                ..Default::default()
            })
            .expect("history should be paginated");
        assert_eq!(first.changes.len(), MAX_HISTORY_PAGE);
        assert_eq!(first.next_page_token, Some(MAX_HISTORY_PAGE.to_string()));

        let second = svc
            .list_tool_output_settings_history_page(ListToolOutputSettingsHistoryRequest {
                cursor: first.next_page_token,
                ..Default::default()
            })
            .expect("second history page should succeed");
        assert_eq!(second.changes.len(), 25);
        assert_eq!(second.next_page_token, None);
    }

    #[test]
    fn tool_output_history_limit_zero_rejects_malformed_cursor() {
        let svc = ready_service();
        let err = svc
            .list_tool_output_settings_history_page(ListToolOutputSettingsHistoryRequest {
                limit: Some(0),
                cursor: Some("not-a-cursor".to_string()),
                ..Default::default()
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
    }

    #[test]
    fn tool_output_history_limit_zero_rejects_offset_and_cursor() {
        let svc = ready_service();
        let err = svc
            .list_tool_output_settings_history_page(ListToolOutputSettingsHistoryRequest {
                limit: Some(0),
                offset: Some(0),
                cursor: Some("0".to_string()),
                ..Default::default()
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
    }

    #[test]
    fn tool_output_history_filters_scope_before_paging() {
        let svc = ready_service();
        for i in 0..3 {
            svc.update_tool_output_defaults(
                actor(),
                ToolOutputSettingsScope::workspace(),
                ToolOutputOverrides {
                    max_inline_bytes: Some(300 + (i as u64)),
                    ..ToolOutputOverrides::default()
                },
                format!("global {i}"),
            )
            .expect("workspace output update should succeed");
        }

        let repository_scope = ToolOutputSettingsScope::repository(RepositoryId::new());
        for i in 0..5 {
            svc.update_tool_output_defaults(
                actor(),
                repository_scope.clone(),
                ToolOutputOverrides {
                    max_inline_lines: Some(100 + i),
                    ..ToolOutputOverrides::default()
                },
                format!("repository {i}"),
            )
            .expect("repository output update should succeed");
        }

        let first = svc
            .list_tool_output_settings_history_page(ListToolOutputSettingsHistoryRequest {
                scope: Some(repository_scope.clone()),
                limit: Some(2),
                ..Default::default()
            })
            .expect("history should be available");
        assert_eq!(first.changes.len(), 2);
        assert_eq!(first.next_page_token, Some("2".to_string()));
        assert!(first
            .changes
            .iter()
            .all(|change| change.scope == repository_scope));

        let second = svc
            .list_tool_output_settings_history_page(ListToolOutputSettingsHistoryRequest {
                scope: Some(repository_scope.clone()),
                cursor: first.next_page_token,
                ..Default::default()
            })
            .expect("next history page should be available");
        assert_eq!(second.changes.len(), 3);
        assert_eq!(second.next_page_token, None);
        assert!(second
            .changes
            .iter()
            .all(|change| change.scope == repository_scope));
    }

    // -- job lifecycle API --------------------------------------------------

    fn actor() -> Actor {
        Actor::Agent {
            id: "agent:test".to_string(),
            display_name: Some("Test Agent".to_string()),
        }
    }

    fn actor_two() -> Actor {
        Actor::User {
            id: "user:test-two".to_string(),
            display_name: Some("Second Actor".to_string()),
        }
    }

    #[test]
    fn submit_get_and_list_job_round_trip() {
        let svc = ready_service();
        let root = test_repo_dir("job-round");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "summarize status".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .expect("submit should succeed");

        assert_eq!(receipt.job.status, JobStatus::Succeeded);

        let fetched = svc.get_job(&receipt.job.id).expect("get should succeed");
        assert_eq!(fetched.id, receipt.job.id);

        let page = svc
            .list_jobs(
                Some(repository.id),
                Some(JobStatus::Succeeded),
                None,
                None,
                None,
            )
            .expect("list jobs should succeed");
        assert_eq!(page.jobs.len(), 1);
        assert_eq!(page.jobs[0].id, receipt.job.id);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_jobs_request_forwards_pagination() {
        let svc = ready_service();
        let root = test_repo_dir("job-list-pagination");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-pagination-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        let repository_id = repository.id;

        svc.submit_job(SubmitJobRequest {
            requester: actor(),
            repository_id: repository_id.clone(),
            kind: JobKind::Read,
            goal: "first".to_string(),
            resource_scope: None,
            requested_capabilities: Vec::new(),
            idempotency_key: None,
        })
        .expect("submit should succeed");
        svc.submit_job(SubmitJobRequest {
            requester: actor(),
            repository_id: repository_id.clone(),
            kind: JobKind::Read,
            goal: "second".to_string(),
            resource_scope: None,
            requested_capabilities: Vec::new(),
            idempotency_key: None,
        })
        .expect("submit should succeed");

        let first = svc
            .list_jobs(
                Some(repository_id.clone()),
                Some(JobStatus::Succeeded),
                None,
                Some(1),
                None,
            )
            .expect("list jobs should succeed");
        assert_eq!(first.jobs.len(), 1);
        assert!(first.next_page_token.is_some());

        let second = svc
            .list_jobs(
                Some(repository_id),
                Some(JobStatus::Succeeded),
                None,
                Some(1),
                first.next_page_token,
            )
            .expect("list jobs should succeed");
        assert_eq!(second.jobs.len(), 1);
        assert_ne!(second.jobs[0].id, first.jobs[0].id);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_uses_tool_output_settings_for_runtime_rendering() {
        let svc = ready_service();
        let root = test_repo_dir("submit-job-tool-output-settings");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "submit-job-tool-output-settings".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        svc.update_tool_output_defaults(
            actor(),
            ToolOutputSettingsScope::repository(repository.id.clone())
                .for_tool(SECRETARY_ECHO_TOOL_ID),
            ToolOutputOverrides {
                granularity: Some(atelia_core::ToolOutputGranularity::Summary),
                ..ToolOutputOverrides::default()
            },
            "Compact secretary echo runtime output".to_string(),
        )
        .expect("tool output settings update should succeed");

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: "summarize the runtime output".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .expect("submit should succeed");
        let rendered = receipt
            .rendered_output
            .expect("rendered output should exist");

        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("render policy compacted output"));
        assert!(rendered.body.contains("summary"));
        assert!(!rendered
            .body
            .lines()
            .any(|line| line.starts_with("  goal,")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_empty_goal() {
        let svc = ready_service();
        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: RepositoryId::new(),
                kind: JobKind::Read,
                goal: " ".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
    }

    #[test]
    fn submit_job_accepts_supported_requested_capabilities() {
        let svc = ready_service();
        let root = test_repo_dir("unsupported-capabilities");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let first = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "summarize".to_string(),
                resource_scope: None,
                requested_capabilities: vec!["policy.check".to_string()],
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("submit should succeed");
        assert_eq!(
            first.policy_decision.requested_capability,
            "capability.discovery"
        );
        let second = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: "summarize".to_string(),
                resource_scope: None,
                requested_capabilities: vec!["capability.discovery".to_string()],
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("normalized alias should replay the same job");

        assert_eq!(first.job.id, second.job.id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_treats_empty_requested_capabilities_like_capability_discovery() {
        let svc = ready_service();
        let root = test_repo_dir("empty-capability-idempotency");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let first = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "summarize".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("submit should succeed");

        let second = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: "summarize".to_string(),
                resource_scope: None,
                requested_capabilities: vec!["capability.discovery".to_string()],
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("normalized capability should replay the same job");

        assert_eq!(first.job.id, second.job.id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_unsupported_requested_capabilities() {
        let svc = ready_service();
        let root = test_repo_dir("unsupported-capabilities");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: "summarize".to_string(),
                resource_scope: None,
                requested_capabilities: vec!["filesystem.write".to_string()],
                idempotency_key: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_replays_idempotent_requests() {
        let svc = ready_service();
        let root = test_repo_dir("supported-idempotency");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let first = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "summarize".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("first submit should succeed");

        let second = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: "summarize".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("replayed submit should return same job");

        assert_eq!(second.job.id, first.job.id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_idempotent_requests_are_serialized_concurrently() {
        let svc = Arc::new(ready_service());
        let root = test_repo_dir("concurrent-idempotency");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let repository_id = repository.id;
        let mut handles = Vec::with_capacity(8);
        for _ in 0..8 {
            let svc = svc.clone();
            let repository_id = repository_id.clone();
            handles.push(thread::spawn(move || {
                svc.submit_job(SubmitJobRequest {
                    requester: actor(),
                    repository_id,
                    kind: JobKind::Read,
                    goal: "summarize".to_string(),
                    resource_scope: None,
                    requested_capabilities: Vec::new(),
                    idempotency_key: Some("shared-key".to_string()),
                })
            }));
        }

        let job_ids = handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .expect("worker thread should finish")
                    .expect("submit should succeed")
                    .job
                    .id
                    .as_str()
                    .to_string()
            })
            .collect::<HashSet<_>>();
        assert_eq!(job_ids.len(), 1);

        let page = svc
            .list_jobs(
                Some(repository_id),
                Some(JobStatus::Succeeded),
                None,
                None,
                None,
            )
            .expect("list jobs should succeed");
        assert_eq!(page.jobs.len(), 1);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_conflicting_idempotent_request() {
        let svc = ready_service();
        let root = test_repo_dir("conflicting-idempotency");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let first = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "summarize".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("first submit should succeed");

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor_two(),
                repository_id: first.job.repository_id,
                kind: JobKind::Read,
                goal: "different summary".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("request-123".to_string()),
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::Conflict { reason: _ }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn list_jobs_request_filters_requester() {
        let svc = ready_service();
        let root = test_repo_dir("job-list-requester");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        let repository_id = repository.id;

        svc.submit_job(SubmitJobRequest {
            requester: actor(),
            repository_id: repository_id.clone(),
            kind: JobKind::Read,
            goal: "from-first".to_string(),
            resource_scope: None,
            requested_capabilities: Vec::new(),
            idempotency_key: None,
        })
        .expect("first submit should succeed");
        svc.submit_job(SubmitJobRequest {
            requester: actor_two(),
            repository_id: repository_id.clone(),
            kind: JobKind::Read,
            goal: "from-second".to_string(),
            resource_scope: None,
            requested_capabilities: Vec::new(),
            idempotency_key: None,
        })
        .expect("second submit should succeed");

        let first_actor_jobs = svc
            .list_jobs(
                Some(repository_id.clone()),
                Some(JobStatus::Succeeded),
                Some(actor()),
                Some(10),
                None,
            )
            .expect("list first actor jobs");
        assert_eq!(first_actor_jobs.jobs.len(), 1);
        assert_eq!(first_actor_jobs.jobs[0].requester, actor());

        let second_actor_jobs = svc
            .list_jobs(
                Some(repository_id.clone()),
                Some(JobStatus::Succeeded),
                Some(actor_two()),
                Some(10),
                None,
            )
            .expect("list second actor jobs");
        assert_eq!(second_actor_jobs.jobs.len(), 1);
        assert_eq!(second_actor_jobs.jobs[0].requester, actor_two());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cancel_job_preserves_requester_for_follow_up_job_queries() {
        let svc = ready_service();
        let root = test_repo_dir("cancel-requester");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        let repository_id = repository.id.clone();

        let queued_job = JobRecord::new(
            actor(),
            repository_id.clone(),
            JobKind::Read,
            "manual-cancel".to_string(),
            atelia_core::LedgerTimestamp::from_unix_millis(2_000),
        );
        let initial_event = atelia_core::JobEvent {
            id: atelia_core::JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: atelia_core::LedgerTimestamp::from_unix_millis(2_001),
            subject: atelia_core::EventSubject::job(&queued_job.id),
            kind: atelia_core::JobEventKind::JobSubmitted,
            severity: atelia_core::EventSeverity::Info,
            public_message: "queued".to_string(),
            refs: atelia_core::EventRefs {
                repository_id: Some(repository_id.clone()),
                job_id: Some(queued_job.id.clone()),
                ..Default::default()
            },
            redactions: Vec::new(),
        };
        svc.lifecycle
            .runtime()
            .store()
            .create_job_with_initial_event(queued_job.clone(), initial_event)
            .expect("seeded queued job should be inserted");

        let receipt = svc
            .cancel_job(&queued_job.id, "test cancel".to_string(), Some(actor()))
            .expect("queued job should be cancellable");
        assert_eq!(receipt.job.id, queued_job.id);

        let requester = svc
            .cancellation_requester(&receipt.job.id)
            .expect("cancel requester should be tracked");
        assert_eq!(requester, actor());

        let updated = svc
            .list_jobs(
                Some(repository_id.clone()),
                Some(JobStatus::Canceled),
                None,
                None,
                None,
            )
            .expect("list should return canceled job");
        assert_eq!(updated.jobs.len(), 1);
        assert_eq!(
            svc.cancellation_requester(&updated.jobs[0].id)
                .expect("listed job should keep cancel requester"),
            actor()
        );
        let fetched = svc
            .get_job(&queued_job.id)
            .expect("get should return canceled job");
        assert_eq!(
            svc.cancellation_requester(&fetched.id)
                .expect("get_job should keep cancel requester"),
            actor()
        );

        let _ = fs::remove_dir_all(root);
    }

    // -- whitespace-only validation tests ------------------------------------

    #[test]
    fn register_rejects_whitespace_only_display_name() {
        let svc = ready_service();
        let err = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "   ".to_string(),
                root_path: "/tmp/test".to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
    }

    #[test]
    fn register_rejects_whitespace_only_root_path() {
        let svc = ready_service();
        let err = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "test-repo".to_string(),
                root_path: "  \t ".to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
    }

    // -- DAEMON_VERSION derived from Cargo.toml -----------------------------

    #[test]
    fn daemon_version_matches_cargo_pkg_version() {
        assert_eq!(DAEMON_VERSION, env!("CARGO_PKG_VERSION"));
    }
}
