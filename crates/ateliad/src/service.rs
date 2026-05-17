//! Daemon service skeleton for Atelia Secretary (Slice 4).
//!
//! Owns daemon health/status metadata, an in-memory job lifecycle runtime, and
//! exposes a synchronous service API for health checks, repository
//! registration/listing, and the first supported job lifecycle calls.

use atelia_core::policy::{REASON_REPOSITORY_BLOCKED, SCOPE_KIND_REPOSITORY, SCOPE_VALUE_ROOT};
use atelia_core::{
    canonicalize_job_requested_capability, canonicalize_within_scope,
    render_tool_result_with_policy, Actor, ApplyBlocklistRequest, ApplyBlocklistResponse,
    AuditRecordId, BlockKey, BlocklistEntry, CancelJobReceipt, DefaultPolicyEngine,
    DisableExtensionRequest, DisableExtensionResponse, EchoTool, EnableExtensionRequest,
    EnableExtensionResponse, EventCursor, EventPage, EventQuery, ExtensionBlocklistMatch,
    ExtensionBoundary, ExtensionInstallStatus, ExtensionManifest, ExtensionPublication,
    ExtensionRegistry, ExtensionRegistryAuditKind, ExtensionRegistryAuditProvenance,
    ExtensionRegistryAuditRecord, ExtensionRegistryAuditRecordRef, ExtensionRegistryService,
    ExtensionRegistrySnapshot, ExtensionServices, ExtensionSourceSnapshot, ExtensionStatusRequest,
    ExtensionStatusResponse, FsDeleteTool, FsDiffTool, FsListTool, FsReadTool, FsSearchTool,
    FsStatTool, InMemoryStore, InMemoryToolOutputSettingsService, InstallExtensionRequest,
    InstallExtensionResponse, JobEvent, JobId, JobKind, JobLifecycleService, JobPage, JobQuery,
    JobRecord, JobStatus, LedgerTimestamp, ListBlocklistRequest, ListBlocklistResponse,
    ListExtensionsRequest, ListExtensionsResponse, ManifestValidationPolicy, OutputFormat,
    PathScope, PolicyDecision, PolicyEngine, PolicyInput, PolicyOutcome, RegistryError,
    RemoveExtensionRequest, RemoveExtensionResponse, RenderedToolOutput, RepositoryId,
    RepositoryRecord, RepositoryTrustState, ResourceScope, RollbackExtensionRequest,
    RollbackExtensionResponse, RollbackSnapshot, RuntimeError, RuntimeJobReceipt,
    RuntimeJobRequest, SecretaryStore, StoreError, SubmitJobIdempotencyRecord, ToolInvocationId,
    ToolOutputDefaults, ToolOutputOverrides, ToolOutputSettingsChange, ToolOutputSettingsError,
    ToolOutputSettingsScope, ToolResultId, TruncationMetadata, UpdateExtensionPublicationRequest,
    UpdateExtensionPublicationResponse, UpdateExtensionRegistrySubmissionRequest,
    UpdateExtensionRegistrySubmissionResponse, UpdateExtensionRequest, UpdateExtensionResponse,
    ValidateExtensionManifestRequest, ValidateExtensionManifestResponse, WatchJobEvent,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

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
    "extensions.authoring.v1",
    "package_inspect.v1",
    "package_trust_index.v1",
    "tool_output_settings.v1",
    "tool_output_render.v1",
    "project_status.v1",
];
const MAX_HISTORY_PAGE: usize = 1000;
const SECRETARY_ECHO_TOOL_ID: &str = "secretary.echo";
const SECRETARY_ECHO_TOOL_NAME: &str = "Secretary Echo";
const SECRETARY_ECHO_TOOL_DESCRIPTION: &str =
    "Echo input for daemon smoke tests and context probes.";
const SECRETARY_FS_DELETE_TOOL_ID: &str = "fs.delete";
const SECRETARY_FS_DELETE_TOOL_NAME: &str = "Filesystem Delete";
const SECRETARY_FS_DELETE_TOOL_DESCRIPTION: &str =
    "Delete one file from an allowed repository path.";
const SECRETARY_FS_LIST_TOOL_ID: &str = "fs.list";
const SECRETARY_FS_LIST_TOOL_NAME: &str = "Filesystem List";
const SECRETARY_FS_LIST_TOOL_DESCRIPTION: &str = "List directory entries within an allowed scope.";
const SECRETARY_FS_SEARCH_TOOL_ID: &str = "fs.search";
const SECRETARY_FS_SEARCH_TOOL_NAME: &str = "Filesystem Search";
const SECRETARY_FS_SEARCH_TOOL_DESCRIPTION: &str =
    "Search file contents for a literal pattern within an allowed repository scope.";
const SECRETARY_FS_READ_TOOL_ID: &str = "fs.read";
const SECRETARY_FS_READ_TOOL_NAME: &str = "Filesystem Read";
const SECRETARY_FS_READ_TOOL_DESCRIPTION: &str = "Read a file from an allowed repository scope.";
const SECRETARY_FS_DIFF_TOOL_ID: &str = "fs.diff";
const SECRETARY_FS_DIFF_TOOL_NAME: &str = "Filesystem Diff";
const SECRETARY_FS_DIFF_TOOL_DESCRIPTION: &str =
    "Compare a bounded diff between two files in the repository.";
const SECRETARY_FS_STAT_TOOL_ID: &str = "fs.stat";
const SECRETARY_FS_STAT_TOOL_NAME: &str = "Filesystem Stat";
const SECRETARY_FS_STAT_TOOL_DESCRIPTION: &str = "Read metadata for a file or directory.";
const SECRETARY_TOOL_PROVIDER_KIND: &str = "builtin";
const SECRETARY_TOOL_PROVIDER_ID: &str = "atelia-secretary";
const SECRETARY_TOON_FORMAT: &str = "toon";
const SECRETARY_JSON_FORMAT: &str = "json";
const SECRETARY_FS_READ_CAPABILITY: &str = "filesystem.read";
const SECRETARY_FS_LIST_CAPABILITY: &str = "filesystem.list";
const SECRETARY_FS_STAT_CAPABILITY: &str = "filesystem.stat";
const SECRETARY_FS_DELETE_CAPABILITY: &str = "filesystem.delete";
const SECRETARY_FS_SEARCH_CAPABILITY: &str = "filesystem.search";
const SECRETARY_FS_DIFF_CAPABILITY: &str = "filesystem.diff";
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListPackageTrustIndexRequest {
    #[serde(default = "ListPackageTrustIndexRequest::default_include_blocked")]
    pub include_blocked: bool,
    #[serde(default)]
    pub discovery_only: bool,
}

impl ListPackageTrustIndexRequest {
    fn default_include_blocked() -> bool {
        true
    }
}

impl Default for ListPackageTrustIndexRequest {
    fn default() -> Self {
        Self {
            include_blocked: true,
            discovery_only: false,
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPackageTrustIndexResponse {
    pub packages: Vec<PackageTrustIndexEntry>,
}

/// Aggregated package detail used by client inspect surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageInspectResponse {
    /// Current installed-package status and active record.
    pub extension: ExtensionStatusResponse,
    /// Manifest snapshot for the active installed version.
    pub manifest: ExtensionManifest,
    /// Active blocklist match, when the package is blocked.
    pub block: Option<ExtensionBlocklistMatch>,
    /// Approved permissions recorded for the installed package.
    pub permissions: Vec<String>,
    /// Service declarations from the active manifest.
    pub services: ExtensionServices,
    /// Whether a rollback target is available.
    pub rollback_available: bool,
    /// Snapshot of the rollback target, when available.
    pub rollback_snapshot: Option<RollbackSnapshot>,
    /// Source and provenance snapshot recorded at install time.
    pub source: ExtensionSourceSnapshot,
    /// Publication and registry trust metadata, when available.
    pub trust: Option<ExtensionPublication>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageTrustIndexEntry {
    pub package_id: String,
    pub version: Option<String>,
    pub status: Option<ExtensionInstallStatus>,
    pub boundary: Option<ExtensionBoundary>,
    pub manifest_digest: Option<String>,
    pub artifact_digest: Option<String>,
    pub source: Option<ExtensionSourceSnapshot>,
    pub block: Option<ExtensionBlocklistMatch>,
}

impl From<ExtensionStatusResponse> for PackageTrustIndexEntry {
    fn from(status: ExtensionStatusResponse) -> Self {
        let (version, install_status, boundary, manifest_digest, artifact_digest, source) = status
            .record
            .map(|record| {
                (
                    Some(record.version),
                    Some(record.status),
                    Some(record.boundary),
                    Some(record.manifest_digest),
                    Some(record.artifact_digest),
                    Some(record.source),
                )
            })
            .unwrap_or((None, None, None, None, None, None));

        Self {
            package_id: status.extension_id,
            version,
            status: install_status,
            boundary,
            manifest_digest,
            artifact_digest,
            source,
            block: status.block,
        }
    }
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
    pub goal: Option<String>,
    pub resource_scope: Option<ResourceScope>,
    pub requested_capabilities: Vec<String>,
    pub tool_args: Option<SubmitJobToolArgs>,
    /// Optional caller-provided key used to deduplicate successful retries.
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)]
pub struct SubmitJobToolArgs {
    pub pattern: Option<String>,
    pub max: Option<u64>,
    pub comparison_path: Option<String>,
    pub max_bytes: Option<u64>,
    pub max_chars: Option<u64>,
}

#[derive(Debug, Clone)]
struct SubmitJobToolArgsSearch {
    pattern: String,
    max_results: Option<usize>,
}

#[derive(Debug, Clone)]
struct SubmitJobToolArgsDiff {
    comparison_path: String,
    max_bytes: Option<usize>,
    max_chars: Option<usize>,
}

#[derive(Debug, Clone)]
enum SubmitJobToolArgsSpec {
    Search(SubmitJobToolArgsSearch),
    Diff(SubmitJobToolArgsDiff),
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmitJobToolKind {
    Echo,
    FsRead,
    FsList,
    FsStat,
    FsDelete,
    FsSearch,
    FsDiff,
}

impl SubmitJobToolKind {
    fn tool_id(self) -> &'static str {
        match self {
            Self::Echo => SECRETARY_ECHO_TOOL_ID,
            Self::FsRead => SECRETARY_FS_READ_TOOL_ID,
            Self::FsList => SECRETARY_FS_LIST_TOOL_ID,
            Self::FsStat => SECRETARY_FS_STAT_TOOL_ID,
            Self::FsDelete => SECRETARY_FS_DELETE_TOOL_ID,
            Self::FsSearch => SECRETARY_FS_SEARCH_TOOL_ID,
            Self::FsDiff => SECRETARY_FS_DIFF_TOOL_ID,
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

#[derive(Debug, Clone, Default)]
pub struct ListExtensionRegistryAuditRecordsRequest {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ListExtensionRegistryAuditRecordsPage {
    pub records: Vec<ExtensionRegistryAuditRecord>,
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
    pub receiver: mpsc::Receiver<WatchJobEvent>,
    pub replay_max_sequence: Option<u64>,
    pub resolved_cursor_sequence: Option<u64>,
}

struct ExtensionRegistryAuditContext {
    package_id: Option<String>,
    record_version: Option<String>,
    actor: Actor,
    policy_decision_id: Option<atelia_core::PolicyDecisionId>,
    blocklist_entry: Option<BlocklistEntry>,
}

const PACKAGE_REGISTRY_REQUEST_SOURCE_MAX_CHARS: usize = 128;
const PACKAGE_REGISTRY_REASON_MAX_CHARS: usize = 1024;

fn default_package_registry_actor() -> Actor {
    Actor::System {
        id: "atelia-secretary".to_string(),
    }
}

fn package_registry_actor(requester: Option<Actor>) -> Actor {
    requester.unwrap_or_else(default_package_registry_actor)
}

fn truncate_audit_text(value: String, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value
    } else {
        value.chars().take(max_chars).collect()
    }
}

fn package_registry_request_source(request_source: Option<String>) -> String {
    let value = request_source
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "service".to_string());
    truncate_audit_text(value, PACKAGE_REGISTRY_REQUEST_SOURCE_MAX_CHARS)
}

fn package_registry_reason(reason: Option<String>, default_reason: &str) -> String {
    let value = reason
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_reason.to_string());
    truncate_audit_text(value, PACKAGE_REGISTRY_REASON_MAX_CHARS)
}

fn active_extension_record_ref(
    snapshot: &ExtensionRegistrySnapshot,
    package_id: &str,
) -> Option<ExtensionRegistryAuditRecordRef> {
    let active_version = snapshot.active_versions.get(package_id)?;
    snapshot
        .records
        .get(package_id)?
        .get(active_version)
        .map(ExtensionRegistryAuditRecordRef::from)
}

fn extension_record_ref(
    snapshot: &ExtensionRegistrySnapshot,
    package_id: &str,
    version: &str,
) -> Option<ExtensionRegistryAuditRecordRef> {
    snapshot
        .records
        .get(package_id)?
        .get(version)
        .map(ExtensionRegistryAuditRecordRef::from)
}

fn active_extension_provenance(
    snapshot: &ExtensionRegistrySnapshot,
    package_id: &str,
) -> Option<ExtensionRegistryAuditProvenance> {
    let active_version = snapshot.active_versions.get(package_id)?;
    snapshot
        .records
        .get(package_id)?
        .get(active_version)
        .map(|record| ExtensionRegistryAuditProvenance::from(&record.source))
}

fn extension_provenance(
    snapshot: &ExtensionRegistrySnapshot,
    package_id: &str,
    version: &str,
) -> Option<ExtensionRegistryAuditProvenance> {
    snapshot
        .records
        .get(package_id)?
        .get(version)
        .map(|record| ExtensionRegistryAuditProvenance::from(&record.source))
}

fn extension_registry_audit_record(
    kind: ExtensionRegistryAuditKind,
    request_source: String,
    reason: String,
    context: ExtensionRegistryAuditContext,
    before: &ExtensionRegistrySnapshot,
    after: &ExtensionRegistrySnapshot,
) -> ExtensionRegistryAuditRecord {
    let package_id = context.package_id.clone();
    let record_version = context.record_version.as_deref();
    let previous_record = package_id.as_deref().and_then(|package_id| {
        if let Some(version) = record_version {
            extension_record_ref(before, package_id, version)
        } else {
            active_extension_record_ref(before, package_id)
        }
    });
    let new_record = package_id.as_deref().and_then(|package_id| {
        if let Some(version) = record_version {
            extension_record_ref(after, package_id, version)
        } else {
            active_extension_record_ref(after, package_id)
        }
    });
    let provenance = package_id.as_deref().and_then(|package_id| {
        if let Some(version) = record_version {
            extension_provenance(after, package_id, version)
                .or_else(|| extension_provenance(before, package_id, version))
        } else {
            active_extension_provenance(after, package_id)
                .or_else(|| active_extension_provenance(before, package_id))
        }
    });

    ExtensionRegistryAuditRecord {
        id: AuditRecordId::new(),
        schema_version: atelia_core::EXTENSION_REGISTRY_AUDIT_SCHEMA_VERSION,
        created_at: LedgerTimestamp::now(),
        kind,
        actor: context.actor,
        request_source,
        reason,
        policy_decision_id: context.policy_decision_id,
        package_id,
        previous_record,
        new_record,
        provenance,
        blocklist_entry: context.blocklist_entry,
    }
}

impl SecretaryService {
    /// Create a new service backed by an in-memory store and default policy.
    pub fn new() -> Self {
        Self::from_store(InMemoryStore::new())
            .expect("in-memory secretary service should initialize")
    }

    pub fn new_durable(storage_dir: impl Into<PathBuf>) -> ServiceResult<Self> {
        let store = InMemoryStore::with_durable_storage_dir(storage_dir)?;
        Self::from_store(store)
    }

    fn from_store(store: InMemoryStore) -> ServiceResult<Self> {
        let extension_registry = ExtensionRegistryService::with_registry(
            ExtensionRegistry::from_snapshot(
                store.extension_registry_snapshot()?,
                ManifestValidationPolicy::default(),
            )
            .map_err(|reason| {
                ServiceError::Store(atelia_core::StoreError::InvalidRecord {
                    collection: "extension_registry",
                    reason,
                })
            })?,
        );

        Ok(Self {
            lifecycle: JobLifecycleService::new(atelia_core::SecretaryRuntime::new(
                store,
                DefaultPolicyEngine::new(),
            )),
            started_at: LedgerTimestamp::now(),
            daemon_status: DaemonStatus::Starting,
            extension_registry: Mutex::new(extension_registry),
            tool_output_settings: Mutex::new(InMemoryToolOutputSettingsService::new(
                LedgerTimestamp::now(),
            )),
            idempotent_submissions: VecDeque::new().into(),
            idempotent_submission_locks: HashMap::new().into(),
            cancellation_requesters: HashMap::new().into(),
        })
    }

    fn persist_extension_registry_snapshot(
        &self,
        registry: &ExtensionRegistryService,
    ) -> ServiceResult<()> {
        self.lifecycle
            .runtime()
            .store()
            .set_extension_registry_snapshot(registry.snapshot())
            .map_err(ServiceError::from)?;
        Ok(())
    }

    fn mutate_extension_registry_with_audit<R>(
        &self,
        kind: ExtensionRegistryAuditKind,
        request_source: impl Into<String>,
        reason: impl Into<String>,
        mutator: impl FnOnce(&mut ExtensionRegistryService) -> Result<R, RegistryError>,
        audit_context: impl FnOnce(&R) -> ExtensionRegistryAuditContext,
        attach_audit_record_id: impl FnOnce(&mut R, AuditRecordId),
    ) -> ServiceResult<R> {
        let mut registry = self.lock_extension_registry()?;
        let before = registry.snapshot();
        let validation_policy = registry.validation_policy();
        let mut draft = ExtensionRegistryService::with_registry(
            ExtensionRegistry::from_snapshot(before.clone(), validation_policy).map_err(
                |reason| {
                    ServiceError::Store(atelia_core::StoreError::InvalidRecord {
                        collection: "extension_registry",
                        reason,
                    })
                },
            )?,
        );
        let mut response = mutator(&mut draft).map_err(ServiceError::from)?;
        let context = audit_context(&response);
        let after = draft.snapshot();
        let audit_record = extension_registry_audit_record(
            kind,
            request_source.into(),
            reason.into(),
            context,
            &before,
            &after,
        );
        let audit_record_id = audit_record.id.clone();
        draft
            .append_audit_record(audit_record)
            .map_err(ServiceError::from)?;
        self.persist_extension_registry_snapshot(&draft)?;
        attach_audit_record_id(&mut response, audit_record_id);
        *registry = draft;
        Ok(response)
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
        let start = list_history_page_start(offset, cursor, "tool output settings history")?;
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
    /// and an approved filesystem tool when requested.
    #[allow(dead_code)]
    pub fn submit_job(&self, request: SubmitJobRequest) -> ServiceResult<RuntimeJobReceipt> {
        let requested_capabilities =
            normalize_requested_capabilities(&request.requested_capabilities)?;
        let tool_kind = resolve_submit_job_tool_kind(&request, &requested_capabilities)?;
        let repository = self.get_repository(&request.repository_id)?;
        let normalized_goal = normalize_submit_job_goal(request.goal.clone());
        let resolved_tool_args =
            resolve_submit_job_tool_args(&tool_kind, request.tool_args.clone())?;

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
        let request_signature = submit_job_request_signature(
            &request,
            normalized_goal.as_deref(),
            &requested_capabilities,
        );
        let tool_output_defaults = self.get_tool_output_defaults(
            ToolOutputSettingsScope::repository(repository.id.clone())
                .for_tool(tool_kind.tool_id()),
        )?;

        let resource_scope = request.resource_scope.unwrap_or_else(|| ResourceScope {
            kind: "repository".to_string(),
            value: ".".to_string(),
        });
        if matches!(
            tool_kind,
            SubmitJobToolKind::FsRead
                | SubmitJobToolKind::FsList
                | SubmitJobToolKind::FsStat
                | SubmitJobToolKind::FsDelete
                | SubmitJobToolKind::FsSearch
                | SubmitJobToolKind::FsDiff
        ) {
            validate_filesystem_path_scope(
                &repository,
                &resource_scope,
                matches!(
                    tool_kind,
                    SubmitJobToolKind::FsRead | SubmitJobToolKind::FsDelete
                ),
            )?;
        }
        if let SubmitJobToolArgsSpec::Diff(diff) = &resolved_tool_args {
            validate_secondary_filesystem_path_scope(
                &repository,
                &resource_scope,
                &diff.comparison_path,
            )?;
        }

        let runtime_request = RuntimeJobRequest::new(
            request.requester,
            request.repository_id,
            request.kind,
            normalized_goal.clone(),
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
                    SubmitJobToolKind::FsList => {
                        let tool = FsListTool::new(&repository.root_path);
                        self.lifecycle.runtime().run_tool_job_with_finalizer(
                            runtime_request.clone(),
                            &tool,
                            Some(make_submit_job_finalizer(
                                idempotency_key.to_string(),
                                request_signature.clone(),
                            )),
                        )?
                    }
                    SubmitJobToolKind::FsStat => {
                        let tool = FsStatTool::new(&repository.root_path);
                        self.lifecycle.runtime().run_tool_job_with_finalizer(
                            runtime_request.clone(),
                            &tool,
                            Some(make_submit_job_finalizer(
                                idempotency_key.to_string(),
                                request_signature.clone(),
                            )),
                        )?
                    }
                    SubmitJobToolKind::FsDelete => {
                        let tool = FsDeleteTool::new(&repository.root_path);
                        self.lifecycle.runtime().run_tool_job_with_finalizer(
                            runtime_request.clone(),
                            &tool,
                            Some(make_submit_job_finalizer(
                                idempotency_key.to_string(),
                                request_signature.clone(),
                            )),
                        )?
                    }
                    SubmitJobToolKind::FsSearch => {
                        let search = match &resolved_tool_args {
                            SubmitJobToolArgsSpec::Search(args) => args,
                            _ => {
                                return Err(ServiceError::InvalidArgument {
                                    reason: "internal tool_args resolution lost for fs.search"
                                        .to_string(),
                                });
                            }
                        };
                        let allowed_roots = allowed_search_roots_for_repository(&repository)?;
                        let mut tool = FsSearchTool::new(&repository.root_path, &search.pattern)
                            .with_allowed_roots(&allowed_roots);
                        if let Some(max) = search.max_results {
                            tool = tool.with_max_results(max);
                        }
                        self.lifecycle.runtime().run_tool_job_with_finalizer(
                            runtime_request.clone(),
                            &tool,
                            Some(make_submit_job_finalizer(
                                idempotency_key.to_string(),
                                request_signature.clone(),
                            )),
                        )?
                    }
                    SubmitJobToolKind::FsDiff => {
                        let diff = match &resolved_tool_args {
                            SubmitJobToolArgsSpec::Diff(args) => args,
                            _ => {
                                return Err(ServiceError::InvalidArgument {
                                    reason: "internal tool_args resolution lost for fs.diff"
                                        .to_string(),
                                });
                            }
                        };
                        let mut tool =
                            FsDiffTool::new(&repository.root_path, &diff.comparison_path);
                        if let Some(max_bytes) = diff.max_bytes {
                            tool = tool.with_max_bytes(max_bytes);
                        }
                        if let Some(max_chars) = diff.max_chars {
                            tool = tool.with_max_chars(max_chars);
                        }
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
                SubmitJobToolKind::FsList => {
                    let tool = FsListTool::new(&repository.root_path);
                    self.lifecycle.runtime().run_tool_job_with_finalizer(
                        runtime_request,
                        &tool,
                        None::<
                            fn(&RuntimeJobReceipt) -> Option<(String, SubmitJobIdempotencyRecord)>,
                        >,
                    )?
                }
                SubmitJobToolKind::FsStat => {
                    let tool = FsStatTool::new(&repository.root_path);
                    self.lifecycle.runtime().run_tool_job_with_finalizer(
                        runtime_request,
                        &tool,
                        None::<
                            fn(&RuntimeJobReceipt) -> Option<(String, SubmitJobIdempotencyRecord)>,
                        >,
                    )?
                }
                SubmitJobToolKind::FsDelete => {
                    let tool = FsDeleteTool::new(&repository.root_path);
                    self.lifecycle.runtime().run_tool_job_with_finalizer(
                        runtime_request,
                        &tool,
                        None::<
                            fn(&RuntimeJobReceipt) -> Option<(String, SubmitJobIdempotencyRecord)>,
                        >,
                    )?
                }
                SubmitJobToolKind::FsSearch => {
                    let search = match resolved_tool_args {
                        SubmitJobToolArgsSpec::Search(ref args) => args,
                        _ => {
                            return Err(ServiceError::InvalidArgument {
                                reason: "internal tool_args resolution lost for fs.search"
                                    .to_string(),
                            })
                        }
                    };
                    let allowed_roots = allowed_search_roots_for_repository(&repository)?;
                    let mut tool = FsSearchTool::new(&repository.root_path, &search.pattern)
                        .with_allowed_roots(&allowed_roots);
                    if let Some(max) = search.max_results {
                        tool = tool.with_max_results(max);
                    }
                    self.lifecycle.runtime().run_tool_job_with_finalizer(
                        runtime_request,
                        &tool,
                        None::<
                            fn(&RuntimeJobReceipt) -> Option<(String, SubmitJobIdempotencyRecord)>,
                        >,
                    )?
                }
                SubmitJobToolKind::FsDiff => {
                    let diff = match resolved_tool_args {
                        SubmitJobToolArgsSpec::Diff(ref args) => args,
                        _ => {
                            return Err(ServiceError::InvalidArgument {
                                reason: "internal tool_args resolution lost for fs.diff"
                                    .to_string(),
                            })
                        }
                    };
                    let mut tool = FsDiffTool::new(&repository.root_path, &diff.comparison_path);
                    if let Some(max_bytes) = diff.max_bytes {
                        tool = tool.with_max_bytes(max_bytes);
                    }
                    if let Some(max_chars) = diff.max_chars {
                        tool = tool.with_max_chars(max_chars);
                    }
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

    /// Atomically snapshot retained events and subscribe to future events.
    #[allow(dead_code)]
    pub fn watch_events_live(&self, query: EventQuery) -> ServiceResult<LiveEventSubscription> {
        if query.page_size == Some(0) {
            return Err(ServiceError::InvalidArgument {
                reason: "page_size must be greater than 0".to_string(),
            });
        }
        let (events, receiver, resolved_cursor_sequence) = self
            .lifecycle
            .runtime()
            .store()
            .watch_job_events_live(query)?;

        let replay_max_sequence = events.last().map(|event| event.sequence_number);
        Ok(LiveEventSubscription {
            events,
            receiver,
            replay_max_sequence,
            resolved_cursor_sequence,
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
        let actor = package_registry_actor(request.requester.clone());
        let request_source = package_registry_request_source(request.request_source.clone());
        let reason = package_registry_reason(request.reason.clone(), "install package");
        self.mutate_extension_registry_with_audit(
            ExtensionRegistryAuditKind::Install,
            request_source,
            reason,
            |registry| registry.install_extension(request),
            |response| ExtensionRegistryAuditContext {
                package_id: Some(response.record.id.clone()),
                record_version: None,
                actor,
                policy_decision_id: None,
                blocklist_entry: None,
            },
            |response, audit_record_id| response.audit_record_id = Some(audit_record_id),
        )
    }

    /// Validate an extension manifest through the registry without installation side effects.
    pub fn validate_extension(
        &self,
        request: ValidateExtensionManifestRequest,
    ) -> ServiceResult<ValidateExtensionManifestResponse> {
        self.lock_extension_registry()?
            .validate_extension_manifest(request)
            .map_err(ServiceError::from)
    }

    pub fn update_extension(
        &self,
        request: UpdateExtensionRequest,
    ) -> ServiceResult<UpdateExtensionResponse> {
        let actor = package_registry_actor(request.requester.clone());
        let request_source = package_registry_request_source(request.request_source.clone());
        let reason = package_registry_reason(request.reason.clone(), "update package");
        self.mutate_extension_registry_with_audit(
            ExtensionRegistryAuditKind::Update,
            request_source,
            reason,
            |registry| registry.update_extension(request),
            |response| ExtensionRegistryAuditContext {
                package_id: Some(response.record.id.clone()),
                record_version: None,
                actor,
                policy_decision_id: None,
                blocklist_entry: None,
            },
            |response, audit_record_id| response.audit_record_id = Some(audit_record_id),
        )
    }

    pub fn extension_status(
        &self,
        request: ExtensionStatusRequest,
    ) -> ServiceResult<ExtensionStatusResponse> {
        self.lock_extension_registry()?
            .extension_status(request)
            .map_err(ServiceError::from)
    }

    /// Returns a single installed-package detail envelope for inspect surfaces.
    pub fn package_inspect(
        &self,
        request: ExtensionStatusRequest,
    ) -> ServiceResult<PackageInspectResponse> {
        let registry = self.lock_extension_registry()?;
        let extension_id = request.extension_id.clone();
        let extension = registry
            .extension_status(ExtensionStatusRequest {
                extension_id: extension_id.clone(),
            })
            .map_err(ServiceError::from)?;
        let record = extension
            .record
            .as_ref()
            .ok_or_else(|| ServiceError::Internal {
                reason: format!(
                    "extension {} is missing active record after status lookup",
                    extension_id
                ),
            })?;
        let snapshot = registry.snapshot();
        let manifest = snapshot
            .manifests
            .get(&extension_id)
            .and_then(|versions| versions.get(&record.version))
            .cloned()
            .ok_or(ServiceError::Store(StoreError::NotFound {
                collection: "extension_manifests",
                id: extension_id.clone(),
            }))?;
        let rollback_snapshot = record
            .previous_version
            .as_ref()
            .and_then(|previous_version| {
                snapshot
                    .manifests
                    .get(&extension_id)
                    .and_then(|versions| versions.get(previous_version))
            })
            .filter(|previous_manifest| {
                !snapshot
                    .blocklist
                    .iter()
                    .any(|entry| entry.matches_manifest(previous_manifest))
            })
            .map(|previous_manifest| RollbackSnapshot {
                manifest_digest: previous_manifest.provenance.manifest_digest.clone(),
                artifact_digest: previous_manifest.provenance.artifact_digest.clone(),
            });
        let rollback_available = rollback_snapshot.is_some();

        Ok(PackageInspectResponse {
            extension: extension.clone(),
            manifest: manifest.clone(),
            block: extension.block.clone(),
            permissions: record.approved_permissions.clone(),
            services: manifest.services.clone(),
            rollback_available,
            rollback_snapshot,
            source: record.source.clone(),
            trust: record.source.publication.clone(),
        })
    }

    pub fn list_extensions(
        &self,
        request: ListExtensionsRequest,
    ) -> ServiceResult<ListExtensionsResponse> {
        self.lock_extension_registry()?
            .list_extensions(request)
            .map_err(ServiceError::from)
    }

    pub fn list_package_trust_index(
        &self,
        request: ListPackageTrustIndexRequest,
    ) -> ServiceResult<ListPackageTrustIndexResponse> {
        let response = self.list_extensions(ListExtensionsRequest {
            include_blocked: request.include_blocked,
        })?;

        let mut packages = response
            .extensions
            .into_iter()
            .map(PackageTrustIndexEntry::from)
            .collect::<Vec<_>>();

        if request.discovery_only {
            packages.retain(|entry| {
                if entry.boundary == Some(ExtensionBoundary::LocalDevelopment) {
                    return false;
                }
                if entry.block.is_some() {
                    return false;
                }
                entry
                    .source
                    .as_ref()
                    .and_then(|source| source.publication.as_ref())
                    .is_some_and(|publication| {
                        publication.visibility
                            == atelia_core::ExtensionPublicationVisibility::PublicSearchable
                            && publication.registry_submission
                                == atelia_core::ExtensionRegistrySubmission::Accepted
                    })
            });
        }

        Ok(ListPackageTrustIndexResponse { packages })
    }

    pub fn rollback_extension(
        &self,
        request: RollbackExtensionRequest,
    ) -> ServiceResult<RollbackExtensionResponse> {
        let actor = package_registry_actor(request.requester.clone());
        let request_source = package_registry_request_source(request.request_source.clone());
        let reason = package_registry_reason(request.reason.clone(), "rollback package");
        self.mutate_extension_registry_with_audit(
            ExtensionRegistryAuditKind::Rollback,
            request_source,
            reason,
            |registry| registry.rollback_extension(request),
            |response| ExtensionRegistryAuditContext {
                package_id: Some(response.record.id.clone()),
                record_version: None,
                actor,
                policy_decision_id: None,
                blocklist_entry: None,
            },
            |response, audit_record_id| response.audit_record_id = Some(audit_record_id),
        )
    }

    pub fn disable_extension(
        &self,
        request: DisableExtensionRequest,
    ) -> ServiceResult<DisableExtensionResponse> {
        let actor = package_registry_actor(request.requester.clone());
        let request_source = package_registry_request_source(request.request_source.clone());
        let reason = package_registry_reason(request.reason.clone(), "disable package");
        self.mutate_extension_registry_with_audit(
            ExtensionRegistryAuditKind::Disable,
            request_source,
            reason,
            |registry| registry.disable_extension(request),
            |response| ExtensionRegistryAuditContext {
                package_id: Some(response.record.id.clone()),
                record_version: None,
                actor,
                policy_decision_id: None,
                blocklist_entry: None,
            },
            |response, audit_record_id| response.audit_record_id = Some(audit_record_id),
        )
    }

    pub fn enable_extension(
        &self,
        request: EnableExtensionRequest,
    ) -> ServiceResult<EnableExtensionResponse> {
        let actor = package_registry_actor(request.requester.clone());
        let request_source = package_registry_request_source(request.request_source.clone());
        let reason = package_registry_reason(request.reason.clone(), "enable package");
        self.mutate_extension_registry_with_audit(
            ExtensionRegistryAuditKind::Enable,
            request_source,
            reason,
            |registry| registry.enable_extension(request),
            |response| ExtensionRegistryAuditContext {
                package_id: Some(response.record.id.clone()),
                record_version: None,
                actor,
                policy_decision_id: None,
                blocklist_entry: None,
            },
            |response, audit_record_id| response.audit_record_id = Some(audit_record_id),
        )
    }

    pub fn remove_extension(
        &self,
        request: RemoveExtensionRequest,
    ) -> ServiceResult<RemoveExtensionResponse> {
        let actor = package_registry_actor(request.requester.clone());
        let request_source = package_registry_request_source(request.request_source.clone());
        let reason = package_registry_reason(request.reason.clone(), "remove package");
        self.mutate_extension_registry_with_audit(
            ExtensionRegistryAuditKind::Remove,
            request_source,
            reason,
            |registry| registry.remove_extension(request),
            |response| ExtensionRegistryAuditContext {
                package_id: Some(response.record.id.clone()),
                record_version: None,
                actor,
                policy_decision_id: None,
                blocklist_entry: None,
            },
            |response, audit_record_id| response.audit_record_id = Some(audit_record_id),
        )
    }

    /// Persist package publication metadata through the Secretary service boundary.
    pub fn update_extension_publication(
        &self,
        request: UpdateExtensionPublicationRequest,
    ) -> ServiceResult<UpdateExtensionPublicationResponse> {
        let actor = package_registry_actor(request.requester.clone());
        let request_source = package_registry_request_source(request.request_source.clone());
        let reason = package_registry_reason(request.reason.clone(), "update package publication");
        self.mutate_extension_registry_with_audit(
            ExtensionRegistryAuditKind::PublicationUpdate,
            request_source,
            reason,
            |registry| registry.update_extension_publication(request),
            |response| ExtensionRegistryAuditContext {
                package_id: Some(response.record.id.clone()),
                record_version: None,
                actor,
                policy_decision_id: None,
                blocklist_entry: None,
            },
            |response, audit_record_id| response.audit_record_id = Some(audit_record_id),
        )
    }

    /// Persist package registry submission state through the Secretary service boundary.
    pub fn update_extension_registry_submission(
        &self,
        request: UpdateExtensionRegistrySubmissionRequest,
    ) -> ServiceResult<UpdateExtensionRegistrySubmissionResponse> {
        let actor = package_registry_actor(request.requester.clone());
        let request_source = package_registry_request_source(request.request_source.clone());
        let reason =
            package_registry_reason(request.reason.clone(), "update package registry submission");
        self.mutate_extension_registry_with_audit(
            ExtensionRegistryAuditKind::RegistrySubmissionUpdate,
            request_source,
            reason,
            |registry| registry.update_extension_registry_submission(request),
            |response| ExtensionRegistryAuditContext {
                package_id: Some(response.record.id.clone()),
                record_version: None,
                actor,
                policy_decision_id: None,
                blocklist_entry: None,
            },
            |response, audit_record_id| response.audit_record_id = Some(audit_record_id),
        )
    }

    pub fn apply_blocklist(
        &self,
        request: ApplyBlocklistRequest,
    ) -> ServiceResult<ApplyBlocklistResponse> {
        let actor = package_registry_actor(request.requester.clone());
        let request_source = package_registry_request_source(request.request_source.clone());
        let reason = package_registry_reason(request.reason.clone(), "apply package blocklist");
        self.mutate_extension_registry_with_audit(
            ExtensionRegistryAuditKind::BlocklistApply,
            request_source,
            reason,
            |registry| registry.apply_blocklist(request),
            |response| ExtensionRegistryAuditContext {
                package_id: match &response.entry.key {
                    BlockKey::ExtensionId(package_id) => Some(package_id.clone()),
                    BlockKey::Version { id, .. } => Some(id.clone()),
                    _ => None,
                },
                record_version: match &response.entry.key {
                    BlockKey::Version { version, .. } => Some(version.clone()),
                    _ => None,
                },
                actor,
                policy_decision_id: None,
                blocklist_entry: Some(response.entry.clone()),
            },
            |response, audit_record_id| response.audit_record_id = Some(audit_record_id),
        )
    }

    pub fn list_blocklist(
        &self,
        request: ListBlocklistRequest,
    ) -> ServiceResult<ListBlocklistResponse> {
        self.lock_extension_registry()?
            .list_blocklist(request)
            .map_err(ServiceError::from)
    }

    pub fn list_extension_registry_audit_records(
        &self,
        request: ListExtensionRegistryAuditRecordsRequest,
    ) -> ServiceResult<ListExtensionRegistryAuditRecordsPage> {
        let requested_limit = request.limit.unwrap_or(MAX_HISTORY_PAGE);
        let limit = requested_limit.min(MAX_HISTORY_PAGE);
        let start = list_history_page_start(
            request.offset,
            request.cursor,
            "extension registry audit history",
        )?;
        if limit == 0 {
            return Ok(ListExtensionRegistryAuditRecordsPage {
                records: Vec::new(),
                next_page_token: None,
            });
        }
        let mut records = self
            .lock_extension_registry()?
            .audit_records_window(start, limit.saturating_add(1));
        let has_next = records.len() > limit;
        if has_next {
            records.truncate(limit);
        }
        let next_page_token = if has_next {
            Some((start + records.len()).to_string())
        } else {
            None
        };
        Ok(ListExtensionRegistryAuditRecordsPage {
            records,
            next_page_token,
        })
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

fn list_history_page_start(
    offset: Option<usize>,
    cursor: Option<String>,
    collection: &'static str,
) -> ServiceResult<usize> {
    let has_offset = offset.is_some();
    let has_cursor = cursor.is_some();
    let cursors_specified = usize::from(has_offset) + usize::from(has_cursor);

    if cursors_specified > 1 {
        return Err(ServiceError::InvalidArgument {
            reason: "only one of offset or cursor may be set".to_string(),
        });
    }

    if let Some(cursor) = cursor {
        return parse_page_token(Some(cursor.as_str()), collection);
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
            SECRETARY_FS_DELETE_TOOL_ID,
            SECRETARY_FS_DELETE_TOOL_NAME,
            SECRETARY_FS_DELETE_TOOL_DESCRIPTION,
            "R2",
            "not idempotent",
            false,
            0,
        ),
        entry(
            SECRETARY_FS_DIFF_TOOL_ID,
            SECRETARY_FS_DIFF_TOOL_NAME,
            SECRETARY_FS_DIFF_TOOL_DESCRIPTION,
            "R1",
            "idempotent",
            false,
            0,
        ),
        entry(
            SECRETARY_FS_SEARCH_TOOL_ID,
            SECRETARY_FS_SEARCH_TOOL_NAME,
            SECRETARY_FS_SEARCH_TOOL_DESCRIPTION,
            "R1",
            "idempotent",
            false,
            0,
        ),
        entry(
            SECRETARY_FS_LIST_TOOL_ID,
            SECRETARY_FS_LIST_TOOL_NAME,
            SECRETARY_FS_LIST_TOOL_DESCRIPTION,
            "R1",
            "idempotent",
            false,
            0,
        ),
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
            SECRETARY_FS_STAT_TOOL_ID,
            SECRETARY_FS_STAT_TOOL_NAME,
            SECRETARY_FS_STAT_TOOL_DESCRIPTION,
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
            SECRETARY_FS_LIST_CAPABILITY => Some(SECRETARY_FS_LIST_CAPABILITY),
            "fs.list" => Some(SECRETARY_FS_LIST_CAPABILITY),
            "fs.read" => Some(SECRETARY_FS_READ_CAPABILITY),
            SECRETARY_FS_STAT_CAPABILITY => Some(SECRETARY_FS_STAT_CAPABILITY),
            "fs.stat" => Some(SECRETARY_FS_STAT_CAPABILITY),
            SECRETARY_FS_DELETE_CAPABILITY => Some(SECRETARY_FS_DELETE_CAPABILITY),
            "fs.delete" => Some(SECRETARY_FS_DELETE_CAPABILITY),
            SECRETARY_FS_SEARCH_CAPABILITY => Some(SECRETARY_FS_SEARCH_CAPABILITY),
            "fs.search" => Some(SECRETARY_FS_SEARCH_CAPABILITY),
            SECRETARY_FS_DIFF_CAPABILITY => Some(SECRETARY_FS_DIFF_CAPABILITY),
            "fs.diff" => Some(SECRETARY_FS_DIFF_CAPABILITY),
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
        [capability]
            if matches!(
                capability.as_str(),
                SECRETARY_FS_READ_CAPABILITY
                    | SECRETARY_FS_LIST_CAPABILITY
                    | SECRETARY_FS_STAT_CAPABILITY
                    | SECRETARY_FS_DELETE_CAPABILITY
                    | SECRETARY_FS_SEARCH_CAPABILITY
                    | SECRETARY_FS_DIFF_CAPABILITY
            ) =>
        {
            let resource_scope =
                request
                    .resource_scope
                    .as_ref()
                    .ok_or_else(|| ServiceError::InvalidArgument {
                        reason: format!("{capability} requires a path_scope/resource_scope"),
                    })?;

            if capability == SECRETARY_FS_DELETE_CAPABILITY && request.kind != JobKind::Mutate {
                return Err(ServiceError::InvalidArgument {
                    reason: "filesystem.delete requires job kind mutate".to_string(),
                });
            }

            if capability == SECRETARY_FS_DELETE_CAPABILITY
                && resource_scope.kind.trim() == "read_only"
            {
                return Err(ServiceError::InvalidArgument {
                    reason: "filesystem.delete requires resource_scope.kind to be repository, explicit_paths, or path"
                        .to_string(),
                });
            }

            if !matches!(
                resource_scope.kind.trim(),
                "repository" | "explicit_paths" | "read_only" | "path"
            ) {
                return Err(ServiceError::InvalidArgument {
                    reason: format!(
                        "{capability} requires resource_scope.kind to be repository, explicit_paths, read_only, or path"
                    ),
                });
            }

            match capability.as_str() {
                SECRETARY_FS_LIST_CAPABILITY => Ok(SubmitJobToolKind::FsList),
                SECRETARY_FS_STAT_CAPABILITY => Ok(SubmitJobToolKind::FsStat),
                SECRETARY_FS_DELETE_CAPABILITY => Ok(SubmitJobToolKind::FsDelete),
                SECRETARY_FS_SEARCH_CAPABILITY => Ok(SubmitJobToolKind::FsSearch),
                SECRETARY_FS_DIFF_CAPABILITY => Ok(SubmitJobToolKind::FsDiff),
                _ => Ok(SubmitJobToolKind::FsRead),
            }
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

fn validate_filesystem_path_scope(
    repository: &RepositoryRecord,
    resource_scope: &ResourceScope,
    require_concrete_path: bool,
) -> ServiceResult<()> {
    let root = Path::new(&repository.root_path);
    let requested =
        canonicalize_within_scope(root, Path::new(&resource_scope.value)).map_err(|err| {
            ServiceError::InvalidArgument {
                reason: format!("filesystem path is outside repository scope: {err}"),
            }
        })?;

    if require_concrete_path && requested.canonical == requested.root {
        return Err(ServiceError::InvalidArgument {
            reason: "filesystem operation requires a concrete path_scope/resource_scope path"
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
            reason: "filesystem operation path is outside allowed_path_scope".to_string(),
        })
    }
}

fn allowed_search_roots_for_repository(
    repository: &RepositoryRecord,
) -> ServiceResult<Vec<PathBuf>> {
    let root = Path::new(&repository.root_path);
    let mut allowed_roots = repository
        .allowed_path_scope
        .allowed_paths
        .iter()
        .map(|path| {
            canonicalize_within_scope(root, Path::new(path)).map(|resolved| resolved.canonical)
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|err| ServiceError::Internal {
            reason: format!("allowed path scope is invalid during submit_job: {err}"),
        })?;

    if allowed_roots.is_empty() {
        allowed_roots.push(root.canonicalize().map_err(|err| ServiceError::Internal {
            reason: format!("repository root is no longer valid: {err}"),
        })?);
    }

    allowed_roots.sort();
    allowed_roots.dedup();
    Ok(allowed_roots)
}

fn validate_secondary_filesystem_path_scope(
    repository: &RepositoryRecord,
    primary_scope: &ResourceScope,
    comparison_path: &str,
) -> ServiceResult<()> {
    let root = Path::new(&repository.root_path);
    let requested = canonicalize_within_scope(root, Path::new(comparison_path)).map_err(|err| {
        ServiceError::InvalidArgument {
            reason: format!("comparison path is outside repository scope: {err}"),
        }
    })?;

    if requested.canonical == requested.root {
        return Err(ServiceError::InvalidArgument {
            reason: "comparison path must be concrete".to_string(),
        });
    }

    let _ = canonicalize_within_scope(root, Path::new(&primary_scope.value)).map_err(|err| {
        ServiceError::InvalidArgument {
            reason: format!("primary path is outside repository scope: {err}"),
        }
    })?;

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
            reason: "comparison path is outside allowed_path_scope".to_string(),
        })
    }
}

fn resolve_submit_job_tool_args(
    tool_kind: &SubmitJobToolKind,
    tool_args: Option<SubmitJobToolArgs>,
) -> ServiceResult<SubmitJobToolArgsSpec> {
    let missing_tool_args = || ServiceError::InvalidArgument {
        reason: "tool_args is required for this capability".to_string(),
    };

    match tool_kind {
        SubmitJobToolKind::FsSearch => {
            let args = tool_args.ok_or_else(missing_tool_args)?;
            if args.comparison_path.is_some()
                || args.max_bytes.is_some()
                || args.max_chars.is_some()
            {
                return Err(ServiceError::InvalidArgument {
                    reason: "search tool_args only supports pattern and max".to_string(),
                });
            }

            let pattern = args.pattern.ok_or_else(|| ServiceError::InvalidArgument {
                reason: "search requires non-empty pattern in tool_args".to_string(),
            })?;
            if pattern.trim().is_empty() {
                return Err(ServiceError::InvalidArgument {
                    reason: "search requires non-empty pattern in tool_args".to_string(),
                });
            }

            let max_results = args
                .max
                .map(|max| {
                    usize::try_from(max).map_err(|_| ServiceError::InvalidArgument {
                        reason: "search max must fit in usize".to_string(),
                    })
                })
                .transpose()?;

            Ok(SubmitJobToolArgsSpec::Search(SubmitJobToolArgsSearch {
                pattern,
                max_results,
            }))
        }
        SubmitJobToolKind::FsDiff => {
            let args = tool_args.ok_or_else(missing_tool_args)?;
            if args.pattern.is_some() || args.max.is_some() {
                return Err(ServiceError::InvalidArgument {
                    reason:
                        "diff tool_args only supports comparison_path, max_bytes, and max_chars"
                            .to_string(),
                });
            }

            let comparison_path =
                args.comparison_path
                    .ok_or_else(|| ServiceError::InvalidArgument {
                        reason: "diff requires non-empty comparison_path in tool_args".to_string(),
                    })?;
            if comparison_path.trim().is_empty() {
                return Err(ServiceError::InvalidArgument {
                    reason: "diff requires non-empty comparison_path in tool_args".to_string(),
                });
            }

            let max_bytes = args
                .max_bytes
                .map(|max_bytes| {
                    usize::try_from(max_bytes).map_err(|_| ServiceError::InvalidArgument {
                        reason: "max_bytes must fit in usize".to_string(),
                    })
                })
                .transpose()?;
            let max_chars = args
                .max_chars
                .map(|max_chars| {
                    usize::try_from(max_chars).map_err(|_| ServiceError::InvalidArgument {
                        reason: "max_chars must fit in usize".to_string(),
                    })
                })
                .transpose()?;

            Ok(SubmitJobToolArgsSpec::Diff(SubmitJobToolArgsDiff {
                comparison_path,
                max_bytes,
                max_chars,
            }))
        }
        _ if tool_args.is_some() => Err(ServiceError::InvalidArgument {
            reason: "tool_args are only supported for filesystem.search and filesystem.diff"
                .to_string(),
        }),
        _ => Ok(SubmitJobToolArgsSpec::None),
    }
}

fn normalize_submit_job_goal(goal: Option<String>) -> Option<String> {
    goal.and_then(|goal| {
        let trimmed = goal.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

/// Build the canonical request signature used to compare idempotent submit-job retries.
fn submit_job_request_signature(
    request: &SubmitJobRequest,
    normalized_goal: Option<&str>,
    requested_capabilities: &[String],
) -> String {
    #[derive(Serialize)]
    struct SubmitJobRequestSignature<'a> {
        actor: &'a Actor,
        repository_id: &'a str,
        kind: &'a JobKind,
        goal: Option<&'a str>,
        resource_scope: &'a ResourceScope,
        requested_capabilities: &'a [String],
        tool_args: &'a Option<SubmitJobToolArgs>,
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
        tool_args: &request.tool_args,
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
        ApplyBlocklistRequest, BlockKey, BlockReason, BlocklistEntry, EventRefs, EventSeverity,
        EventSubject, ExtensionCompatibility, ExtensionEntrypoints, ExtensionFailure,
        ExtensionKind, ExtensionManifest, ExtensionPermission, ExtensionPublisher, ExtensionRealm,
        ExtensionRuntime, ExtensionServices, InstallExtensionRequest, JobEventId, JobEventKind,
        LedgerTimestamp, ListBlocklistRequest, ListExtensionsRequest, PolicyDecision,
        PolicyDecisionId, PolicyOutcome, ProvenanceSource, RepositoryTrustState, ResourceScope,
        RetryPolicy, RiskTier, RollbackExtensionRequest, ValidateExtensionManifestRequest,
        EXTENSION_MANIFEST_SCHEMA, EXTENSION_RPC_PROTOCOL,
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

    fn test_job_event(repository_id: &RepositoryId, sequence_number: u64) -> JobEvent {
        JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number,
            created_at: LedgerTimestamp::from_unix_millis(sequence_number as i64),
            subject: EventSubject::repository(repository_id),
            kind: JobEventKind::Message,
            severity: EventSeverity::Info,
            public_message: format!("event {sequence_number}"),
            refs: EventRefs {
                repository_id: Some(repository_id.clone()),
                ..Default::default()
            },
            redactions: Vec::new(),
        }
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
                source_ref: None,
                manifest_path: None,
                commit: Some("deadbeef".to_string()),
                registry_identity: Some("third-party-registry".to_string()),
                lineage: None,
                publication: None,
                artifact_digest: artifact_digest.to_string(),
                manifest_digest: manifest_digest.to_string(),
                signature: Some("signature".to_string()),
                signer: Some("signer@example.com".to_string()),
            },
            bundle: None,
            migration: Default::default(),
        }
    }

    fn local_unsigned_extension_manifest(
        id: &str,
        version: &str,
        artifact_digest: &str,
        manifest_digest: &str,
    ) -> ExtensionManifest {
        let mut manifest = extension_manifest(id, version, artifact_digest, manifest_digest);
        manifest.provenance.source = ProvenanceSource::Local;
        manifest.provenance.registry_identity = None;
        manifest.provenance.signature = None;
        manifest.provenance.signer = None;
        manifest
    }

    fn local_process_extension_manifest(
        id: &str,
        version: &str,
        artifact_digest: &str,
        manifest_digest: &str,
    ) -> ExtensionManifest {
        let mut manifest =
            local_unsigned_extension_manifest(id, version, artifact_digest, manifest_digest);
        manifest.provenance.signature = Some("signature".to_string());
        manifest.provenance.signer = Some("signer@example.com".to_string());
        manifest.entrypoints.runtime = ExtensionRuntime::Process;
        manifest.entrypoints.wasm = None;
        manifest.entrypoints.command = Some("cargo run".to_string());
        manifest
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
            .contains(&"package_inspect.v1".to_string()));
        assert!(health
            .capabilities
            .contains(&"package_trust_index.v1".to_string()));
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
                approve_source_change: false,
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("first install should succeed");
        assert_eq!(installed_v1.record.version, "1.0.0");

        let installed_v2 = svc
            .install_extension(InstallExtensionRequest {
                manifest: manifest_v2.clone(),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
                requester: None,
                request_source: None,
                reason: None,
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
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("rollback should succeed");
        assert_eq!(rolled_back.record.version, "1.0.0");

        let disabled = svc
            .disable_extension(DisableExtensionRequest {
                extension_id: "com.example.review.extension".to_string(),
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("disable should succeed");
        assert_eq!(
            disabled.record.status,
            atelia_core::ExtensionInstallStatus::Disabled
        );

        let enabled = svc
            .enable_extension(EnableExtensionRequest {
                extension_id: "com.example.review.extension".to_string(),
                requester: None,
                request_source: None,
                reason: None,
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
                requester: None,
                request_source: None,
                reason: None,
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
                requester: None,
                request_source: None,
                reason: None,
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
                requester: None,
                request_source: None,
                reason: None,
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
    fn package_registry_audit_metadata_is_bounded_before_persisting() {
        let svc = ready_service();

        let installed = svc
            .install_extension(InstallExtensionRequest {
                manifest: extension_manifest(
                    "com.example.bounded-audit",
                    "1.0.0",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                ),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
                requester: None,
                request_source: Some("s".repeat(PACKAGE_REGISTRY_REQUEST_SOURCE_MAX_CHARS + 16)),
                reason: Some("r".repeat(PACKAGE_REGISTRY_REASON_MAX_CHARS + 16)),
            })
            .expect("install should persist bounded audit metadata");
        let audit_record_id = installed
            .audit_record_id
            .expect("install should return audit record id");

        let audit_page = svc
            .list_extension_registry_audit_records(ListExtensionRegistryAuditRecordsRequest {
                limit: Some(1),
                offset: None,
                cursor: None,
            })
            .expect("audit records should be listed");
        let audit_record = audit_page
            .records
            .into_iter()
            .find(|record| record.id == audit_record_id)
            .expect("audit record should be persisted");

        assert_eq!(
            audit_record.request_source.chars().count(),
            PACKAGE_REGISTRY_REQUEST_SOURCE_MAX_CHARS
        );
        assert_eq!(
            audit_record.reason.chars().count(),
            PACKAGE_REGISTRY_REASON_MAX_CHARS
        );
    }

    #[test]
    fn version_blocklist_audits_target_blocked_version() {
        const ARTIFACT_V1: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const MANIFEST_V1: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        const ARTIFACT_V2: &str =
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        const MANIFEST_V2: &str =
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

        let svc = ready_service();
        svc.install_extension(InstallExtensionRequest {
            manifest: extension_manifest(
                "com.example.version-blocked",
                "1.0.0",
                ARTIFACT_V1,
                MANIFEST_V1,
            ),
            approve_local_unsigned: false,
            allow_local_process_runtime: false,
            approve_source_change: false,
            requester: None,
            request_source: None,
            reason: None,
        })
        .expect("first install should succeed");
        svc.install_extension(InstallExtensionRequest {
            manifest: extension_manifest(
                "com.example.version-blocked",
                "2.0.0",
                ARTIFACT_V2,
                MANIFEST_V2,
            ),
            approve_local_unsigned: false,
            allow_local_process_runtime: false,
            approve_source_change: false,
            requester: None,
            request_source: None,
            reason: None,
        })
        .expect("second install should succeed");

        let block = svc
            .apply_blocklist(ApplyBlocklistRequest {
                entry: atelia_core::BlocklistEntry {
                    key: BlockKey::Version {
                        id: "com.example.version-blocked".to_string(),
                        version: "1.0.0".to_string(),
                    },
                    reason: BlockReason::PolicyViolation,
                    note: None,
                },
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("version blocklist should audit the targeted version");
        let audit_record_id = block
            .audit_record_id
            .expect("blocklist should return audit record id");

        let audit_page = svc
            .list_extension_registry_audit_records(ListExtensionRegistryAuditRecordsRequest {
                limit: Some(10),
                offset: None,
                cursor: None,
            })
            .expect("audit records should be listed");
        let audit_record = audit_page
            .records
            .into_iter()
            .find(|record| record.id == audit_record_id)
            .expect("blocklist audit record should be persisted");

        assert_eq!(
            audit_record
                .previous_record
                .as_ref()
                .map(|record| record.version.as_str()),
            Some("1.0.0")
        );
        assert_eq!(
            audit_record
                .new_record
                .as_ref()
                .map(|record| record.version.as_str()),
            Some("1.0.0")
        );
    }

    #[test]
    fn package_trust_index_lists_active_and_blocked_packages_with_provenance() {
        let svc = ready_service();

        const ACTIVE_ARTIFACT: &str =
            "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        const ACTIVE_MANIFEST: &str =
            "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        const BLOCKED_ARTIFACT: &str =
            "sha256:1111111111111111111111111111111111111111111111111111111111111111";
        const BLOCKED_MANIFEST: &str =
            "sha256:2222222222222222222222222222222222222222222222222222222222222222";

        let mut active = extension_manifest(
            "com.example.active",
            "1.0.0",
            ACTIVE_ARTIFACT,
            ACTIVE_MANIFEST,
        );
        active.provenance.publication = Some(atelia_core::ExtensionPublication {
            visibility: atelia_core::ExtensionPublicationVisibility::PublicSearchable,
            registry_submission: atelia_core::ExtensionRegistrySubmission::Accepted,
        });

        let mut blocked = extension_manifest(
            "com.example.blocked",
            "1.0.0",
            BLOCKED_ARTIFACT,
            BLOCKED_MANIFEST,
        );
        blocked.provenance.lineage = Some(atelia_core::ExtensionLineage {
            parent_id: "com.example.parent".to_string(),
            parent_version: Some("0.9.0".to_string()),
            parent_manifest_digest: Some(ACTIVE_MANIFEST.to_string()),
            relationship: atelia_core::ExtensionLineageRelationship::Fork,
        });
        blocked.provenance.publication = Some(atelia_core::ExtensionPublication {
            visibility: atelia_core::ExtensionPublicationVisibility::PrivateRemix,
            registry_submission: atelia_core::ExtensionRegistrySubmission::NotSubmitted,
        });

        svc.install_extension(InstallExtensionRequest {
            manifest: active,
            approve_local_unsigned: false,
            allow_local_process_runtime: false,
            approve_source_change: false,
            requester: None,
            request_source: None,
            reason: None,
        })
        .expect("active install should succeed");
        svc.install_extension(InstallExtensionRequest {
            manifest: blocked,
            approve_local_unsigned: false,
            allow_local_process_runtime: false,
            approve_source_change: false,
            requester: None,
            request_source: None,
            reason: None,
        })
        .expect("blocked install should succeed");
        svc.apply_blocklist(ApplyBlocklistRequest {
            entry: atelia_core::BlocklistEntry {
                key: BlockKey::ExtensionId("com.example.blocked".to_string()),
                reason: BlockReason::PolicyViolation,
                note: Some("blocked for trust index".to_string()),
            },
            requester: None,
            request_source: None,
            reason: None,
        })
        .expect("blocklist should apply");

        let index = svc
            .list_package_trust_index(ListPackageTrustIndexRequest::default())
            .expect("package trust index should succeed");

        assert_eq!(index.packages.len(), 2);

        let unblocked_index = svc
            .list_package_trust_index(ListPackageTrustIndexRequest {
                include_blocked: false,
                discovery_only: false,
            })
            .expect("unblocked package trust index should succeed");
        assert_eq!(unblocked_index.packages.len(), 1);
        assert!(unblocked_index
            .packages
            .iter()
            .all(|entry| entry.package_id != "com.example.blocked"));

        let active_entry = index
            .packages
            .iter()
            .find(|entry| entry.package_id == "com.example.active")
            .expect("active package should be listed");
        assert_eq!(active_entry.status, Some(ExtensionInstallStatus::Installed));
        assert_eq!(
            active_entry.source.as_ref().unwrap().publication,
            Some(atelia_core::ExtensionPublication {
                visibility: atelia_core::ExtensionPublicationVisibility::PublicSearchable,
                registry_submission: atelia_core::ExtensionRegistrySubmission::Accepted,
            })
        );

        let blocked_entry = index
            .packages
            .iter()
            .find(|entry| entry.package_id == "com.example.blocked")
            .expect("blocked package should remain visible");
        assert_eq!(blocked_entry.status, Some(ExtensionInstallStatus::Blocked));
        assert!(blocked_entry.block.is_some());
        assert_eq!(
            blocked_entry.source.as_ref().unwrap().publication,
            Some(atelia_core::ExtensionPublication {
                visibility: atelia_core::ExtensionPublicationVisibility::PrivateRemix,
                registry_submission: atelia_core::ExtensionRegistrySubmission::NotSubmitted,
            })
        );
        assert_eq!(
            blocked_entry.source.as_ref().unwrap().lineage,
            Some(atelia_core::ExtensionLineage {
                parent_id: "com.example.parent".to_string(),
                parent_version: Some("0.9.0".to_string()),
                parent_manifest_digest: Some(ACTIVE_MANIFEST.to_string()),
                relationship: atelia_core::ExtensionLineageRelationship::Fork,
            })
        );
    }

    #[test]
    fn package_trust_index_request_defaults_to_full_index() {
        let request: ListPackageTrustIndexRequest = serde_json::from_str("{}").unwrap();

        assert!(request.include_blocked);
        assert!(!request.discovery_only);
        assert_eq!(request, ListPackageTrustIndexRequest::default());
    }

    #[test]
    fn package_trust_index_request_deserializes_filtered_discovery() {
        let request: ListPackageTrustIndexRequest =
            serde_json::from_str("{\"include_blocked\":false,\"discovery_only\":true}").unwrap();

        assert!(!request.include_blocked);
        assert!(request.discovery_only);
    }

    #[test]
    fn package_trust_index_discovery_only_filters_discoverable_packages() {
        let svc = ready_service();

        const PUBLIC_ARTIFACT: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const PUBLIC_MANIFEST: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        const SUBMITTED_ARTIFACT: &str =
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        const SUBMITTED_MANIFEST: &str =
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
        const PRIVATE_ARTIFACT: &str =
            "sha256:eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee";
        const PRIVATE_MANIFEST: &str =
            "sha256:ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
        const LOCAL_ARTIFACT: &str =
            "sha256:1111111111111111111111111111111111111111111111111111111111111111";
        const LOCAL_MANIFEST: &str =
            "sha256:2222222222222222222222222222222222222222222222222222222222222222";
        const BLOCKED_ARTIFACT: &str =
            "sha256:3333333333333333333333333333333333333333333333333333333333333333";
        const BLOCKED_MANIFEST: &str =
            "sha256:4444444444444444444444444444444444444444444444444444444444444444";

        let mut discoverable_public = extension_manifest(
            "com.example.discoverable",
            "1.0.0",
            PUBLIC_ARTIFACT,
            PUBLIC_MANIFEST,
        );
        discoverable_public.provenance.publication = Some(atelia_core::ExtensionPublication {
            visibility: atelia_core::ExtensionPublicationVisibility::PublicSearchable,
            registry_submission: atelia_core::ExtensionRegistrySubmission::Accepted,
        });

        let mut submitted_public = extension_manifest(
            "com.example.submitted",
            "1.0.0",
            SUBMITTED_ARTIFACT,
            SUBMITTED_MANIFEST,
        );
        submitted_public.provenance.publication = Some(atelia_core::ExtensionPublication {
            visibility: atelia_core::ExtensionPublicationVisibility::PublicSearchable,
            registry_submission: atelia_core::ExtensionRegistrySubmission::Submitted,
        });

        let mut private_remix = extension_manifest(
            "com.example.private-remix",
            "1.0.0",
            PRIVATE_ARTIFACT,
            PRIVATE_MANIFEST,
        );
        private_remix.provenance.publication = Some(atelia_core::ExtensionPublication {
            visibility: atelia_core::ExtensionPublicationVisibility::PrivateRemix,
            registry_submission: atelia_core::ExtensionRegistrySubmission::NotSubmitted,
        });

        let local_development = local_unsigned_extension_manifest(
            "local.example.local-development",
            "1.0.0",
            LOCAL_ARTIFACT,
            LOCAL_MANIFEST,
        );

        let mut blocked_public = extension_manifest(
            "com.example.blocked-searchable",
            "1.0.0",
            BLOCKED_ARTIFACT,
            BLOCKED_MANIFEST,
        );
        blocked_public.provenance.publication = Some(atelia_core::ExtensionPublication {
            visibility: atelia_core::ExtensionPublicationVisibility::PublicSearchable,
            registry_submission: atelia_core::ExtensionRegistrySubmission::Accepted,
        });

        svc.install_extension(InstallExtensionRequest {
            manifest: discoverable_public,
            approve_local_unsigned: false,
            allow_local_process_runtime: false,
            approve_source_change: false,
            requester: None,
            request_source: None,
            reason: None,
        })
        .expect("discoverable public install should succeed");
        svc.install_extension(InstallExtensionRequest {
            manifest: submitted_public,
            approve_local_unsigned: false,
            allow_local_process_runtime: false,
            approve_source_change: false,
            requester: None,
            request_source: None,
            reason: None,
        })
        .expect("submitted public install should succeed");
        svc.install_extension(InstallExtensionRequest {
            manifest: private_remix,
            approve_local_unsigned: false,
            allow_local_process_runtime: false,
            approve_source_change: false,
            requester: None,
            request_source: None,
            reason: None,
        })
        .expect("private remix install should succeed");
        svc.install_extension(InstallExtensionRequest {
            manifest: local_development,
            approve_local_unsigned: true,
            allow_local_process_runtime: false,
            approve_source_change: false,
            requester: None,
            request_source: None,
            reason: None,
        })
        .expect("local development install should succeed");
        svc.install_extension(InstallExtensionRequest {
            manifest: blocked_public,
            approve_local_unsigned: false,
            allow_local_process_runtime: false,
            approve_source_change: false,
            requester: None,
            request_source: None,
            reason: None,
        })
        .expect("blocked searchable install should succeed");
        svc.apply_blocklist(ApplyBlocklistRequest {
            entry: atelia_core::BlocklistEntry {
                key: BlockKey::ExtensionId("com.example.blocked-searchable".to_string()),
                reason: BlockReason::PolicyViolation,
                note: Some("blocked for discovery test".to_string()),
            },
            requester: None,
            request_source: None,
            reason: None,
        })
        .expect("blocklist should apply");

        let discovery_index = svc
            .list_package_trust_index(ListPackageTrustIndexRequest {
                include_blocked: true,
                discovery_only: true,
            })
            .expect("discovery trust index should succeed");

        assert_eq!(discovery_index.packages.len(), 1);
        let discoverable_entry = discovery_index
            .packages
            .iter()
            .find(|entry| entry.package_id == "com.example.discoverable")
            .expect("discoverable package should appear in discovery index");
        assert_eq!(
            discoverable_entry.source.as_ref().unwrap().publication,
            Some(atelia_core::ExtensionPublication {
                visibility: atelia_core::ExtensionPublicationVisibility::PublicSearchable,
                registry_submission: atelia_core::ExtensionRegistrySubmission::Accepted,
            })
        );

        assert!(discovery_index
            .packages
            .iter()
            .all(|entry| entry.package_id != "com.example.private-remix"));
        assert!(discovery_index
            .packages
            .iter()
            .all(|entry| entry.package_id != "com.example.submitted"));
        assert!(discovery_index
            .packages
            .iter()
            .all(|entry| entry.package_id != "local.example.local-development"));
        assert!(discovery_index
            .packages
            .iter()
            .all(|entry| entry.package_id != "com.example.blocked-searchable"));
    }

    #[test]
    /// Valid manifest preflight validation leaves service-visible extension state unchanged.
    fn extension_validation_does_not_mutate_extension_state() {
        let svc = ready_service();

        let manifest_v1 = extension_manifest(
            "com.example.validate.extension",
            "1.0.0",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );
        let manifest_v2 = extension_manifest(
            "com.example.validate.extension",
            "2.0.0",
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        );

        svc.install_extension(InstallExtensionRequest {
            manifest: manifest_v1,
            approve_local_unsigned: false,
            allow_local_process_runtime: false,
            approve_source_change: false,
            requester: None,
            request_source: None,
            reason: None,
        })
        .expect("baseline install should succeed");

        let pre_extensions = svc
            .list_extensions(ListExtensionsRequest {
                include_blocked: true,
            })
            .expect("baseline extension list should succeed");
        let pre_blocklist = svc
            .list_blocklist(ListBlocklistRequest {})
            .expect("baseline blocklist should succeed");
        let pre_jobs = svc
            .list_jobs(None, None, None, None, None)
            .expect("baseline job query should succeed");
        let pre_trust_index = svc
            .list_package_trust_index(ListPackageTrustIndexRequest::default())
            .expect("baseline trust index should succeed");

        let validated = svc
            .validate_extension(ValidateExtensionManifestRequest {
                manifest: manifest_v2,
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
            })
            .expect("validation should succeed");
        assert_eq!(validated.boundary, ExtensionBoundary::ThirdParty);
        assert_eq!(validated.manifest.id, "com.example.validate.extension");

        let post_extensions = svc
            .list_extensions(ListExtensionsRequest {
                include_blocked: true,
            })
            .expect("post validation extension list should succeed");
        let post_blocklist = svc
            .list_blocklist(ListBlocklistRequest {})
            .expect("post validation blocklist should succeed");
        let post_jobs = svc
            .list_jobs(None, None, None, None, None)
            .expect("post validation job query should succeed");
        let post_trust_index = svc
            .list_package_trust_index(ListPackageTrustIndexRequest::default())
            .expect("post trust index should succeed");

        assert_eq!(post_extensions, pre_extensions);
        assert_eq!(post_blocklist, pre_blocklist);
        assert_eq!(post_jobs, pre_jobs);
        assert_eq!(post_trust_index, pre_trust_index);
    }

    #[test]
    /// Invalid manifest preflight validation leaves service-visible extension state unchanged.
    fn extension_validation_rejects_invalid_manifest_without_state_change() {
        let svc = ready_service();

        let invalid_manifest = extension_manifest(
            "com.example.validate.extension",
            "not-semver",
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
        );

        let pre_extensions = svc
            .list_extensions(ListExtensionsRequest {
                include_blocked: true,
            })
            .expect("baseline extension list should succeed");
        let pre_blocklist = svc
            .list_blocklist(ListBlocklistRequest {})
            .expect("baseline blocklist should succeed");
        let pre_jobs = svc
            .list_jobs(None, None, None, None, None)
            .expect("baseline job query should succeed");
        let pre_trust_index = svc
            .list_package_trust_index(ListPackageTrustIndexRequest::default())
            .expect("baseline trust index should succeed");

        let err = svc
            .validate_extension(ValidateExtensionManifestRequest {
                manifest: invalid_manifest,
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
            })
            .unwrap_err();
        assert!(matches!(
            err,
            ServiceError::ExtensionRegistry(RegistryError::Validation(_))
        ));

        let post_extensions = svc
            .list_extensions(ListExtensionsRequest {
                include_blocked: true,
            })
            .expect("post validation extension list should succeed");
        let post_blocklist = svc
            .list_blocklist(ListBlocklistRequest {})
            .expect("post validation blocklist should succeed");
        let post_jobs = svc
            .list_jobs(None, None, None, None, None)
            .expect("post validation job query should succeed");
        let post_trust_index = svc
            .list_package_trust_index(ListPackageTrustIndexRequest::default())
            .expect("post trust index should succeed");

        assert_eq!(post_extensions, pre_extensions);
        assert_eq!(post_blocklist, pre_blocklist);
        assert_eq!(post_jobs, pre_jobs);
        assert_eq!(post_trust_index, pre_trust_index);
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
                approve_source_change: false,
                requester: None,
                request_source: None,
                reason: None,
            })
            .map(|_| "install_extension"),
            svc.validate_extension(ValidateExtensionManifestRequest {
                manifest: manifest.clone(),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
            })
            .map(|_| "validate_extension"),
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
                requester: None,
                request_source: None,
                reason: None,
            })
            .map(|_| "rollback_extension"),
            svc.apply_blocklist(ApplyBlocklistRequest {
                entry: atelia_core::BlocklistEntry {
                    key: BlockKey::ExtensionId("com.example.review.extension".to_string()),
                    reason: BlockReason::UserBlocked,
                    note: Some("poison check".to_string()),
                },
                requester: None,
                request_source: None,
                reason: None,
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
                goal: Some("persist me".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some("persist me".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some("first goal".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some("different goal".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some("blocked request".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some("blocked request".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
                idempotency_key: Some("blocked-key".to_string()),
            })
            .expect("blocked submit should execute again after restart");
        assert_eq!(second.job.status, JobStatus::Blocked);
        assert_ne!(second.job.id, first_job_id);

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(storage_dir);
    }

    #[test]
    fn durable_extension_registry_replays_update_rollback_disable_enable() {
        let storage_dir = durable_storage_dir("extension-restart-cycle");
        let service = SecretaryService::new_durable(storage_dir.clone()).expect("durable service");

        const ARTIFACT_V1: &str =
            "sha256:1111111111111111111111111111111111111111111111111111111111111111";
        const MANIFEST_V1: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const ARTIFACT_V2: &str =
            "sha256:2222222222222222222222222222222222222222222222222222222222222222";
        const MANIFEST_V2: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        service
            .install_extension(InstallExtensionRequest {
                manifest: extension_manifest(
                    "com.example.restart.extension",
                    "1.0.0",
                    ARTIFACT_V1,
                    MANIFEST_V1,
                ),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("first install should persist");
        service
            .install_extension(InstallExtensionRequest {
                manifest: extension_manifest(
                    "com.example.restart.extension",
                    "2.0.0",
                    ARTIFACT_V2,
                    MANIFEST_V2,
                ),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("update should persist");

        let rolled_back = service
            .rollback_extension(RollbackExtensionRequest {
                extension_id: "com.example.restart.extension".to_string(),
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("rollback should persist");
        assert_eq!(rolled_back.record.version, "1.0.0");

        let disabled = service
            .disable_extension(DisableExtensionRequest {
                extension_id: "com.example.restart.extension".to_string(),
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("disable should persist");
        assert_eq!(disabled.record.status, ExtensionInstallStatus::Disabled);

        service
            .enable_extension(EnableExtensionRequest {
                extension_id: "com.example.restart.extension".to_string(),
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("enable should persist");

        drop(service);

        let service = SecretaryService::new_durable(storage_dir.clone()).expect("durable reload");
        let status = service
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.restart.extension".to_string(),
            })
            .expect("status should restore");
        let record = status.record.expect("record should be present");
        assert_eq!(record.version, "1.0.0");
        assert_eq!(record.status, ExtensionInstallStatus::Installed);

        let _ = fs::remove_dir_all(storage_dir);
    }

    #[test]
    fn durable_extension_registry_persists_local_unsigned_approval() {
        let storage_dir = durable_storage_dir("extension-local-unsigned");
        let service = SecretaryService::new_durable(storage_dir.clone())
            .expect("durable service should initialize");

        let installed = service
            .install_extension(InstallExtensionRequest {
                manifest: local_unsigned_extension_manifest(
                    "local.example.unsigned",
                    "1.0.0",
                    "sha256:1111111111111111111111111111111111111111111111111111111111111111",
                    "sha256:2222222222222222222222222222222222222222222222222222222222222222",
                ),
                approve_local_unsigned: true,
                allow_local_process_runtime: false,
                approve_source_change: false,
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("local unsigned install should persist");
        assert_eq!(
            installed.record.boundary,
            ExtensionBoundary::LocalDevelopment
        );

        drop(service);

        let service =
            SecretaryService::new_durable(storage_dir.clone()).expect("durable service reload");
        let status = service
            .extension_status(ExtensionStatusRequest {
                extension_id: "local.example.unsigned".to_string(),
            })
            .expect("extension should survive durable reload");
        let record = status.record.expect("record should be present");
        assert_eq!(record.version, "1.0.0");
        assert_eq!(record.boundary, ExtensionBoundary::LocalDevelopment);
        let _ = fs::remove_dir_all(storage_dir);
    }

    #[test]
    fn durable_extension_registry_persists_local_process_runtime_approval() {
        let storage_dir = durable_storage_dir("extension-local-process");
        let service = SecretaryService::new_durable(storage_dir.clone())
            .expect("durable service should initialize");

        let installed = service
            .install_extension(InstallExtensionRequest {
                manifest: local_process_extension_manifest(
                    "local.example.process",
                    "1.0.0",
                    "sha256:3333333333333333333333333333333333333333333333333333333333333333",
                    "sha256:4444444444444444444444444444444444444444444444444444444444444444",
                ),
                approve_local_unsigned: false,
                allow_local_process_runtime: true,
                approve_source_change: false,
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("local process install should persist");
        assert_eq!(
            installed.record.boundary,
            ExtensionBoundary::LocalDevelopment
        );

        drop(service);

        let service =
            SecretaryService::new_durable(storage_dir.clone()).expect("durable service reload");
        let status = service
            .extension_status(ExtensionStatusRequest {
                extension_id: "local.example.process".to_string(),
            })
            .expect("extension should survive durable reload");
        let record = status.record.expect("record should be present");
        assert_eq!(record.version, "1.0.0");
        assert_eq!(record.boundary, ExtensionBoundary::LocalDevelopment);
        let _ = fs::remove_dir_all(storage_dir);
    }

    #[test]
    fn durable_extension_registry_persists_removal() {
        let storage_dir = durable_storage_dir("extension-restart-remove");
        let service = SecretaryService::new_durable(storage_dir.clone()).expect("durable service");

        service
            .install_extension(InstallExtensionRequest {
                manifest: extension_manifest(
                    "com.example.removed.extension",
                    "1.0.0",
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                    "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                ),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("install should succeed");

        let removed = service
            .remove_extension(RemoveExtensionRequest {
                extension_id: "com.example.removed.extension".to_string(),
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("remove should persist");
        assert_eq!(removed.record.status, ExtensionInstallStatus::Disabled);

        drop(service);

        let service = SecretaryService::new_durable(storage_dir.clone()).expect("durable reload");
        let extensions = service
            .list_extensions(ListExtensionsRequest {
                include_blocked: true,
            })
            .expect("extensions should be queryable after restart");
        assert_eq!(extensions.extensions.len(), 0);

        let err = service
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.removed.extension".to_string(),
            })
            .expect_err("removed extension should not be installed");
        assert!(matches!(
            err,
            ServiceError::ExtensionRegistry(RegistryError::NotInstalled { .. })
        ));

        let _ = fs::remove_dir_all(storage_dir);
    }

    #[test]
    fn durable_extension_registry_persists_blocklist_state() {
        let storage_dir = durable_storage_dir("extension-restart-blocklist");
        let service = SecretaryService::new_durable(storage_dir.clone()).expect("durable service");

        service
            .install_extension(InstallExtensionRequest {
                manifest: extension_manifest(
                    "com.example.blocked.extension",
                    "1.0.0",
                    "sha256:1111111111111111111111111111111111111111111111111111111111111112",
                    "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaab",
                ),
                approve_local_unsigned: false,
                allow_local_process_runtime: false,
                approve_source_change: false,
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("install should succeed");

        let block = service
            .apply_blocklist(ApplyBlocklistRequest {
                entry: BlocklistEntry {
                    key: BlockKey::ExtensionId("com.example.blocked.extension".to_string()),
                    reason: BlockReason::UserBlocked,
                    note: Some("blocked for restart".to_string()),
                },
                requester: None,
                request_source: None,
                reason: None,
            })
            .expect("blocklist should persist");
        assert_eq!(
            block.entry.key,
            BlockKey::ExtensionId("com.example.blocked.extension".to_string())
        );

        let before = service
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.blocked.extension".to_string(),
            })
            .expect("status should show block before restart");
        assert_eq!(before.block.unwrap().key, block.entry.key);
        assert_eq!(
            before.record.unwrap().status,
            ExtensionInstallStatus::Blocked
        );

        drop(service);

        let service = SecretaryService::new_durable(storage_dir.clone()).expect("durable reload");
        let after = service
            .extension_status(ExtensionStatusRequest {
                extension_id: "com.example.blocked.extension".to_string(),
            })
            .expect("status should restore blocked state after restart");
        let block = after
            .block
            .expect("blocked status should include block marker");
        assert_eq!(
            block.key,
            BlockKey::ExtensionId("com.example.blocked.extension".to_string())
        );
        assert_eq!(
            after.record.unwrap().status,
            ExtensionInstallStatus::Blocked
        );

        let blocklist = service
            .list_blocklist(ListBlocklistRequest {})
            .expect("blocklist should restore");
        assert_eq!(blocklist.entries.len(), 1);
        assert_eq!(
            blocklist.entries[0].key,
            BlockKey::ExtensionId("com.example.blocked.extension".to_string())
        );

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
            .contains(&"package_inspect.v1".to_string()));
        assert!(metadata
            .capabilities
            .contains(&"package_trust_index.v1".to_string()));
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
            vec![
                "fs.delete",
                "fs.diff",
                "fs.list",
                "fs.read",
                "fs.search",
                "fs.stat",
                "secretary.echo"
            ]
        );
        let delete = repertoire
            .entries
            .iter()
            .find(|entry| entry.tool_id == "fs.delete")
            .expect("fs.delete repertoire entry");
        assert_eq!(delete.name, "Filesystem Delete");
        assert_eq!(delete.risk_tier, "R2");
        assert_eq!(delete.provider_kind, "builtin");
        assert_eq!(delete.provider_id, "atelia-secretary");
        assert_eq!(delete.default_result_format, "toon");
        assert!(!delete.cancellable);
        assert_eq!(
            delete.supported_result_formats,
            vec!["toon".to_string(), "json".to_string()]
        );
        assert_eq!(delete.timeout_ms, 0);
        let list = repertoire
            .entries
            .iter()
            .find(|entry| entry.tool_id == "fs.list")
            .expect("fs.list repertoire entry");
        assert_eq!(list.name, "Filesystem List");
        assert_eq!(list.risk_tier, "R1");
        assert_eq!(list.provider_kind, "builtin");
        assert_eq!(list.provider_id, "atelia-secretary");
        assert_eq!(list.default_result_format, "toon");
        assert!(!list.cancellable);
        assert_eq!(list.timeout_ms, 0);
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
        let diff = repertoire
            .entries
            .iter()
            .find(|entry| entry.tool_id == "fs.diff")
            .expect("fs.diff repertoire entry");
        assert_eq!(diff.name, "Filesystem Diff");
        assert_eq!(diff.risk_tier, "R1");
        assert_eq!(diff.provider_kind, "builtin");
        assert_eq!(diff.provider_id, "atelia-secretary");
        assert_eq!(diff.default_result_format, "toon");
        assert!(!diff.cancellable);
        assert_eq!(diff.timeout_ms, 0);
        let search = repertoire
            .entries
            .iter()
            .find(|entry| entry.tool_id == "fs.search")
            .expect("fs.search repertoire entry");
        assert_eq!(search.name, "Filesystem Search");
        assert_eq!(search.risk_tier, "R1");
        assert_eq!(search.provider_kind, "builtin");
        assert_eq!(search.provider_id, "atelia-secretary");
        assert_eq!(search.default_result_format, "toon");
        assert!(!search.cancellable);
        assert_eq!(search.timeout_ms, 0);
        let stat = repertoire
            .entries
            .iter()
            .find(|entry| entry.tool_id == "fs.stat")
            .expect("fs.stat repertoire entry");
        assert_eq!(stat.name, "Filesystem Stat");
        assert_eq!(stat.risk_tier, "R1");
        assert_eq!(stat.provider_kind, "builtin");
        assert_eq!(stat.provider_id, "atelia-secretary");
        assert_eq!(stat.default_result_format, "toon");
        assert!(!stat.cancellable);
        assert_eq!(stat.timeout_ms, 0);

        assert!(repertoire.entries.iter().all(|entry| {
            matches!(
                entry.tool_id.as_str(),
                "fs.delete"
                    | "fs.diff"
                    | "fs.list"
                    | "fs.read"
                    | "fs.search"
                    | "fs.stat"
                    | "secretary.echo"
            )
        }));
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
    fn watch_events_live_rejects_zero_page_size() {
        let svc = ready_service();
        match svc.watch_events_live(EventQuery {
            page_size: Some(0),
            ..EventQuery::default()
        }) {
            Err(ServiceError::InvalidArgument { .. }) => {}
            _ => panic!("expected invalid page_size to be rejected"),
        }
    }

    #[tokio::test]
    async fn watch_events_live_drains_retained_events_before_streaming() {
        let svc = ready_service();
        let root = test_repo_dir("watch-events-live-retained-pages");

        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "watch-live-retained-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let first_event = svc
            .lifecycle
            .runtime()
            .store()
            .append_job_event(test_job_event(&repository.id, 1))
            .expect("first retained event should append");
        let second_event = svc
            .lifecycle
            .runtime()
            .store()
            .append_job_event(test_job_event(&repository.id, 2))
            .expect("second retained event should append");

        let mut live = svc
            .watch_events_live(EventQuery {
                repository_id: Some(repository.id.clone()),
                cursor: EventCursor::Beginning,
                subject_ids: Vec::new(),
                job_ids: Vec::new(),
                min_severity: None,
                page_size: Some(1),
                page_token: None,
            })
            .expect("live watch should succeed");

        assert_eq!(live.events.len(), 2);
        assert_eq!(
            live.events
                .iter()
                .map(|event| event.sequence_number)
                .collect::<Vec<_>>(),
            vec![first_event.sequence_number, second_event.sequence_number]
        );
        assert_eq!(
            live.events[0]
                .refs
                .repository_id
                .as_ref()
                .map(|id| id.as_str()),
            Some(repository.id.as_str())
        );
        assert_eq!(
            live.events[1]
                .refs
                .repository_id
                .as_ref()
                .map(|id| id.as_str()),
            Some(repository.id.as_str())
        );
        assert_eq!(live.replay_max_sequence, Some(second_event.sequence_number));
        assert_eq!(live.resolved_cursor_sequence, None);

        let live_event = svc
            .lifecycle
            .runtime()
            .store()
            .append_job_event(test_job_event(&repository.id, 3))
            .expect("post-snapshot event should append");
        let received = live
            .receiver
            .recv()
            .await
            .expect("live receiver should get post-snapshot event")
            .expect("post-snapshot event should not be a terminal error");
        assert_eq!(received.sequence_number, live_event.sequence_number);
        assert!(
            received.sequence_number > live.replay_max_sequence.expect("replay boundary"),
            "live delivery should advance beyond replay boundary"
        );

        let _ = fs::remove_dir_all(root);
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
                goal: Some("summarize current repository status".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some("summarize repository a".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some("summarize repository b".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some(long_goal.clone()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some("render tool output".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some("summarize status".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
            goal: Some("first".to_string()),
            resource_scope: None,
            requested_capabilities: Vec::new(),
            tool_args: None,
            idempotency_key: None,
        })
        .expect("submit should succeed");
        svc.submit_job(SubmitJobRequest {
            requester: actor(),
            repository_id: repository_id.clone(),
            kind: JobKind::Read,
            goal: Some("second".to_string()),
            resource_scope: None,
            requested_capabilities: Vec::new(),
            tool_args: None,
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
                goal: Some("summarize the runtime output".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
    fn submit_job_accepts_empty_goal_and_records_none() {
        let svc = ready_service();
        let root = test_repo_dir("empty-goal-submit");
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
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some(" ".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
                idempotency_key: None,
            })
            .expect("empty goal should be accepted");

        assert_eq!(receipt.job.goal, None);
        let _ = fs::remove_dir_all(root);
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
                goal: Some("summarize".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some("summarize".to_string()),
                resource_scope: None,
                requested_capabilities: vec!["policy.check".to_string()],
                tool_args: None,
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
                goal: Some("summarize".to_string()),
                resource_scope: None,
                requested_capabilities: vec!["capability.discovery".to_string()],
                tool_args: None,
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
                goal: Some("read repository notes".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "README.md".to_string(),
                }),
                requested_capabilities: vec!["filesystem.read".to_string()],
                tool_args: None,
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
    fn submit_job_dispatches_filesystem_list_tool() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-list-dispatch");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "list-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::write(root.join("notes").join("a.txt"), "alpha\n").unwrap();
        fs::write(root.join("notes").join("b.txt"), "beta\n").unwrap();

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("list directory".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "notes".to_string(),
                }),
                requested_capabilities: vec!["filesystem.list".to_string()],
                tool_args: None,
                idempotency_key: None,
            })
            .expect("list dispatch should succeed");

        assert_eq!(
            receipt
                .tool_invocation
                .as_ref()
                .expect("tool invocation should exist")
                .tool_id,
            "fs.list"
        );
        assert_eq!(
            receipt.policy_decision.requested_capability,
            "filesystem.list"
        );
        let tool_result = receipt.tool_result.expect("tool result should exist");
        assert_eq!(
            tool_result.schema_ref.as_deref(),
            Some("tool_result.fs.list.v1")
        );
        assert!(tool_result
            .fields
            .iter()
            .any(|field| field.key == "entries"
                && matches!(&field.value, atelia_core::StructuredValue::StringList(values) if values.contains(&"a.txt".to_string()) && values.contains(&"b.txt".to_string()))));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_allows_filesystem_list_repository_root_scope() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-list-root-scope");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "list-root-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::write(root.join("notes").join("a.txt"), "alpha\n").unwrap();

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("list repository root".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "repository".to_string(),
                    value: ".".to_string(),
                }),
                requested_capabilities: vec!["filesystem.list".to_string()],
                tool_args: None,
                idempotency_key: None,
            })
            .expect("list root scope should dispatch");

        assert_eq!(
            receipt
                .tool_invocation
                .as_ref()
                .expect("tool invocation should exist")
                .tool_id,
            "fs.list"
        );
        let tool_result = receipt.tool_result.expect("tool result should exist");
        assert_eq!(
            tool_result.schema_ref.as_deref(),
            Some("tool_result.fs.list.v1")
        );
        assert!(tool_result
            .fields
            .iter()
            .any(|field| field.key == "entries"
                && matches!(&field.value, atelia_core::StructuredValue::StringList(values) if values.contains(&"notes".to_string()))));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_list_outside_allowed_scope() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-list-allowed-scope");
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::write(root.join("README.md"), "root\n").unwrap();
        fs::write(root.join("notes").join("note.txt"), "alpha\n").unwrap();
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "list-scope-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: Some(PathScope {
                    root_path: root.to_string_lossy().to_string(),
                    allowed_paths: vec!["notes".to_string()],
                }),
                requester: None,
            })
            .expect("register should succeed");

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("list outside allowed path".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "README.md".to_string(),
                }),
                requested_capabilities: vec!["filesystem.list".to_string()],
                tool_args: None,
                idempotency_key: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_dispatches_filesystem_search_tool() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-search-dispatch");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "search-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("note.txt"), "alpha\nneedle\n").unwrap();
        fs::write(root.join("other.txt"), "beta\nNEEDLE\n").unwrap();

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("search repository notes".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "note.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.search".to_string()],
                tool_args: Some(SubmitJobToolArgs {
                    pattern: Some("needle".to_string()),
                    max: None,
                    comparison_path: None,
                    max_bytes: None,
                    max_chars: None,
                }),
                idempotency_key: None,
            })
            .expect("search dispatch should succeed");

        assert_eq!(
            receipt
                .tool_invocation
                .as_ref()
                .expect("tool invocation should exist")
                .tool_id,
            "fs.search"
        );
        assert_eq!(
            receipt.policy_decision.requested_capability,
            "filesystem.search"
        );
        let tool_result = receipt.tool_result.expect("tool result should exist");
        assert_eq!(
            tool_result.schema_ref.as_deref(),
            Some("tool_result.fs.search.v1")
        );
        assert!(tool_result.fields.iter().any(|field| field.key == "matches"
            && matches!(&field.value, atelia_core::StructuredValue::StringList(_))));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_search_outside_allowed_scope() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-search-allowed-scope");
        fs::create_dir_all(root.join("notes")).unwrap();
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "search-scope-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: Some(PathScope {
                    root_path: root.to_string_lossy().to_string(),
                    allowed_paths: vec!["notes".to_string()],
                }),
                requester: None,
            })
            .expect("register should succeed");
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::write(root.join("notes").join("note.txt"), "alpha\nneedle\n").unwrap();
        fs::write(root.join("outside.txt"), "outside\n").unwrap();

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("search disallowed path".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "outside.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.search".to_string()],
                tool_args: Some(SubmitJobToolArgs {
                    pattern: Some("needle".to_string()),
                    max: None,
                    comparison_path: None,
                    max_bytes: None,
                    max_chars: None,
                }),
                idempotency_key: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn submit_job_search_does_not_follow_scoped_symlink_to_outside_path() {
        use std::os::unix::fs::symlink;

        let svc = ready_service();
        let root = test_repo_dir("filesystem-search-scoped-symlink");
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("private.txt"), "outside secret\n").unwrap();
        fs::write(root.join("docs").join("notes.txt"), "inside notes\n").unwrap();
        symlink(root.join("private.txt"), root.join("docs").join("link")).unwrap();

        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "search-symlink-scope-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: Some(PathScope {
                    root_path: root.to_string_lossy().to_string(),
                    allowed_paths: vec!["docs".to_string()],
                }),
                requester: None,
            })
            .expect("register should succeed");

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id.clone(),
                kind: JobKind::Read,
                goal: Some("search scoped with symlink".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "repository".to_string(),
                    value: "docs".to_string(),
                }),
                requested_capabilities: vec!["filesystem.search".to_string()],
                tool_args: Some(SubmitJobToolArgs {
                    pattern: Some("outside".to_string()),
                    max: None,
                    comparison_path: None,
                    max_bytes: None,
                    max_chars: None,
                }),
                idempotency_key: None,
            })
            .expect("scoped search should succeed");

        let tool_result = receipt
            .tool_result
            .as_ref()
            .expect("tool result should exist");
        let match_count = tool_result
            .fields
            .iter()
            .find(|field| field.key == "match_count")
            .and_then(|field| match &field.value {
                atelia_core::StructuredValue::Integer(value) => Some(*value),
                _ => None,
            })
            .unwrap_or(0);
        assert_eq!(match_count, 0);
        assert!(tool_result.fields.iter().all(|field| {
            !matches!(&field.value, atelia_core::StructuredValue::StringList(matches) if matches.iter().any(|entry| entry.contains("private.txt")))
        }));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_dispatches_filesystem_diff_tool() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-diff-dispatch");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "diff-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("left.txt"), "alpha\n").unwrap();
        fs::write(root.join("right.txt"), "alpha\nbeta\n").unwrap();

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("diff two files".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "left.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.diff".to_string()],
                tool_args: Some(SubmitJobToolArgs {
                    pattern: None,
                    max: None,
                    comparison_path: Some("right.txt".to_string()),
                    max_bytes: Some(128),
                    max_chars: Some(128),
                }),
                idempotency_key: None,
            })
            .expect("diff dispatch should succeed");

        assert_eq!(
            receipt
                .tool_invocation
                .as_ref()
                .expect("tool invocation should exist")
                .tool_id,
            "fs.diff"
        );
        assert_eq!(
            receipt.policy_decision.requested_capability,
            "filesystem.diff"
        );
        let tool_result = receipt.tool_result.expect("tool result should exist");
        assert_eq!(
            tool_result.schema_ref.as_deref(),
            Some("tool_result.fs.diff.v1")
        );
        assert!(tool_result.fields.iter().any(|field| {
            field.key == "diff" && matches!(&field.value, atelia_core::StructuredValue::String(_))
        }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_dispatches_filesystem_stat_tool() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-stat-dispatch");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "stat-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("note.txt"), "hello\n").unwrap();

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("stat file".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "note.txt".to_string(),
                }),
                requested_capabilities: vec!["fs.stat".to_string()],
                tool_args: None,
                idempotency_key: None,
            })
            .expect("stat dispatch should succeed");

        assert_eq!(
            receipt
                .tool_invocation
                .as_ref()
                .expect("tool invocation should exist")
                .tool_id,
            "fs.stat"
        );
        let tool_result = receipt.tool_result.expect("tool result should exist");
        assert_eq!(
            tool_result.schema_ref.as_deref(),
            Some("tool_result.fs.stat.v1")
        );
        assert!(tool_result
            .fields
            .iter()
            .any(|field| field.key == "file_type"
                && matches!(&field.value, atelia_core::StructuredValue::String(value) if value == "file")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_allows_filesystem_stat_repository_root_scope() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-stat-root-scope");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "stat-root-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("note.txt"), "hello\n").unwrap();

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("stat repository root".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "repository".to_string(),
                    value: ".".to_string(),
                }),
                requested_capabilities: vec!["filesystem.stat".to_string()],
                tool_args: None,
                idempotency_key: None,
            })
            .expect("stat root scope should dispatch");

        assert_eq!(
            receipt
                .tool_invocation
                .as_ref()
                .expect("tool invocation should exist")
                .tool_id,
            "fs.stat"
        );
        let tool_result = receipt.tool_result.expect("tool result should exist");
        assert_eq!(
            tool_result.schema_ref.as_deref(),
            Some("tool_result.fs.stat.v1")
        );
        assert!(tool_result
            .fields
            .iter()
            .any(|field| field.key == "file_type"
                && matches!(&field.value, atelia_core::StructuredValue::String(value) if value == "directory")));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_stat_outside_allowed_scope() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-stat-allowed-scope");
        fs::write(root.join("outside.txt"), "outside\n").unwrap();
        fs::create_dir_all(root.join("notes")).unwrap();
        fs::write(root.join("notes").join("note.txt"), "hello\n").unwrap();
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "stat-scope-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: Some(PathScope {
                    root_path: root.to_string_lossy().to_string(),
                    allowed_paths: vec!["notes".to_string()],
                }),
                requester: None,
            })
            .expect("register should succeed");

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("stat outside allowed path".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "outside.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.stat".to_string()],
                tool_args: None,
                idempotency_key: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_dispatches_filesystem_delete_tool() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-delete-dispatch");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "delete-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("to-delete.txt"), "temporary\n").unwrap();

        let receipt = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Mutate,
                goal: Some("delete file".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "to-delete.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.delete".to_string()],
                tool_args: None,
                idempotency_key: None,
            })
            .expect("delete dispatch should succeed");

        assert_eq!(
            receipt
                .tool_invocation
                .as_ref()
                .expect("tool invocation should exist")
                .tool_id,
            "fs.delete"
        );
        assert_eq!(
            receipt.policy_decision.requested_capability,
            "filesystem.delete"
        );
        assert!(
            !root.join("to-delete.txt").exists(),
            "file should be removed by delete tool"
        );
        let tool_result = receipt.tool_result.expect("tool result should exist");
        assert_eq!(
            tool_result.schema_ref.as_deref(),
            Some("tool_result.fs.delete.v1")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_delete_without_mutate_kind() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-delete-mutate-kind");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "delete-kind-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("protected.txt"), "contents\n").unwrap();

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("delete with read kind".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "protected.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.delete".to_string()],
                tool_args: None,
                idempotency_key: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        assert!(
            root.join("protected.txt").exists(),
            "read-typed delete should not remove the file"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_delete_with_read_only_scope() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-delete-read-only");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "delete-read-only-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("protected.txt"), "contents\n").unwrap();

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Mutate,
                goal: Some("delete with read-only scope".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "read_only".to_string(),
                    value: "protected.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.delete".to_string()],
                tool_args: None,
                idempotency_key: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        assert!(
            root.join("protected.txt").exists(),
            "read-only scoped delete should not remove the file"
        );
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
                goal: Some("read scoped notes".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "explicit_paths".to_string(),
                    value: "docs/guide.md".to_string(),
                }),
                requested_capabilities: vec!["filesystem.read".to_string()],
                tool_args: None,
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
                goal: Some("read root notes".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "explicit_paths".to_string(),
                    value: "README.md".to_string(),
                }),
                requested_capabilities: vec!["filesystem.read".to_string()],
                tool_args: None,
                idempotency_key: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_delete_outside_allowed_scope() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-delete-allowed-scope");
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(root.join("README.md"), "root\n").unwrap();
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "delete-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: Some(PathScope {
                    root_path: root.to_string_lossy().to_string(),
                    allowed_paths: vec!["docs".to_string()],
                }),
                requester: None,
            })
            .expect("register should succeed");

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Mutate,
                goal: Some("delete root notes".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "README.md".to_string(),
                }),
                requested_capabilities: vec!["filesystem.delete".to_string()],
                tool_args: None,
                idempotency_key: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        assert!(
            root.join("README.md").exists(),
            "root file should still exist after rejection"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_search_without_pattern() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-search-missing-arg");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "search-arg-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("note.txt"), "alpha\n").unwrap();

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("search without pattern".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "note.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.search".to_string()],
                tool_args: Some(SubmitJobToolArgs {
                    pattern: None,
                    max: None,
                    comparison_path: None,
                    max_bytes: None,
                    max_chars: None,
                }),
                idempotency_key: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_search_with_unexpected_args() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-search-unexpected-args");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "search-unexpected-arg-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("note.txt"), "alpha\nneedle\nneedle\n").unwrap();

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("search with comparison_path".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "note.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.search".to_string()],
                tool_args: Some(SubmitJobToolArgs {
                    pattern: Some("needle".to_string()),
                    max: Some(1),
                    comparison_path: Some("note.txt".to_string()),
                    max_bytes: None,
                    max_chars: None,
                }),
                idempotency_key: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_search_with_unexpected_bounds() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-search-unexpected-bounds");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "search-unexpected-bounds-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("note.txt"), "alpha\nneedle\n").unwrap();

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("search with bounds".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "note.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.search".to_string()],
                tool_args: Some(SubmitJobToolArgs {
                    pattern: Some("needle".to_string()),
                    max: None,
                    comparison_path: None,
                    max_bytes: Some(1),
                    max_chars: Some(64),
                }),
                idempotency_key: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_diff_without_comparison_path() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-diff-missing-arg");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "diff-arg-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("left.txt"), "alpha\n").unwrap();

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("diff without path".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "left.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.diff".to_string()],
                tool_args: Some(SubmitJobToolArgs {
                    pattern: None,
                    max: None,
                    comparison_path: None,
                    max_bytes: None,
                    max_chars: None,
                }),
                idempotency_key: None,
            })
            .unwrap_err();
        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_diff_with_unexpected_args() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-diff-unexpected-args");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "diff-unexpected-arg-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("left.txt"), "alpha\n").unwrap();
        fs::write(root.join("right.txt"), "alpha\n").unwrap();

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("diff with pattern".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "left.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.diff".to_string()],
                tool_args: Some(SubmitJobToolArgs {
                    pattern: Some("alpha".to_string()),
                    max: Some(1),
                    comparison_path: Some("right.txt".to_string()),
                    max_bytes: None,
                    max_chars: None,
                }),
                idempotency_key: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_filesystem_diff_comparison_path_outside_allowed_scope() {
        let svc = ready_service();
        let root = test_repo_dir("filesystem-diff-allowed-scope");
        fs::create_dir_all(root.join("docs")).unwrap();
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "diff-scope-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: Some(PathScope {
                    root_path: root.to_string_lossy().to_string(),
                    allowed_paths: vec!["docs".to_string()],
                }),
                requester: None,
            })
            .expect("register should succeed");
        fs::write(root.join("docs").join("left.txt"), "alpha\n").unwrap();
        fs::write(root.join("outside.txt"), "outside\n").unwrap();

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("diff scoped".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "path".to_string(),
                    value: "docs/left.txt".to_string(),
                }),
                requested_capabilities: vec!["filesystem.diff".to_string()],
                tool_args: Some(SubmitJobToolArgs {
                    pattern: None,
                    max: None,
                    comparison_path: Some("outside.txt".to_string()),
                    max_bytes: None,
                    max_chars: None,
                }),
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
                goal: Some("read repository notes".to_string()),
                resource_scope: None,
                requested_capabilities: vec!["filesystem.read".to_string()],
                tool_args: None,
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
                    goal: Some("read repository root".to_string()),
                    resource_scope: Some(ResourceScope {
                        kind: "repository".to_string(),
                        value: value.to_string(),
                    }),
                    requested_capabilities: vec!["filesystem.read".to_string()],
                    tool_args: None,
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
            goal: Some("summarize".to_string()),
            resource_scope: None,
            requested_capabilities: Vec::new(),
            tool_args: None,
            idempotency_key: Some("request-123".to_string()),
        };
        let first_normalized_goal = normalize_submit_job_goal(first_request.goal.clone());
        let first_signature = submit_job_request_signature(
            &first_request,
            first_normalized_goal.as_deref(),
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
            goal: Some("summarize".to_string()),
            resource_scope: None,
            requested_capabilities: vec!["capability.discovery".to_string()],
            tool_args: None,
            idempotency_key: Some("request-123".to_string()),
        };
        let second_normalized_goal = normalize_submit_job_goal(second_request.goal.clone());
        let second_signature = submit_job_request_signature(
            &second_request,
            second_normalized_goal.as_deref(),
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
                goal: Some("  summarize  ".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("first submit should succeed");

        assert_eq!(first.job.goal, Some("summarize".to_string()));

        let second = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("summarize".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("trimmed goal should replay the same job");

        assert_eq!(second.job.id, first.job.id);
        assert_eq!(second.job.goal, Some("summarize".to_string()));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_treats_blank_goal_like_absent_goal_for_idempotency() {
        let svc = ready_service();
        let root = test_repo_dir("blank-goal-idempotency");
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
            goal: Some("   ".to_string()),
            resource_scope: None,
            requested_capabilities: Vec::new(),
            tool_args: None,
            idempotency_key: Some("request-blank".to_string()),
        };
        let first_normalized_goal = normalize_submit_job_goal(first_request.goal.clone());
        let first_signature = submit_job_request_signature(
            &first_request,
            first_normalized_goal.as_deref(),
            &normalize_requested_capabilities(&first_request.requested_capabilities)
                .expect("first capability normalization should succeed"),
        );
        let first = svc
            .submit_job(first_request)
            .expect("blank goal submit should succeed");

        let second_request = SubmitJobRequest {
            requester: actor(),
            repository_id: repository.id,
            kind: JobKind::Read,
            goal: None,
            resource_scope: None,
            requested_capabilities: Vec::new(),
            tool_args: None,
            idempotency_key: Some("request-blank".to_string()),
        };
        let second_normalized_goal = normalize_submit_job_goal(second_request.goal.clone());
        let second_signature = submit_job_request_signature(
            &second_request,
            second_normalized_goal.as_deref(),
            &normalize_requested_capabilities(&second_request.requested_capabilities)
                .expect("second capability normalization should succeed"),
        );
        let second = svc
            .submit_job(second_request)
            .expect("absent goal should replay the blank-goal submission");

        assert_eq!(first.job.id, second.job.id);
        assert_eq!(first.job.goal, None);
        assert_eq!(second.job.goal, None);
        assert_eq!(first_signature, second_signature);
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
                goal: Some("inspect".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "repository".to_string(),
                    value: "binary.bin".to_string(),
                }),
                requested_capabilities: vec!["filesystem.read".to_string()],
                tool_args: None,
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("first submit should return a failed receipt");

        assert_eq!(first.job.status, JobStatus::Failed);

        let second = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("inspect".to_string()),
                resource_scope: Some(ResourceScope {
                    kind: "repository".to_string(),
                    value: "binary.bin".to_string(),
                }),
                requested_capabilities: vec!["filesystem.read".to_string()],
                tool_args: None,
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
            goal: Some("summarize;resource_scope=repository:.".to_string()),
            resource_scope: Some(ResourceScope {
                kind: "repository".to_string(),
                value: "branch=main;capabilities=capability.discovery".to_string(),
            }),
            requested_capabilities: vec!["capability.discovery".to_string()],
            tool_args: None,
            idempotency_key: None,
        };
        let request_two = SubmitJobRequest {
            requester: actor(),
            repository_id: repository.id,
            kind: JobKind::Read,
            goal: Some("summarize".to_string()),
            resource_scope: Some(ResourceScope {
                kind: "repository".to_string(),
                value: "branch=main".to_string(),
            }),
            requested_capabilities: vec!["capability.discovery".to_string()],
            tool_args: None,
            idempotency_key: None,
        };

        let request_one_normalized_goal = normalize_submit_job_goal(request_one.goal.clone());
        let request_two_normalized_goal = normalize_submit_job_goal(request_two.goal.clone());
        let signature_one = submit_job_request_signature(
            &request_one,
            request_one_normalized_goal.as_deref(),
            &normalized_capabilities,
        );
        let signature_two = submit_job_request_signature(
            &request_two,
            request_two_normalized_goal.as_deref(),
            &normalized_capabilities,
        );

        assert_ne!(signature_one, signature_two);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_request_signature_includes_tool_args() {
        let svc = ready_service();
        let root = test_repo_dir("submit-job-signature-tool-args");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "signature-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
                allowed_scope: None,
                requester: None,
            })
            .expect("register should succeed");

        let base_request = SubmitJobRequest {
            requester: actor(),
            repository_id: repository.id,
            kind: JobKind::Read,
            goal: Some("search".to_string()),
            resource_scope: Some(ResourceScope {
                kind: "path".to_string(),
                value: "note.txt".to_string(),
            }),
            requested_capabilities: vec!["filesystem.search".to_string()],
            tool_args: Some(SubmitJobToolArgs {
                pattern: Some("needle".to_string()),
                max: None,
                comparison_path: None,
                max_bytes: None,
                max_chars: None,
            }),
            idempotency_key: None,
        };
        let normalized = normalize_submit_job_goal(base_request.goal.clone());
        let request_signature_one = submit_job_request_signature(
            &base_request,
            normalized.as_deref(),
            &normalize_requested_capabilities(&base_request.requested_capabilities)
                .expect("capabilities should normalize"),
        );

        let comparison_request = SubmitJobRequest {
            goal: Some("search".to_string()),
            resource_scope: Some(ResourceScope {
                kind: "path".to_string(),
                value: "note.txt".to_string(),
            }),
            tool_args: Some(SubmitJobToolArgs {
                pattern: Some("other".to_string()),
                max: None,
                comparison_path: None,
                max_bytes: None,
                max_chars: None,
            }),
            ..base_request
        };
        let normalized = normalize_submit_job_goal(comparison_request.goal.clone());
        let request_signature_two = submit_job_request_signature(
            &comparison_request,
            normalized.as_deref(),
            &normalize_requested_capabilities(&comparison_request.requested_capabilities)
                .expect("capabilities should normalize"),
        );
        assert_ne!(request_signature_one, request_signature_two);
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
                goal: Some("summarize".to_string()),
                resource_scope: None,
                requested_capabilities: vec!["filesystem.write".to_string()],
                tool_args: None,
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
                goal: Some("summarize".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("first submit should succeed");

        let second = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: Some("summarize".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some("summarize".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
                idempotency_key: Some("request-0".to_string()),
            })
            .expect("first submit should succeed");

        for index in 1..=IDEMPOTENT_SUBMISSION_CACHE_LIMIT {
            let receipt = svc
                .submit_job(SubmitJobRequest {
                    requester: actor(),
                    repository_id: repository.id.clone(),
                    kind: JobKind::Read,
                    goal: Some("summarize".to_string()),
                    resource_scope: None,
                    requested_capabilities: Vec::new(),
                    tool_args: None,
                    idempotency_key: Some(format!("request-{index}")),
                })
                .expect("unique submit should succeed");
            assert_eq!(receipt.job.goal, Some("summarize".to_string()));
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
                goal: Some("summarize".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                    goal: Some("summarize".to_string()),
                    resource_scope: None,
                    requested_capabilities: Vec::new(),
                    tool_args: None,
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
                goal: Some("summarize".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
                idempotency_key: Some("request-123".to_string()),
            })
            .expect("first submit should succeed");

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor_two(),
                repository_id: first.job.repository_id,
                kind: JobKind::Read,
                goal: Some("different summary".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
                goal: Some("summarize".to_string()),
                resource_scope: None,
                requested_capabilities: Vec::new(),
                tool_args: None,
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
            goal: Some("from-first".to_string()),
            resource_scope: None,
            requested_capabilities: Vec::new(),
            tool_args: None,
            idempotency_key: None,
        })
        .expect("first submit should succeed");
        svc.submit_job(SubmitJobRequest {
            requester: actor_two(),
            repository_id: repository_id.clone(),
            kind: JobKind::Read,
            goal: Some("from-second".to_string()),
            resource_scope: None,
            requested_capabilities: Vec::new(),
            tool_args: None,
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
