//! Daemon service skeleton for Atelia Secretary (Slice 4).
//!
//! Owns daemon health/status metadata, an in-memory job lifecycle runtime, and
//! exposes a synchronous service API for health checks, repository
//! registration/listing, and the first supported job lifecycle calls.

use atelia_core::policy::{REASON_REPOSITORY_BLOCKED, SCOPE_KIND_REPOSITORY, SCOPE_VALUE_ROOT};
use atelia_core::{
    canonicalize_job_requested_capability, canonicalize_within_scope,
    render_tool_result_with_policy, Actor, ApplyBlocklistRequest, ApplyBlocklistResponse,
    CancelJobReceipt, DefaultPolicyEngine, DisableExtensionRequest, DisableExtensionResponse,
    EchoTool, EnableExtensionRequest, EnableExtensionResponse, EventCursor, EventPage, EventQuery,
    ExtensionRegistryService, ExtensionStatusRequest, ExtensionStatusResponse, FsReadTool,
    InMemoryStore, InMemoryToolOutputSettingsService, InstallExtensionRequest,
    InstallExtensionResponse, JobEvent, JobId, JobKind, JobLifecycleService, JobPage, JobQuery,
    JobRecord, JobStatus, LedgerTimestamp, ListBlocklistRequest, ListBlocklistResponse,
    ListExtensionsRequest, ListExtensionsResponse, OutputFormat, PathScope, PolicyDecision,
    PolicyEngine, PolicyInput, PolicyOutcome, RegistryError, RemoveExtensionRequest,
    RemoveExtensionResponse, RenderedToolOutput, RepositoryId, RepositoryRecord,
    RepositoryTrustState, ResourceScope, RollbackExtensionRequest, RollbackExtensionResponse,
    RuntimeError, RuntimeJobReceipt, RuntimeJobRequest, SecretaryStore, StoreError,
    SubmitJobIdempotencyRecord, ToolInvocationId, ToolOutputDefaults, ToolOutputOverrides,
    ToolOutputSettingsChange, ToolOutputSettingsError, ToolOutputSettingsScope, ToolResultId,
    TruncationMetadata, UpdateExtensionRequest, UpdateExtensionResponse,
};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "1.0.0";
const STORAGE_VERSION: &str = "0.1.0";
pub(crate) const EXTENSION_EXECUTION_UNAVAILABLE_REASON: &str =
    "extension execution is not implemented in the Secretary beta; use extension install, status, and blocklist management APIs instead";
const DAEMON_CAPABILITIES: &[&str] = &[
    "health.v1",
    "repositories.v1",
    "jobs.v1",
    "events.v1",
    "policy.v1",
    "repertoire.v1",
    "extensions.registry.v1",
    "tool_output_settings.v1",
    "tool_output_render.v1",
    "project_status.v1",
];
const MAX_HISTORY_PAGE: usize = 1000;
const SECRETARY_ECHO_TOOL_ID: &str = "secretary.echo";
const SECRETARY_ECHO_TOOL_NAME: &str = "Secretary Echo";
const SECRETARY_ECHO_TOOL_DESCRIPTION: &str =
    "Echo input for daemon smoke tests and context probes.";
const SECRETARY_FS_READ_TOOL_ID: &str = "fs.read";
const SECRETARY_FS_READ_TOOL_NAME: &str = "Filesystem Read";
const SECRETARY_FS_READ_TOOL_DESCRIPTION: &str = "Read a file from an allowed repository scope.";
const SECRETARY_TOOL_PROVIDER_KIND: &str = "builtin";
const SECRETARY_TOOL_PROVIDER_ID: &str = "atelia-secretary";
const SECRETARY_TOON_FORMAT: &str = "toon";
const SECRETARY_JSON_FORMAT: &str = "json";
const SECRETARY_FS_READ_CAPABILITY: &str = "filesystem.read";
const SECRETARY_CAPABILITY_DISCOVERY: &str = "capability.discovery";

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
    /// Beta-only state durability hint exposed so clients understand restart semantics.
    pub beta_state: Option<BetaStateHint>,
    pub daemon_version: String,
    pub protocol_version: String,
    pub storage_version: String,
    pub capabilities: Vec<String>,
    pub repository_count: usize,
    pub started_at: LedgerTimestamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BetaStateHint {
    /// Process or storage boundary that owns the state.
    pub scope: String,
    /// Durability class for the current beta daemon state.
    pub durability: String,
    /// Observable behavior clients should expect across daemon restarts.
    pub restart_semantics: String,
    /// Stable code tokens attached to this beta durability class.
    pub limits: Vec<String>,
}

impl BetaStateHint {
    /// Build the beta hint for the process-local in-memory store.
    pub fn in_memory_process_local() -> Self {
        Self {
            scope: "process_local".to_string(),
            durability: "in_memory".to_string(),
            restart_semantics: "reset_on_restart".to_string(),
            limits: vec![
                "state_is_limited_to_the_current_daemon_process".to_string(),
                "state_is_not_recovered_after_restart".to_string(),
            ],
        }
    }

    /// Build the beta hint for the durable ledger snapshot store.
    pub fn durable_snapshot_replay() -> Self {
        Self {
            scope: "storage_backed".to_string(),
            durability: "durable_snapshot".to_string(),
            restart_semantics: "restore_on_restart".to_string(),
            limits: vec![
                "state_is_persisted_to_ledger_json".to_string(),
                "state_is_validated_on_startup".to_string(),
                "state_is_restored_after_restart".to_string(),
            ],
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
    UnsupportedCapability { reason: String },
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
            Self::UnsupportedCapability { reason } => {
                write!(f, "unsupported capability: {reason}")
            }
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

/// Request DTO for `submit_job`, including an optional idempotency key for replay-safe retries.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct SubmitJobRequest {
    pub requester: Actor,
    pub repository_id: RepositoryId,
    pub kind: JobKind,
    pub goal: String,
    pub resource_scope: Option<ResourceScope>,
    pub requested_capabilities: Vec<String>,
    /// Optional caller-provided key used to deduplicate successful retries.
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmitJobToolKind {
    Echo,
    FsRead,
}

impl SubmitJobToolKind {
    fn tool_id(self) -> &'static str {
        match self {
            Self::Echo => SECRETARY_ECHO_TOOL_ID,
            Self::FsRead => SECRETARY_FS_READ_TOOL_ID,
        }
    }
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

/// Request for the beta repertoire projection.
#[derive(Debug, Clone, Default)]
pub struct ListRepertoireRequest;

/// Response containing the built-in tools exposed through the beta repertoire.
#[derive(Debug, Clone)]
pub struct ListRepertoireResponse {
    /// Projected repertoire entries in stable display order.
    pub entries: Vec<RepertoireEntry>,
}

/// Public repertoire view for a single built-in tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepertoireEntry {
    /// Stable tool identifier.
    pub tool_id: String,
    /// Human-readable tool name.
    pub name: String,
    /// Concise description shown to clients.
    pub description: String,
    /// Provider category for the tool implementation.
    pub provider_kind: String,
    /// Stable provider identifier.
    pub provider_id: String,
    /// Risk tier used for client-side policy and presentation.
    pub risk_tier: String,
    /// Default result format returned by the tool.
    pub default_result_format: String,
    /// Result formats the tool can emit.
    pub supported_result_formats: Vec<String>,
    /// Idempotency classification for repeated calls.
    pub idempotency: String,
    /// Whether the tool can be cancelled after dispatch.
    pub cancellable: bool,
    /// Whether the tool streams partial results.
    pub streaming: bool,
    /// Advertised timeout budget in milliseconds; `0` means not enforced yet.
    pub timeout_ms: u32,
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
pub struct ExtensionExecutionRequest {
    pub extension_id: String,
}

#[derive(Debug, Clone)]
pub struct ExtensionExecutionResponse {
    pub metadata: ProtocolMetadata,
}

/// In-memory cache entry for a previously committed successful submit-job call.
#[derive(Debug, Clone)]
struct IdempotentSubmitJob {
    /// Canonical request signature used to validate replay compatibility.
    signature: String,
    /// The receipt returned for the original successful submission.
    receipt: RuntimeJobReceipt,
}

const IDEMPOTENT_SUBMISSION_CACHE_LIMIT: usize = 256;

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
    idempotent_submissions: Mutex<VecDeque<(String, IdempotentSubmitJob)>>,
    idempotent_submission_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    cancellation_requesters: Mutex<HashMap<JobId, Actor>>,
}

#[allow(dead_code)]
pub struct LiveEventSubscription {
    pub events: Vec<JobEvent>,
    pub receiver: broadcast::Receiver<JobEvent>,
    pub replay_max_sequence: u64,
}

impl SecretaryService {
    /// Create a new service backed by an in-memory store and default policy.
    pub fn new() -> Self {
        Self::from_store(InMemoryStore::new())
    }

    pub fn new_durable(storage_dir: impl Into<PathBuf>) -> ServiceResult<Self> {
        let store = InMemoryStore::with_durable_storage_dir(storage_dir)?;
        Ok(Self::from_store(store))
    }

    fn from_store(store: InMemoryStore) -> Self {
        Self {
            lifecycle: JobLifecycleService::new(atelia_core::SecretaryRuntime::new(
                store,
                DefaultPolicyEngine::new(),
            )),
            started_at: LedgerTimestamp::now(),
            daemon_status: DaemonStatus::Starting,
            extension_registry: Mutex::new(ExtensionRegistryService::new()),
            tool_output_settings: Mutex::new(InMemoryToolOutputSettingsService::new(
                LedgerTimestamp::now(),
            )),
            idempotent_submissions: VecDeque::new().into(),
            idempotent_submission_locks: HashMap::new().into(),
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

        let beta_state = if self.lifecycle.runtime().store().uses_durable_snapshot() {
            BetaStateHint::durable_snapshot_replay()
        } else {
            BetaStateHint::in_memory_process_local()
        };

        DaemonHealth {
            daemon_status: self.daemon_status,
            storage_status,
            beta_state: Some(beta_state),
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

    /// Find a blocked policy decision whose scope overlaps the repository root being registered.
    fn blocking_policy_decision_for_root(
        &self,
        root_path: &str,
    ) -> ServiceResult<Option<atelia_core::PolicyDecision>> {
        let candidate_root = Path::new(root_path);
        let repository_roots = self
            .lifecycle
            .runtime()
            .store()
            .list_repositories()?
            .into_iter()
            .map(|repository| (repository.id, PathBuf::from(repository.root_path)))
            .collect::<HashMap<_, _>>();

        Ok(blocking_policy_decision_for_candidate_root(
            candidate_root,
            repository_roots,
            self.lifecycle.runtime().store().list_policy_decisions()?,
        ))
    }

    /// Find a blocked repository whose root overlaps the repository root being registered.
    fn blocked_repository_ancestor_for_root(
        &self,
        root_path: &str,
    ) -> ServiceResult<Option<RepositoryRecord>> {
        let candidate_root = Path::new(root_path);
        Ok(self
            .lifecycle
            .runtime()
            .store()
            .list_repositories()?
            .into_iter()
            .find(|repository| {
                repository.trust_state == RepositoryTrustState::Blocked
                    && roots_strictly_overlap(candidate_root, Path::new(&repository.root_path))
            }))
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

        if let Some(blocked_repository) =
            self.blocked_repository_ancestor_for_root(&record.root_path)?
        {
            return Err(ServiceError::Conflict {
                reason: format!(
                    "root_path is blocked by repository {}",
                    blocked_repository.id.as_str()
                ),
            });
        }

        if let Some(blocked_policy_decision) =
            self.blocking_policy_decision_for_root(&record.root_path)?
        {
            return Err(ServiceError::Conflict {
                reason: format!(
                    "root_path is blocked by policy decision {}",
                    blocked_policy_decision.id.as_str()
                ),
            });
        }

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

    /// Project the implemented built-in tool surface for beta repertoire clients.
    pub fn list_repertoire(
        &self,
        _request: ListRepertoireRequest,
    ) -> ServiceResult<ListRepertoireResponse> {
        Ok(ListRepertoireResponse {
            entries: list_repertoire_entries(),
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

    /// Submit a supported daemon job, dispatching the echo tool by default
    /// and `fs.read` when the request asks for a filesystem read.
    #[allow(dead_code)]
    pub fn submit_job(&self, request: SubmitJobRequest) -> ServiceResult<RuntimeJobReceipt> {
        if request.goal.trim().is_empty() {
            return Err(ServiceError::InvalidArgument {
                reason: "goal must not be empty".to_string(),
            });
        }
        let normalized_goal = request.goal.trim().to_string();

        let requested_capabilities =
            normalize_requested_capabilities(&request.requested_capabilities)?;
        let tool_kind = resolve_submit_job_tool_kind(&request, &requested_capabilities)?;
        let repository = self.get_repository(&request.repository_id)?;

        let normalized_idempotency_key = match request.idempotency_key.as_ref() {
            Some(idempotency_key) => {
                let trimmed = idempotency_key.trim();
                if trimmed.is_empty() {
                    return Err(ServiceError::InvalidArgument {
                        reason: "idempotency_key must not be blank".to_string(),
                    });
                }
                Some(trimmed.to_string())
            }
            None => None,
        };
        let request_signature =
            submit_job_request_signature(&request, &normalized_goal, &requested_capabilities);
        let tool_output_defaults = self.get_tool_output_defaults(
            ToolOutputSettingsScope::repository(repository.id.clone())
                .for_tool(tool_kind.tool_id()),
        )?;

        let resource_scope = request.resource_scope.unwrap_or_else(|| ResourceScope {
            kind: "repository".to_string(),
            value: ".".to_string(),
        });
        if matches!(tool_kind, SubmitJobToolKind::FsRead) {
            validate_filesystem_read_scope(&repository, &resource_scope)?;
        }

        let runtime_request = RuntimeJobRequest::new(
            request.requester,
            request.repository_id,
            request.kind,
            normalized_goal,
        )
        .with_requested_capabilities(requested_capabilities)
        .with_tool_output_defaults(tool_output_defaults)
        .with_resource_scope(resource_scope.kind, resource_scope.value);

        let receipt = if let Some(idempotency_key) = normalized_idempotency_key.as_deref() {
            let key_lock = {
                let mut locks = self.idempotent_submission_locks.lock().map_err(|err| {
                    ServiceError::Internal {
                        reason: format!("idempotency lock map poisoned: {err}"),
                    }
                })?;
                locks
                    .entry(idempotency_key.to_string())
                    .or_insert_with(|| Arc::new(Mutex::new(())))
                    .clone()
            };
            let _key_guard = key_lock.lock().map_err(|err| ServiceError::Internal {
                reason: format!("idempotency key lock poisoned: {err}"),
            })?;

            let receipt_result = (|| -> ServiceResult<RuntimeJobReceipt> {
                {
                    let cache = self.idempotent_submissions.lock().map_err(|err| {
                        ServiceError::Internal {
                            reason: format!("idempotency cache lock poisoned: {err}"),
                        }
                    })?;

                    if let Some(cached) = cache
                        .iter()
                        .rev()
                        .find(|(cached_key, _)| cached_key.as_str() == idempotency_key)
                        .map(|(_, cached)| cached)
                    {
                        if cached.signature == request_signature {
                            return Ok(cached.receipt.clone());
                        }
                        return Err(ServiceError::Conflict {
                            reason:
                                "idempotency_key was previously used for a different submit request"
                                    .to_string(),
                        });
                    }
                }

                if let Some(stored) = self
                    .lifecycle
                    .runtime()
                    .store()
                    .get_submit_job_idempotency(idempotency_key)?
                {
                    if stored.signature == request_signature {
                        let receipt = stored.receipt.clone();
                        let mut cache = self.idempotent_submissions.lock().map_err(|err| {
                            ServiceError::Internal {
                                reason: format!("idempotency cache lock poisoned: {err}"),
                            }
                        })?;
                        cache_idempotent_submission(
                            &mut cache,
                            idempotency_key.to_string(),
                            IdempotentSubmitJob {
                                signature: stored.signature,
                                receipt: receipt.clone(),
                            },
                        );
                        return Ok(receipt);
                    }

                    return Err(ServiceError::Conflict {
                        reason:
                            "idempotency_key was previously used for a different submit request"
                                .to_string(),
                    });
                }

                let receipt = match tool_kind {
                    SubmitJobToolKind::Echo => {
                        self.lifecycle.runtime().run_tool_job_with_finalizer(
                            runtime_request.clone(),
                            &EchoTool,
                            Some(make_submit_job_finalizer(
                                idempotency_key.to_string(),
                                request_signature.clone(),
                            )),
                        )?
                    }
                    SubmitJobToolKind::FsRead => {
                        let tool = FsReadTool::new(&repository.root_path);
                        self.lifecycle.runtime().run_tool_job_with_finalizer(
                            runtime_request.clone(),
                            &tool,
                            Some(make_submit_job_finalizer(
                                idempotency_key.to_string(),
                                request_signature.clone(),
                            )),
                        )?
                    }
                };
                if receipt.job.status == JobStatus::Succeeded {
                    let mut cache = self.idempotent_submissions.lock().map_err(|err| {
                        ServiceError::Internal {
                            reason: format!("idempotency cache lock poisoned: {err}"),
                        }
                    })?;
                    cache_idempotent_submission(
                        &mut cache,
                        idempotency_key.to_string(),
                        IdempotentSubmitJob {
                            signature: request_signature.clone(),
                            receipt: receipt.clone(),
                        },
                    );
                }

                Ok(receipt)
            })();

            drop(_key_guard);
            drop(key_lock);
            {
                let mut locks = self.idempotent_submission_locks.lock().map_err(|err| {
                    ServiceError::Internal {
                        reason: format!("idempotency lock map poisoned: {err}"),
                    }
                })?;
                if let Some(existing) = locks.get(idempotency_key) {
                    if Arc::strong_count(existing) == 1 {
                        locks.remove(idempotency_key);
                    }
                }
            }

            receipt_result?
        } else {
            match tool_kind {
                SubmitJobToolKind::Echo => self.lifecycle.runtime().run_tool_job_with_finalizer(
                    runtime_request,
                    &EchoTool,
                    None::<fn(&RuntimeJobReceipt) -> Option<(String, SubmitJobIdempotencyRecord)>>,
                )?,
                SubmitJobToolKind::FsRead => {
                    let tool = FsReadTool::new(&repository.root_path);
                    self.lifecycle.runtime().run_tool_job_with_finalizer(
                        runtime_request,
                        &tool,
                        None::<
                            fn(&RuntimeJobReceipt) -> Option<(String, SubmitJobIdempotencyRecord)>,
                        >,
                    )?
                }
            }
        };

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

    /// Subscribe to future events while returning the initial replay slice.
    #[allow(dead_code)]
    pub fn watch_events_live(&self, query: EventQuery) -> ServiceResult<LiveEventSubscription> {
        let receiver = self.lifecycle.runtime().store().subscribe_job_events();
        let events = self.list_events_page(query)?.events;
        let replay_max_sequence = events
            .last()
            .map(|event| event.sequence_number)
            .unwrap_or(0);
        Ok(LiveEventSubscription {
            events,
            receiver,
            replay_max_sequence,
        })
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

    pub fn execute_extension(
        &self,
        request: ExtensionExecutionRequest,
    ) -> ServiceResult<ExtensionExecutionResponse> {
        let _ = request.extension_id;
        Err(ServiceError::UnsupportedCapability {
            reason: EXTENSION_EXECUTION_UNAVAILABLE_REASON.to_string(),
        })
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
    let mut retained = Vec::with_capacity(page_size.min(1024));
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

fn list_repertoire_entries() -> Vec<RepertoireEntry> {
    fn entry(
        tool_id: &str,
        name: &str,
        description: &str,
        risk_tier: &str,
        idempotency: &str,
        cancellable: bool,
        timeout_ms: u32,
    ) -> RepertoireEntry {
        RepertoireEntry {
            tool_id: tool_id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
            provider_kind: SECRETARY_TOOL_PROVIDER_KIND.to_string(),
            provider_id: SECRETARY_TOOL_PROVIDER_ID.to_string(),
            risk_tier: risk_tier.to_string(),
            default_result_format: SECRETARY_TOON_FORMAT.to_string(),
            supported_result_formats: vec![
                SECRETARY_TOON_FORMAT.to_string(),
                SECRETARY_JSON_FORMAT.to_string(),
            ],
            idempotency: idempotency.to_string(),
            cancellable,
            streaming: false,
            timeout_ms,
        }
    }

    let mut entries = vec![
        entry(
            SECRETARY_FS_READ_TOOL_ID,
            SECRETARY_FS_READ_TOOL_NAME,
            SECRETARY_FS_READ_TOOL_DESCRIPTION,
            "R1",
            "idempotent",
            false,
            0,
        ),
        entry(
            SECRETARY_ECHO_TOOL_ID,
            SECRETARY_ECHO_TOOL_NAME,
            SECRETARY_ECHO_TOOL_DESCRIPTION,
            "R0",
            "idempotent",
            false,
            0,
        ),
    ];

    entries.sort_by(|left, right| left.tool_id.cmp(&right.tool_id));
    entries
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

/// Return `true` when a policy decision blocks repository registration at the
/// repository root scope.
fn is_repository_registration_block(policy_decision: &atelia_core::PolicyDecision) -> bool {
    policy_decision.outcome == PolicyOutcome::Blocked
        && policy_decision.reason_code.trim() == REASON_REPOSITORY_BLOCKED
        && policy_decision.resource_scope.kind.trim() == SCOPE_KIND_REPOSITORY
        && policy_decision.resource_scope.value.trim() == SCOPE_VALUE_ROOT
}

/// Return `true` when two repository roots are distinct and one is a strict
/// ancestor of the other.
fn roots_strictly_overlap(candidate_root: &Path, blocked_root: &Path) -> bool {
    candidate_root != blocked_root
        && (candidate_root.starts_with(blocked_root) || blocked_root.starts_with(candidate_root))
}

/// Find the first blocked policy decision whose canonical scope contains the candidate root.
fn blocking_policy_decision_for_candidate_root(
    candidate_root: &Path,
    repository_roots: HashMap<RepositoryId, PathBuf>,
    policy_decisions: Vec<atelia_core::PolicyDecision>,
) -> Option<atelia_core::PolicyDecision> {
    for policy_decision in policy_decisions {
        if !is_repository_registration_block(&policy_decision) {
            continue;
        }

        let Some(blocked_repository_root) = repository_roots.get(&policy_decision.repository_id)
        else {
            continue;
        };
        let blocked_scope = match canonicalize_within_scope(
            blocked_repository_root,
            Path::new(policy_decision.resource_scope.value.trim()),
        ) {
            Ok(scope) => scope,
            Err(_) => continue,
        };

        if roots_strictly_overlap(candidate_root, blocked_scope.canonical.as_path()) {
            return Some(policy_decision);
        }
    }

    None
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

        let canonical = canonicalize_submit_requested_capability(trimmed).ok_or_else(|| {
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

fn canonicalize_submit_requested_capability(name: &str) -> Option<&'static str> {
    canonicalize_job_requested_capability(name).or_else(|| {
        let normalized = name
            .trim()
            .to_ascii_lowercase()
            .replace(['_', '-', ':', '/'], ".");

        match normalized.as_str() {
            SECRETARY_FS_READ_CAPABILITY => Some(SECRETARY_FS_READ_CAPABILITY),
            _ => None,
        }
    })
}

fn resolve_submit_job_tool_kind(
    request: &SubmitJobRequest,
    requested_capabilities: &[String],
) -> ServiceResult<SubmitJobToolKind> {
    match requested_capabilities {
        [capability] if capability == SECRETARY_CAPABILITY_DISCOVERY => Ok(SubmitJobToolKind::Echo),
        [capability] if capability == SECRETARY_FS_READ_CAPABILITY => {
            let resource_scope =
                request
                    .resource_scope
                    .as_ref()
                    .ok_or_else(|| ServiceError::InvalidArgument {
                        reason: "filesystem.read requires a path_scope/resource_scope".to_string(),
                    })?;

            if !matches!(
                resource_scope.kind.trim(),
                "repository" | "explicit_paths" | "read_only" | "path"
            ) {
                return Err(ServiceError::InvalidArgument {
                    reason: "filesystem.read requires resource_scope.kind to be repository, explicit_paths, read_only, or path".to_string(),
                });
            }

            if matches!(resource_scope.value.trim(), "" | ".") {
                return Err(ServiceError::InvalidArgument {
                    reason: "filesystem.read requires a concrete path_scope/resource_scope root"
                        .to_string(),
                });
            }

            Ok(SubmitJobToolKind::FsRead)
        }
        [capability] => Err(ServiceError::InvalidArgument {
            reason: format!("requested_capabilities contains unsupported capability: {capability}"),
        }),
        _ => Err(ServiceError::InvalidArgument {
            reason: "requested_capabilities must resolve to exactly one supported capability"
                .to_string(),
        }),
    }
}

fn validate_filesystem_read_scope(
    repository: &RepositoryRecord,
    resource_scope: &ResourceScope,
) -> ServiceResult<()> {
    let root = Path::new(&repository.root_path);
    let requested =
        canonicalize_within_scope(root, Path::new(&resource_scope.value)).map_err(|err| {
            ServiceError::InvalidArgument {
                reason: format!("filesystem.read path is outside repository scope: {err}"),
            }
        })?;

    if requested.canonical == requested.root {
        return Err(ServiceError::InvalidArgument {
            reason: "filesystem.read requires a concrete path_scope/resource_scope root"
                .to_string(),
        });
    }

    let allowed = repository
        .allowed_path_scope
        .allowed_paths
        .iter()
        .any(|allowed_path| {
            canonicalize_within_scope(root, Path::new(allowed_path))
                .map(|allowed| requested.canonical.starts_with(&allowed.canonical))
                .unwrap_or(false)
        });

    if allowed {
        Ok(())
    } else {
        Err(ServiceError::InvalidArgument {
            reason: "filesystem.read path is outside allowed_path_scope".to_string(),
        })
    }
}

/// Build the canonical request signature used to compare idempotent submit-job retries.
fn submit_job_request_signature(
    request: &SubmitJobRequest,
    normalized_goal: &str,
    requested_capabilities: &[String],
) -> String {
    #[derive(Serialize)]
    struct SubmitJobRequestSignature<'a> {
        actor: &'a Actor,
        repository_id: &'a str,
        kind: &'a JobKind,
        goal: &'a str,
        resource_scope: &'a ResourceScope,
        requested_capabilities: &'a [String],
    }

    let resource_scope = request
        .resource_scope
        .clone()
        .unwrap_or_else(|| ResourceScope {
            kind: "repository".to_string(),
            value: ".".to_string(),
        });

    serde_json::to_string(&SubmitJobRequestSignature {
        actor: &request.requester,
        repository_id: request.repository_id.as_str(),
        kind: &request.kind,
        goal: normalized_goal,
        resource_scope: &resource_scope,
        requested_capabilities,
    })
    .expect("serialize canonical submit_job request signature")
}

fn cache_idempotent_submission(
    cache: &mut VecDeque<(String, IdempotentSubmitJob)>,
    idempotency_key: String,
    submission: IdempotentSubmitJob,
) {
    if let Some(position) = cache
        .iter()
        .position(|(cached_key, _)| cached_key == &idempotency_key)
    {
        cache.remove(position);
    }
    if cache.len() >= IDEMPOTENT_SUBMISSION_CACHE_LIMIT {
        cache.pop_front();
    }
    cache.push_back((idempotency_key, submission));
}

fn make_submit_job_finalizer(
    idempotency_key: String,
    request_signature: String,
) -> impl FnOnce(&RuntimeJobReceipt) -> Option<(String, SubmitJobIdempotencyRecord)> {
    move |receipt: &RuntimeJobReceipt| {
        if receipt.job.status == JobStatus::Succeeded {
            Some((
                idempotency_key.clone(),
                SubmitJobIdempotencyRecord {
                    signature: request_signature.clone(),
                    receipt: receipt.clone(),
                },
            ))
        } else {
            None
        }
    }
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
        InstallExtensionRequest, LedgerTimestamp, ListBlocklistRequest, ListExtensionsRequest,
        PolicyDecision, PolicyDecisionId, PolicyOutcome, ProvenanceSource, RepositoryTrustState,
        ResourceScope, RetryPolicy, RiskTier, RollbackExtensionRequest, EXTENSION_MANIFEST_SCHEMA,
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

    fn durable_storage_dir(name: &str) -> PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "atelia-service-durable-test-{}-{}-{name}",
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
        let beta_state = health.beta_state.expect("beta state hint");
        assert_eq!(beta_state.scope, "process_local");
        assert_eq!(beta_state.durability, "in_memory");
        assert_eq!(beta_state.restart_semantics, "reset_on_restart");
        assert!(beta_state
            .limits
            .contains(&"state_is_not_recovered_after_restart".to_string()));
        assert_eq!(health.daemon_version, DAEMON_VERSION);
        assert_eq!(health.protocol_version, PROTOCOL_VERSION);
        assert_eq!(health.storage_version, STORAGE_VERSION);
    }

    #[test]
    fn health_marks_durable_snapshot_restart_semantics() {
        let storage_dir = durable_storage_dir("health");
        let svc =
            SecretaryService::new_durable(storage_dir.clone()).expect("durable service reload");

        let health = svc.health();
        let beta_state = health.beta_state.expect("beta state hint");
        assert_eq!(beta_state.scope, "storage_backed");
        assert_eq!(beta_state.durability, "durable_snapshot");
        assert_eq!(beta_state.restart_semantics, "restore_on_restart");
        assert!(beta_state
            .limits
            .contains(&"state_is_validated_on_startup".to_string()));

        let _ = fs::remove_dir_all(storage_dir);
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
    fn extension_execution_is_explicitly_unavailable_in_beta() {
        let svc = ready_service();
        let err = svc
            .execute_extension(ExtensionExecutionRequest {
                extension_id: "com.example.review.extension".to_string(),
            })
            .expect_err("extension execution should be gated in beta");

        assert!(matches!(
            err,
            ServiceError::UnsupportedCapability { reason }
                if reason.contains("install, status, and blocklist")
        ));
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
    fn durable_service_replays_state_after_restart() {
        let storage_dir = durable_storage_dir("restart");
        let first_service =
            SecretaryService::new_durable(storage_dir.clone()).expect("durable service");
        let root = test_repo_dir("durable-restart");
        let repository = first_service
            .register_repository(RegisterRepositoryRequest {
                display_name: "durable-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("repository registration should succeed");
        let receipt = first_service
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "persist me".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("restart-key".to_string()),
            })
            .expect("job submission should succeed");
        let job_id = receipt.job.id.clone();
        assert!(first_service
            .lifecycle
            .runtime()
            .store()
            .get_submit_job_idempotency("restart-key")
            .expect("idempotency record should persist")
            .map(|record| record.receipt == receipt)
            .unwrap_or(false));
        drop(first_service);

        let second_service =
            SecretaryService::new_durable(storage_dir.clone()).expect("durable service reload");
        let repositories = second_service
            .list_repositories()
            .expect("repositories should reload");
        assert_eq!(repositories.len(), 1);
        assert_eq!(repositories[0].id, repository.id);

        let jobs = second_service
            .list_jobs(None, None, None, None, None)
            .expect("jobs should reload");
        assert_eq!(jobs.jobs.len(), 1);
        assert_eq!(jobs.jobs[0].id, job_id.clone());

        let replayed = second_service
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "persist me".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("restart-key".to_string()),
            })
            .expect("idempotent replay should return the stored receipt");
        assert_eq!(replayed.job.id, job_id);
        assert!(second_service
            .lifecycle
            .runtime()
            .store()
            .get_submit_job_idempotency("restart-key")
            .expect("idempotency record should reload")
            .map(|record| record.receipt == receipt)
            .unwrap_or(false));

        let status = second_service
            .get_project_status(GetProjectStatusRequest {
                repository_id: repository.id.clone(),
            })
            .expect("project status should reload");
        assert_eq!(status.recent_jobs.len(), 1);
        assert_eq!(status.recent_jobs[0].id, job_id.clone());
        assert!(status.latest_event.is_some());
        let replayed = second_service
            .watch_events(atelia_core::EventCursor::Beginning, None)
            .expect("event replay should succeed");
        assert!(!replayed.is_empty());
        assert!(replayed
            .iter()
            .any(|event| event.refs.job_id == Some(job_id.clone())));
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(storage_dir);
    }

    #[test]
    fn durable_service_conflicts_on_idempotency_signature_mismatch_after_restart() {
        let storage_dir = durable_storage_dir("restart-conflict");
        let first_service =
            SecretaryService::new_durable(storage_dir.clone()).expect("durable service");
        let root = test_repo_dir("durable-restart-conflict");
        let repository = first_service
            .register_repository(RegisterRepositoryRequest {
                display_name: "durable-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("repository registration should succeed");
        first_service
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "first goal".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("restart-key".to_string()),
            })
            .expect("job submission should succeed");
        drop(first_service);

        let second_service =
            SecretaryService::new_durable(storage_dir.clone()).expect("durable service reload");
        let err = second_service
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "different goal".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("restart-key".to_string()),
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::Conflict { .. }));

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(storage_dir);
    }

    #[test]
    fn durable_service_does_not_persist_blocked_idempotency_records() {
        let storage_dir = durable_storage_dir("blocked-idempotency");
        let first_service =
            SecretaryService::new_durable(storage_dir.clone()).expect("durable service");
        let root = test_repo_dir("blocked-idempotency");
        let repository = first_service
            .register_repository(RegisterRepositoryRequest {
                display_name: "durable-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Blocked,
                allowed_scope: None,
                requester: None,
            })
            .expect("repository registration should succeed");

        let first = first_service
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "blocked request".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("blocked-key".to_string()),
            })
            .expect("blocked submit should still return a receipt");
        assert_eq!(first.job.status, JobStatus::Blocked);
        assert!(first_service
            .lifecycle
            .runtime()
            .store()
            .get_submit_job_idempotency("blocked-key")
            .expect("idempotency lookup should succeed")
            .is_none());
        let first_job_id = first.job.id.clone();
        drop(first_service);

        let second_service =
            SecretaryService::new_durable(storage_dir.clone()).expect("durable service reload");
        assert!(second_service
            .lifecycle
            .runtime()
            .store()
            .get_submit_job_idempotency("blocked-key")
            .expect("idempotency lookup should succeed")
            .is_none());

        let second = second_service
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "blocked request".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("blocked-key".to_string()),
            })
            .expect("blocked submit should execute again after restart");
        assert_eq!(second.job.status, JobStatus::Blocked);
        assert_ne!(second.job.id, first_job_id);

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(storage_dir);
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
        assert!(metadata.capabilities.contains(&"repertoire.v1".to_string()));
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

    fn repertoire_tool_ids(entries: &[RepertoireEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.tool_id.as_str()).collect()
    }

    #[test]
    fn list_repertoire_projects_static_builtin_tools() {
        let svc = ready_service();
        let repertoire = svc
            .list_repertoire(ListRepertoireRequest)
            .expect("repertoire projection should succeed");

        assert_eq!(
            repertoire_tool_ids(&repertoire.entries),
            vec!["fs.read", "secretary.echo"]
        );
        let read = repertoire
            .entries
            .iter()
            .find(|entry| entry.tool_id == "fs.read")
            .expect("fs.read repertoire entry");
        assert_eq!(read.name, "Filesystem Read");
        assert_eq!(read.risk_tier, "R1");
        assert_eq!(read.provider_kind, "builtin");
        assert_eq!(read.provider_id, "atelia-secretary");
        assert_eq!(read.default_result_format, "toon");
        assert!(!read.cancellable);
        assert_eq!(
            read.supported_result_formats,
            vec!["toon".to_string(), "json".to_string()]
        );
        assert_eq!(read.timeout_ms, 0);
        let echo = repertoire
            .entries
            .iter()
            .find(|entry| entry.tool_id == "secretary.echo")
            .expect("secretary.echo repertoire entry");
        assert_eq!(echo.risk_tier, "R0");
        assert!(!echo.cancellable);
        assert_eq!(echo.timeout_ms, 0);
        assert!(repertoire
            .entries
            .iter()
            .all(|entry| { matches!(entry.tool_id.as_str(), "fs.read" | "secretary.echo") }));
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
    fn register_prefers_duplicate_root_conflict_over_exact_root_blocked_policy_decision() {
        let svc = ready_service();
        let root = test_repo_dir("duplicate-blocked-policy");

        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "repo-a".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("first register should succeed");

        svc.lifecycle
            .runtime()
            .store()
            .create_policy_decision(blocked_policy_decision(
                repository.id.clone(),
                "filesystem-read",
                ".",
            ))
            .expect("policy decision should persist");

        let err = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "repo-b".to_string(),
                root_path: root.join(".").to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::ReadOnly,
                allowed_scope: None,
                requester: None,
            })
            .unwrap_err();

        match err {
            ServiceError::Conflict { reason } => {
                assert_eq!(reason, "root_path is already registered");
            }
            other => panic!("expected duplicate-root conflict, got {other:?}"),
        }
        assert_eq!(svc.health().repository_count, 1);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn register_rejects_root_covered_by_blocked_policy_decision_for_alias_and_non_read_capabilities(
    ) {
        for requested_capability in ["filesystem-read", "repo.broad.mutation"] {
            let svc = ready_service();
            let root = test_repo_dir(&format!("blocked-policy-root-{requested_capability}"));
            let child_root = root.join("nested");
            fs::create_dir_all(&child_root).unwrap();
            fs::create_dir_all(child_root.join(".git")).unwrap();

            let parent_repository = svc
                .register_repository(RegisterRepositoryRequest {
                    display_name: "parent-repo".to_string(),
                    root_path: root.to_string_lossy().to_string(),
                    trust_state: RepositoryTrustState::Trusted,
                    allowed_scope: None,
                    requester: None,
                })
                .expect("parent register should succeed");

            svc.lifecycle
                .runtime()
                .store()
                .create_policy_decision(blocked_policy_decision(
                    parent_repository.id.clone(),
                    requested_capability,
                    ".",
                ))
                .expect("policy decision should persist");

            let err = svc
                .register_repository(RegisterRepositoryRequest {
                    display_name: format!("child-repo-{requested_capability}"),
                    root_path: child_root.to_string_lossy().to_string(),
                    trust_state: RepositoryTrustState::Trusted,
                    allowed_scope: None,
                    requester: None,
                })
                .unwrap_err();

            assert!(matches!(err, ServiceError::Conflict { .. }));
            assert_eq!(svc.health().repository_count, 1);

            let _ = fs::remove_dir_all(child_root);
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn register_rejects_root_under_blocked_ancestor_repository() {
        let svc = ready_service();
        let root = test_repo_dir("blocked-ancestor-root");
        let child_root = root.join("nested");
        fs::create_dir_all(&child_root).unwrap();
        fs::create_dir_all(child_root.join(".git")).unwrap();

        svc.register_repository(RegisterRepositoryRequest {
            display_name: "blocked-parent".to_string(),
            root_path: root.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::Blocked,
            allowed_scope: None,
            requester: None,
        })
        .expect("blocked parent register should succeed");

        let err = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "child-repo".to_string(),
                root_path: child_root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::Conflict { .. }));
        assert_eq!(svc.health().repository_count, 1);
        let _ = fs::remove_dir_all(child_root);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn register_rejects_root_over_blocked_child_repository() {
        let svc = ready_service();
        let root = test_repo_dir("blocked-child-root");
        let child_root = root.join("nested");
        fs::create_dir_all(&child_root).unwrap();
        fs::create_dir_all(child_root.join(".git")).unwrap();

        svc.register_repository(RegisterRepositoryRequest {
            display_name: "blocked-child".to_string(),
            root_path: child_root.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::Blocked,
            allowed_scope: None,
            requester: None,
        })
        .expect("blocked child register should succeed");

        let err = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "parent-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::Conflict { .. }));
        assert_eq!(svc.health().repository_count, 1);
        let _ = fs::remove_dir_all(child_root);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn register_rejects_root_over_root_scoped_blocked_policy_decision_child() {
        let svc = ready_service();
        let root = test_repo_dir("blocked-policy-child-root");
        let child_root = root.join("nested");
        fs::create_dir_all(&child_root).unwrap();
        fs::create_dir_all(child_root.join(".git")).unwrap();

        let child_repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "child-repo".to_string(),
                root_path: child_root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("child register should succeed");

        svc.lifecycle
            .runtime()
            .store()
            .create_policy_decision(blocked_policy_decision(
                child_repository.id.clone(),
                "filesystem-read",
                ".",
            ))
            .expect("policy decision should persist");

        let err = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "parent-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::Conflict { .. }));
        assert_eq!(svc.health().repository_count, 1);
        let _ = fs::remove_dir_all(child_root);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn register_accepts_root_covered_by_non_root_blocked_path_like_policy_decision() {
        let svc = ready_service();
        let root = test_repo_dir("non-root-blocked-policy-root");
        let child_root = root.join("nested");
        fs::create_dir_all(&child_root).unwrap();
        fs::create_dir_all(child_root.join(".git")).unwrap();

        let parent_repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "parent-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("parent register should succeed");

        svc.lifecycle
            .runtime()
            .store()
            .create_policy_decision(PolicyDecision {
                id: PolicyDecisionId::new(),
                schema_version: 1,
                created_at: LedgerTimestamp::now(),
                requester: Actor::System {
                    id: "service-test".to_string(),
                },
                repository_id: parent_repository.id.clone(),
                requested_capability: "repo.broad.mutation".to_string(),
                resource_scope: ResourceScope {
                    kind: "path".to_string(),
                    value: ".".to_string(),
                },
                tool_id: None,
                provider_id: None,
                declared_effect: "block repository mutation".to_string(),
                current_trust_state: RepositoryTrustState::Trusted,
                approval_available: false,
                policy_version: atelia_core::DEFAULT_POLICY_VERSION.to_string(),
                outcome: PolicyOutcome::Blocked,
                risk_tier: RiskTier::R4,
                reason_code: "destructive_repository_action_blocked".to_string(),
                user_reason: "path-scoped blocked action".to_string(),
                approval_request_ref: None,
                audit_ref: None,
                redactions: Vec::new(),
            })
            .expect("policy decision should persist");

        let child_repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "child-repo".to_string(),
                root_path: child_root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("child register should succeed");

        assert_eq!(svc.health().repository_count, 2);
        assert_eq!(
            child_repository.root_path,
            child_root.canonicalize().unwrap().to_string_lossy()
        );
        let _ = fs::remove_dir_all(child_root);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn blocking_policy_decision_skips_stale_repository_references() {
        let candidate_root = Path::new("/tmp/atelia-register-candidate");
        let stale_decision = PolicyDecision {
            id: PolicyDecisionId::new(),
            schema_version: 1,
            created_at: LedgerTimestamp::now(),
            requester: Actor::System {
                id: "service-test".to_string(),
            },
            repository_id: RepositoryId::new(),
            requested_capability: "filesystem.read".to_string(),
            resource_scope: ResourceScope {
                kind: "repository".to_string(),
                value: ".".to_string(),
            },
            tool_id: None,
            provider_id: None,
            declared_effect: "stale blocked repository reference".to_string(),
            current_trust_state: RepositoryTrustState::Trusted,
            approval_available: false,
            policy_version: atelia_core::DEFAULT_POLICY_VERSION.to_string(),
            outcome: PolicyOutcome::Blocked,
            risk_tier: RiskTier::R4,
            reason_code: "repository_blocked".to_string(),
            user_reason: "stale repository reference".to_string(),
            approval_request_ref: None,
            audit_ref: None,
            redactions: Vec::new(),
        };

        let matched = blocking_policy_decision_for_candidate_root(
            candidate_root,
            HashMap::new(),
            vec![stale_decision],
        );

        assert!(matched.is_none());
    }

    #[test]
    fn blocking_policy_decision_ignores_non_path_scopes() {
        let source_root = plain_test_dir("register-source-non-path-scope");
        let candidate_root = source_root.join("candidate");
        fs::create_dir_all(&candidate_root).unwrap();
        let repository_id = RepositoryId::new();
        let mut repository_roots = HashMap::new();
        repository_roots.insert(repository_id.clone(), source_root.clone());

        let non_path_decision = PolicyDecision {
            id: PolicyDecisionId::new(),
            schema_version: 1,
            created_at: LedgerTimestamp::now(),
            requester: Actor::System {
                id: "service-test".to_string(),
            },
            repository_id,
            requested_capability: "filesystem.read".to_string(),
            resource_scope: ResourceScope {
                kind: "artifact_ref".to_string(),
                value: ".".to_string(),
            },
            tool_id: None,
            provider_id: None,
            declared_effect: "non-path blocked reference".to_string(),
            current_trust_state: RepositoryTrustState::Trusted,
            approval_available: false,
            policy_version: atelia_core::DEFAULT_POLICY_VERSION.to_string(),
            outcome: PolicyOutcome::Blocked,
            risk_tier: RiskTier::R4,
            reason_code: "repository_blocked".to_string(),
            user_reason: "non-path blocked scope".to_string(),
            approval_request_ref: None,
            audit_ref: None,
            redactions: Vec::new(),
        };

        let matched = blocking_policy_decision_for_candidate_root(
            candidate_root.as_path(),
            repository_roots,
            vec![non_path_decision],
        );

        assert!(matched.is_none());
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn blocking_policy_decision_ignores_non_root_blocked_path_like_scope() {
        let source_root = plain_test_dir("register-source-non-root-path-like");
        let candidate_root = source_root.join("nested");
        fs::create_dir_all(&candidate_root).unwrap();
        let repository_id = RepositoryId::new();
        let mut repository_roots = HashMap::new();
        repository_roots.insert(repository_id.clone(), source_root.clone());

        let non_root_blocked_decision = PolicyDecision {
            id: PolicyDecisionId::new(),
            schema_version: 1,
            created_at: LedgerTimestamp::now(),
            requester: Actor::System {
                id: "service-test".to_string(),
            },
            repository_id,
            requested_capability: "repo.broad.mutation".to_string(),
            resource_scope: ResourceScope {
                kind: "path".to_string(),
                value: ".".to_string(),
            },
            tool_id: None,
            provider_id: None,
            declared_effect: "block repository mutation".to_string(),
            current_trust_state: RepositoryTrustState::Trusted,
            approval_available: false,
            policy_version: atelia_core::DEFAULT_POLICY_VERSION.to_string(),
            outcome: PolicyOutcome::Blocked,
            risk_tier: RiskTier::R4,
            reason_code: "destructive_repository_action_blocked".to_string(),
            user_reason: "path-scoped blocked action".to_string(),
            approval_request_ref: None,
            audit_ref: None,
            redactions: Vec::new(),
        };

        let matched = blocking_policy_decision_for_candidate_root(
            candidate_root.as_path(),
            repository_roots,
            vec![non_root_blocked_decision],
        );

        assert!(matched.is_none());
        let _ = fs::remove_dir_all(source_root);
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

    /// Build a blocked policy decision fixture for repository registration tests.
    fn blocked_policy_decision(
        repository_id: RepositoryId,
        requested_capability: &str,
        resource_scope_value: &str,
    ) -> PolicyDecision {
        PolicyDecision {
            id: PolicyDecisionId::new(),
            schema_version: 1,
            created_at: LedgerTimestamp::now(),
            requester: Actor::System {
                id: "service-test".to_string(),
            },
            repository_id,
            requested_capability: requested_capability.to_string(),
            resource_scope: ResourceScope {
                kind: "repository".to_string(),
                value: resource_scope_value.to_string(),
            },
            tool_id: None,
            provider_id: None,
            declared_effect: "block repository registration".to_string(),
            current_trust_state: RepositoryTrustState::Trusted,
            approval_available: false,
            policy_version: atelia_core::DEFAULT_POLICY_VERSION.to_string(),
            outcome: PolicyOutcome::Blocked,
            risk_tier: RiskTier::R4,
            reason_code: "repository_blocked".to_string(),
            user_reason: "repository root is blocked by policy".to_string(),
            approval_request_ref: None,
            audit_ref: None,
            redactions: Vec::new(),
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
    fn submit_job_idempotency_cache_lock_poisoning_returns_service_error() {
        let svc = Arc::new(ready_service());
        let poisoned = Arc::clone(&svc);

        thread::spawn(move || {
            let _guard = poisoned.idempotent_submissions.lock().unwrap();
            panic!("poison idempotency cache lock");
        })
        .join()
        .expect_err("poisoning thread should panic");

        let root = test_repo_dir("idempotency-cache-poison");
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
                requested_capabilities: Vec::new(),
                idempotency_key: Some("request-123".to_string()),
            })
            .unwrap_err();

        assert!(matches!(
            err,
            ServiceError::Internal { reason } if reason.contains("idempotency cache lock poisoned")
        ));
        let _ = fs::remove_dir_all(root);
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
    fn submit_job_dispatches_filesystem_read_tool() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-read-dispatch");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "read-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("README.md"), "alpha\nbeta\n").unwrap();

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: "read repository notes".to_string(),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "README.md".to_string(),
                }),
                requested_capabilities: vec!["filesystem.read".to_string()],
                idempotency_key: Some("read-job-key".to_string()),
            })
            .expect("real read dispatch should succeed");

        assert_eq!(
            receipt
                .tool_invocation
                .as_ref()
                .expect("tool invocation should exist")
                .tool_id,
            "fs.read"
        );
        assert_eq!(
            receipt.policy_decision.requested_capability,
            "filesystem.read"
        );

        let tool_result = receipt.tool_result.expect("tool result should exist");
        assert_eq!(
            tool_result.schema_ref.as_deref(),
            Some("tool_result.fs.read.v1")
        );
        assert!(tool_result.fields.iter().any(|field| {
            field.key == "content"
                && matches!(&field.value, atelia_core::StructuredValue::String(value) if value == "alpha\nbeta")
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_read_outside_allowed_scope() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-read-allowed-scope");
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("README.md"), "root\n").unwrap();
        fs::write(root.join("docs/guide.md"), "docs\n").unwrap();
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "read-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: Some(PathScope {
                    root_path: root.to_string_lossy().to_string(),
                    allowed_paths: vec!["docs".to_string()],
                }),
                requester: None,
            })
            .expect("register should succeed");

        let allowed = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "read scoped notes".to_string(),
                resource_scope: Some(ResourceScope {
                    kind: "explicit_paths".to_string(),
                    value: "docs/guide.md".to_string(),
                }),
                requested_capabilities: vec!["filesystem.read".to_string()],
                idempotency_key: None,
            })
            .expect("read inside allowed scope should succeed");
        assert_eq!(
            allowed
                .tool_invocation
                .as_ref()
                .expect("tool invocation should exist")
                .tool_id,
            "fs.read"
        );

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: "read root notes".to_string(),
                resource_scope: Some(ResourceScope {
                    kind: "explicit_paths".to_string(),
                    value: "README.md".to_string(),
                }),
                requested_capabilities: vec!["filesystem.read".to_string()],
                idempotency_key: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_read_without_path_scope_before_side_effects() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-read-rejected");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "read-repo".to_string(),
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
                goal: "read repository notes".to_string(),
                resource_scope: None,
                requested_capabilities: vec!["filesystem.read".to_string()],
                idempotency_key: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        assert!(svc
            .list_jobs(None, None, None, None, None)
            .expect("job query should succeed")
            .jobs
            .is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_read_repository_root_before_side_effects() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-read-root-rejected");
        fs::create_dir_all(root.join("docs")).unwrap();
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "read-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        for value in [".", "./", "docs/.."] {
            let err = svc
                .submit_job(SubmitJobRequest {
                    requester: actor(),
                    repository_id: repository.id.clone(),
                    kind: JobKind::Read,
                    goal: "read repository root".to_string(),
                    resource_scope: Some(ResourceScope {
                        kind: "repository".to_string(),
                        value: value.to_string(),
                    }),
                    requested_capabilities: vec!["filesystem.read".to_string()],
                    idempotency_key: None,
                })
                .unwrap_err();

            assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        }
        assert!(svc
            .list_jobs(None, None, None, None, None)
            .expect("job query should succeed")
            .jobs
            .is_empty());
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

        let first_request = SubmitJobRequest {
            requester: actor(),
            repository_id: repository.id.clone(),
            kind: JobKind::Read,
            goal: "summarize".to_string(),
            resource_scope: None,
            requested_capabilities: Vec::new(),
            idempotency_key: Some("request-123".to_string()),
        };
        let first_normalized_goal = first_request.goal.trim().to_string();
        let first_signature = submit_job_request_signature(
            &first_request,
            &first_normalized_goal,
            &normalize_requested_capabilities(&first_request.requested_capabilities)
                .expect("first capability normalization should succeed"),
        );
        let first = svc
            .submit_job(first_request)
            .expect("submit should succeed");

        let second_request = SubmitJobRequest {
            requester: actor(),
            repository_id: repository.id,
            kind: JobKind::Read,
            goal: "summarize".to_string(),
            resource_scope: None,
            requested_capabilities: vec!["capability.discovery".to_string()],
            idempotency_key: Some("request-123".to_string()),
        };
        let second_normalized_goal = second_request.goal.trim().to_string();
        let second_signature = submit_job_request_signature(
            &second_request,
            &second_normalized_goal,
            &normalize_requested_capabilities(&second_request.requested_capabilities)
                .expect("second capability normalization should succeed"),
        );
        let second = svc
            .submit_job(second_request)
            .expect("normalized capability should replay the same job");

        assert_eq!(first.job.id, second.job.id);
        assert_eq!(first_signature, second_signature);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_trims_goal_before_runtime_and_replay() {
        let svc = ready_service();
        let root = test_repo_dir("trimmed-goal-idempotency");
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
                goal: "  summarize  ".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("first submit should succeed");

        assert_eq!(first.job.goal, "summarize");

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
            .expect("trimmed goal should replay the same job");

        assert_eq!(second.job.id, first.job.id);
        assert_eq!(second.job.goal, "summarize");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_does_not_cache_failed_idempotent_requests() {
        let svc = ready_service();
        let root = test_repo_dir("failed-idempotency");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("binary.bin"), vec![0x61, 0x00, 0x62]).expect("binary test file");

        let first = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: "inspect".to_string(),
                resource_scope: Some(ResourceScope {
                    kind: "repository".to_string(),
                    value: "binary.bin".to_string(),
                }),
                requested_capabilities: vec!["filesystem.read".to_string()],
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("first submit should return a failed receipt");

        assert_eq!(first.job.status, JobStatus::Failed);

        let second = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: "inspect".to_string(),
                resource_scope: Some(ResourceScope {
                    kind: "repository".to_string(),
                    value: "binary.bin".to_string(),
                }),
                requested_capabilities: vec!["filesystem.read".to_string()],
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("failed job should not be replayed from the in-memory cache");

        assert_eq!(second.job.status, JobStatus::Failed);
        assert_ne!(second.job.id, first.job.id);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_request_signature_distinguishes_delimiter_like_goal_and_scope_values() {
        let svc = ready_service();
        let root = test_repo_dir("delimiter-like-signature");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let normalized_capabilities =
            normalize_requested_capabilities(&["capability.discovery".to_string()])
                .expect("capability discovery should normalize");

        let request_one = SubmitJobRequest {
            requester: actor(),
            repository_id: repository.id.clone(),
            kind: JobKind::Read,
            goal: "summarize;resource_scope=repository:.".to_string(),
            resource_scope: Some(ResourceScope {
                kind: "repository".to_string(),
                value: "branch=main;capabilities=capability.discovery".to_string(),
            }),
            requested_capabilities: vec!["capability.discovery".to_string()],
            idempotency_key: None,
        };
        let request_two = SubmitJobRequest {
            requester: actor(),
            repository_id: repository.id,
            kind: JobKind::Read,
            goal: "summarize".to_string(),
            resource_scope: Some(ResourceScope {
                kind: "repository".to_string(),
                value: "branch=main".to_string(),
            }),
            requested_capabilities: vec!["capability.discovery".to_string()],
            idempotency_key: None,
        };

        let request_one_normalized_goal = request_one.goal.trim().to_string();
        let request_two_normalized_goal = request_two.goal.trim().to_string();
        let signature_one = submit_job_request_signature(
            &request_one,
            &request_one_normalized_goal,
            &normalized_capabilities,
        );
        let signature_two = submit_job_request_signature(
            &request_two,
            &request_two_normalized_goal,
            &normalized_capabilities,
        );

        assert_ne!(signature_one, signature_two);
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
        assert!(svc.idempotent_submission_locks.lock().unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_idempotency_cache_is_bounded() {
        let svc = ready_service();
        let root = test_repo_dir("bounded-idempotency-cache");
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
                idempotency_key: Some("request-0".to_string()),
            })
            .expect("first submit should succeed");

        for index in 1..=IDEMPOTENT_SUBMISSION_CACHE_LIMIT {
            let receipt = svc
                .submit_job(SubmitJobRequest {
                    requester: actor(),
                    repository_id: repository.id.clone(),
                    kind: JobKind::Read,
                    goal: "summarize".to_string(),
                    resource_scope: None,
                    requested_capabilities: Vec::new(),
                    idempotency_key: Some(format!("request-{index}")),
                })
                .expect("unique submit should succeed");
            assert_eq!(receipt.job.goal, "summarize");
        }

        assert_eq!(
            svc.idempotent_submissions.lock().unwrap().len(),
            IDEMPOTENT_SUBMISSION_CACHE_LIMIT
        );

        let replayed = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: "summarize".to_string(),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                idempotency_key: Some("request-0".to_string()),
            })
            .expect("replayed submit should still succeed after cache eviction");
        assert_eq!(replayed.job.id, first.job.id);
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
        assert!(svc.idempotent_submission_locks.lock().unwrap().is_empty());

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
    fn submit_job_rejects_blank_idempotency_key() {
        let svc = ready_service();
        let root = test_repo_dir("blank-idempotency-key");
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
                requested_capabilities: Vec::new(),
                idempotency_key: Some("   ".to_string()),
            })
            .unwrap_err();

        assert!(matches!(
            err,
            ServiceError::InvalidArgument { reason }
                if reason == "idempotency_key must not be blank"
        ));
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
