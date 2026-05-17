use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use crate::rpc;
use anyhow::{anyhow, Context, Result};
use async_stream::stream;
use atelia_core::{
    Actor, JobId, LedgerTimestamp, OutputFormat, OversizeOutputPolicy, ProjectId, RenderOptions,
    RepositoryId, StoreError, ToolOutputDefaults, ToolOutputGranularity, ToolOutputOverrides,
    ToolOutputSettingsChange, ToolOutputSettingsScope, ToolOutputVerbosity,
};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::net::TcpListener;
use tokio::sync::{oneshot, RwLock};

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8080";
const LISTEN_ADDR_ENV: &str = "ATELIA_DAEMON_LISTEN_ADDR";
const UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV: &str = "ATELIA_DAEMON_UNSAFE_ALLOW_NON_LOOPBACK_LISTEN";
const AUTH_DISABLED_ENV: &str = "ATELIA_DAEMON_AUTH_DISABLED";
const AUTH_TOKEN_ENV: &str = "ATELIA_DAEMON_AUTH_TOKEN";
const AUTH_TOKEN_FILE_NAME: &str = "daemon-auth.token";
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024; // 1 MiB
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Shared RPC server state used by the HTTP router.
pub type RpcServerState = Arc<RwLock<rpc::SecretaryRpcServer>>;

/// Local authentication mode for the daemon boundary.
#[derive(Clone, PartialEq, Eq)]
pub enum LocalAuthConfig {
    /// Require a bearer token on the `Authorization` header.
    BearerToken { token: String },
    /// Disable local authentication entirely.
    Disabled,
}

impl std::fmt::Debug for LocalAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BearerToken { .. } => f
                .debug_struct("LocalAuthConfig")
                .field("token", &"<redacted>")
                .finish(),
            Self::Disabled => f.write_str("LocalAuthConfig::Disabled"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    Health,
    SubmitJob,
    GetJob { job_id: String },
    ListJobs,
    ListJobEvents { job_id: String },
    CancelJob { job_id: String },
    ListRepositories,
    RegisterRepository,
    ListRepertoire,
    ListEvents,
    WatchEvents,
    ReplayEvents,
    GetToolOutputDefaults,
    UpdateToolOutputDefaults,
    ListToolOutputSettingsHistory,
    InstallExtension,
    ValidateExtension,
    UpdateExtension,
    ListExtensions,
    ListPackageTrustIndex,
    PackageAuthoringFlow { extension_id: String },
    PackageRemix { extension_id: String },
    PackagePublication { extension_id: String },
    PackageRegistrySubmission { extension_id: String },
    PackageInspect { extension_id: String },
    ExtensionExecution { extension_id: String },
    ExtensionStatus { extension_id: String },
    RollbackExtension { extension_id: String },
    DisableExtension { extension_id: String },
    EnableExtension { extension_id: String },
    RemoveExtension { extension_id: String },
    ApplyBlocklist,
    ListBlocklist,
    ListExtensionRegistryAuditRecords,
    RenderToolOutput,
    ProjectStatus,
    Unsupported,
}

fn route_for_path(path: &str) -> Route {
    if let Some(job_id) = path
        .strip_prefix("/v1/jobs/")
        .and_then(|path| path.strip_suffix("/cancel"))
        .and_then(valid_job_id)
    {
        return Route::CancelJob {
            job_id: job_id.to_string(),
        };
    }
    if let Some(job_id) = path
        .strip_prefix("/v1/jobs/")
        .and_then(|path| path.strip_suffix("/events"))
        .and_then(valid_job_id)
    {
        return Route::ListJobEvents {
            job_id: job_id.to_string(),
        };
    }
    if let Some(job_id) = path.strip_prefix("/v1/jobs/").and_then(valid_job_id) {
        return Route::GetJob {
            job_id: job_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/packages/")
        .and_then(|path| path.strip_suffix("/authoring-flow"))
        .and_then(valid_extension_id)
    {
        return Route::PackageAuthoringFlow {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/packages/")
        .and_then(|path| path.strip_suffix("/remix"))
        .and_then(valid_extension_id)
    {
        return Route::PackageRemix {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/packages/")
        .and_then(|path| path.strip_suffix("/publication"))
        .and_then(valid_extension_id)
    {
        return Route::PackagePublication {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/packages/")
        .and_then(|path| path.strip_suffix("/registry-submission"))
        .and_then(valid_extension_id)
    {
        return Route::PackageRegistrySubmission {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/packages/")
        .and_then(|path| path.strip_suffix("/status"))
        .and_then(valid_extension_id)
    {
        return Route::ExtensionStatus {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/packages/")
        .and_then(|path| path.strip_suffix("/inspect"))
        .and_then(valid_extension_id)
    {
        return Route::PackageInspect {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/packages/")
        .and_then(|path| path.strip_suffix("/rollback"))
        .and_then(valid_extension_id)
    {
        return Route::RollbackExtension {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/packages/")
        .and_then(|path| path.strip_suffix("/disable"))
        .and_then(valid_extension_id)
    {
        return Route::DisableExtension {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/packages/")
        .and_then(|path| path.strip_suffix("/enable"))
        .and_then(valid_extension_id)
    {
        return Route::EnableExtension {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/packages/")
        .and_then(|path| path.strip_suffix("/remove"))
        .and_then(valid_extension_id)
    {
        return Route::RemoveExtension {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/packages/")
        .and_then(|path| path.strip_suffix("/execute"))
        .and_then(valid_extension_id)
    {
        return Route::ExtensionExecution {
            extension_id: extension_id.to_string(),
        };
    }
    match path {
        "/v1/health" => Route::Health,
        "/v1/jobs/submit" => Route::SubmitJob,
        "/v1/jobs/list" => Route::ListJobs,
        "/v1/repositories:list" => Route::ListRepositories,
        "/v1/repositories:register" => Route::RegisterRepository,
        "/v1/repertoire:list" => Route::ListRepertoire,
        "/v1/events/list" => Route::ListEvents,
        "/v1/events/watch" => Route::WatchEvents,
        "/v1/events/replay" => Route::ReplayEvents,
        "/v1/tool-output/settings/get" => Route::GetToolOutputDefaults,
        "/v1/tool-output/settings/update" => Route::UpdateToolOutputDefaults,
        "/v1/tool-output/settings/history:list" => Route::ListToolOutputSettingsHistory,
        "/v1/packages/install" => Route::InstallExtension,
        "/v1/packages/validate" => Route::ValidateExtension,
        "/v1/packages/update" => Route::UpdateExtension,
        "/v1/packages/list" => Route::ListExtensions,
        "/v1/package-trust-index:list" => Route::ListPackageTrustIndex,
        "/v1/packages/blocklist/apply" => Route::ApplyBlocklist,
        "/v1/packages/blocklist/list" => Route::ListBlocklist,
        "/v1/packages/audit:list" => Route::ListExtensionRegistryAuditRecords,
        "/v1/tool-results:render" => Route::RenderToolOutput,
        "/v1/project-status:get" => Route::ProjectStatus,
        _ => Route::Unsupported,
    }
}

fn valid_path_id(id: &str) -> Option<&str> {
    if id.is_empty() || id.contains('/') {
        None
    } else {
        Some(id)
    }
}

fn valid_extension_id(extension_id: &str) -> Option<&str> {
    valid_path_id(extension_id)
}

fn valid_job_id(job_id: &str) -> Option<&str> {
    JobId::try_from_string(job_id.to_string())
        .is_ok()
        .then_some(job_id)
}

#[derive(Debug, Serialize)]
struct ErrorBody {
    code: &'static str,
    reason: String,
    recoverable: bool,
    next_state: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
enum ApiResponse {
    Ok { data: serde_json::Value },
    Error { error: ErrorBody },
}

impl ApiResponse {
    fn ok(data: serde_json::Value) -> Self {
        Self::Ok { data }
    }

    fn error(
        code: &'static str,
        reason: impl Into<String>,
        recoverable: bool,
        next_state: impl Into<String>,
    ) -> Self {
        Self::Error {
            error: ErrorBody {
                code,
                reason: reason.into(),
                recoverable,
                next_state: next_state.into(),
            },
        }
    }
}

/// HTTP payload for requesting a package authoring flow.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageAuthoringFlowRequestPayload {
    package_id: Option<String>,
    include_private_steps: Option<bool>,
}

/// HTTP payload for previewing a package remix flow.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageRemixRequestPayload {
    package_id: Option<String>,
    #[serde(default)]
    source_class: Option<rpc::PackageSourceClass>,
    #[serde(default)]
    source: Option<rpc::PackageGitHubSourceReference>,
}

/// HTTP payload for preparing package publication.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackagePublicationRequestPayload {
    package_id: Option<String>,
    visibility: rpc::PackagePublicationVisibility,
    #[serde(default = "default_true")]
    requires_registry_submission: bool,
    requester: Option<ActorPayload>,
    request_source: Option<String>,
    reason: Option<String>,
}

/// HTTP payload for updating package registry submission state.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageRegistrySubmissionRequestPayload {
    package_id: Option<String>,
    #[serde(default)]
    state: Option<rpc::PackageRegistrySubmissionState>,
    #[serde(default)]
    registry_identity: Option<String>,
    requester: Option<ActorPayload>,
    request_source: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InstallExtensionRequestPayload {
    manifest: atelia_core::ExtensionManifest,
    #[serde(default)]
    approve_local_unsigned: bool,
    #[serde(default)]
    allow_local_process_runtime: bool,
    #[serde(default)]
    approve_source_change: bool,
    requester: Option<ActorPayload>,
    request_source: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateExtensionRequestPayload {
    manifest: atelia_core::ExtensionManifest,
    #[serde(default)]
    approve_local_unsigned: bool,
    #[serde(default)]
    allow_local_process_runtime: bool,
    #[serde(default)]
    approve_source_change: bool,
    requester: Option<ActorPayload>,
    request_source: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApplyBlocklistRequestPayload {
    entry: atelia_core::BlocklistEntry,
    requester: Option<ActorPayload>,
    request_source: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PackageMutationAuditPayload {
    requester: Option<ActorPayload>,
    request_source: Option<String>,
    reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ListPackageAuditRequestPayload {
    limit: Option<usize>,
    offset: Option<usize>,
    cursor: Option<String>,
}

/// HTTP payload for routes that intentionally accept no request fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyRequestPayload {}

/// Return the default registry-submission requirement for publication requests.
fn default_true() -> bool {
    true
}

/// Resolve the canonical package id from the URL path and optional request body.
fn package_id_from_path_and_payload(
    path_extension_id: String,
    payload_package_id: Option<String>,
) -> Result<String, String> {
    match payload_package_id {
        Some(package_id) if package_id == path_extension_id => Ok(package_id),
        Some(package_id) => Err(format!(
            "package_id {package_id} does not match path package id {path_extension_id}"
        )),
        None => Ok(path_extension_id),
    }
}

#[derive(Debug, Deserialize)]
struct ListRepositoriesRequestPayload {
    trust_state: Option<String>,
    page_size: Option<usize>,
    page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListRepertoireRequestPayload {}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ActorPayload {
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

#[derive(Debug, Deserialize)]
struct SubmitJobRequestPayload {
    repository_id: String,
    requester: ActorPayload,
    kind: String,
    #[serde(default)]
    goal: Option<String>,
    path_scope: Option<PathScopePayload>,
    requested_capabilities: Option<Vec<String>>,
    tool_args: Option<SubmitJobToolArgsPayload>,
    idempotency_key: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SubmitJobToolArgsPayload {
    pattern: Option<String>,
    max: Option<u64>,
    comparison_path: Option<String>,
    max_bytes: Option<u64>,
    max_chars: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterRepositoryRequestPayload {
    display_name: String,
    root_path: String,
    allowed_scope: Option<PathScopePayload>,
    requester: Option<ActorPayload>,
}

fn requests_filesystem_path_operation(capabilities: &[String]) -> bool {
    capabilities.iter().any(|capability| {
        let normalized = capability
            .trim()
            .to_ascii_lowercase()
            .replace(['_', '-', ':', '/'], ".");

        matches!(
            normalized.as_str(),
            "filesystem.read"
                | "filesystem.list"
                | "filesystem.stat"
                | "filesystem.delete"
                | "fs.read"
                | "fs.list"
                | "fs.stat"
                | "fs.delete"
                | "filesystem.search"
                | "filesystem.diff"
                | "fs.search"
                | "fs.diff"
        )
    })
}

#[derive(Debug, Deserialize)]
struct ListJobsRequestPayload {
    repository_id: Option<String>,
    status: Option<String>,
    requester: Option<ActorPayload>,
    page_size: Option<usize>,
    page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CancelJobRequestPayload {
    requester: ActorPayload,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct PathScopePayload {
    kind: Option<String>,
    roots: Option<Vec<String>>,
    include_patterns: Option<Vec<String>>,
    exclude_patterns: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct ToolResultRefPayload {
    tool_result_id: String,
    tool_invocation_id: String,
    job_id: String,
    repository_id: String,
    content_type: String,
}

#[derive(Debug, Deserialize)]
struct RenderToolOutputRequestPayload {
    tool_result: ToolResultRefPayload,
    format: String,
}

#[derive(Debug, Deserialize)]
struct ListEventsRequestPayload {
    repository_id: Option<String>,
    cursor: Option<EventCursorPayload>,
    subject_ids: Option<Vec<String>>,
    job_ids: Option<Vec<String>>,
    min_severity: Option<String>,
    page_size: Option<usize>,
    page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ListJobEventsRequestPayload {
    repository_id: Option<String>,
    cursor: Option<EventCursorPayload>,
    min_severity: Option<String>,
    page_size: Option<usize>,
    page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReplayEventsRequestPayload {
    repository_id: String,
    cursor: Option<EventCursorPayload>,
    subject_ids: Option<Vec<String>>,
    min_severity: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
struct GetToolOutputDefaultsRequestPayload {
    scope: ToolOutputSettingsScope,
}

#[derive(Debug, Deserialize)]
struct UpdateToolOutputDefaultsRequestPayload {
    scope: ToolOutputSettingsScope,
    actor: Actor,
    reason: String,
    overrides: ToolOutputOverrides,
}

#[derive(Debug, Deserialize)]
struct ListToolOutputSettingsHistoryRequestPayload {
    scope: Option<ToolOutputSettingsScope>,
    limit: Option<usize>,
    offset: Option<usize>,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum EventCursorPayload {
    Beginning,
    AfterSequence { sequence_number: u64 },
    AfterEventId { event_id: String },
}

#[derive(Debug, Deserialize)]
struct GetProjectStatusRequestPayload {
    repository_id: String,
}

/// Resolve the daemon listen address and whether it came from the environment.
pub fn listen_addr() -> Result<(SocketAddr, bool)> {
    let raw_addr = std::env::var(LISTEN_ADDR_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let from_env = raw_addr.is_some();
    let addr_str = raw_addr.unwrap_or_else(|| DEFAULT_LISTEN_ADDR.to_string());
    let addr = parse_socket_addr(&addr_str)?;
    Ok((addr, from_env))
}

/// Enforce the beta loopback-only listener boundary unless explicitly bypassed.
pub fn validate_listen_addr(listen_addr: &SocketAddr, explicit_addr: bool) -> Result<()> {
    if is_loopback(listen_addr) {
        return Ok(());
    }

    if unsafe_allow_non_loopback_listen() {
        return Ok(());
    }

    if explicit_addr {
        Err(anyhow!(
            "refusing explicit non-loopback listener address {listen_addr}; set {UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV}=1 to allow this during beta"
        ))
    } else {
        Err(anyhow!(
            "refusing default non-loopback listener address {listen_addr}; set {UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV}=1 to allow this during beta"
        ))
    }
}

/// Refuse to combine auth-disabled mode with any non-loopback listener.
pub fn validate_local_auth_binding(
    local_auth: &LocalAuthConfig,
    listen_addr: &SocketAddr,
) -> Result<()> {
    if matches!(local_auth, LocalAuthConfig::Disabled) && !is_loopback(listen_addr) {
        Err(anyhow!(
            "refusing to combine {AUTH_DISABLED_ENV}=1 with non-loopback listener address {listen_addr}; keep auth enabled or bind to loopback only"
        ))
    } else {
        Ok(())
    }
}

/// Return whether the socket address binds only to the local host.
pub fn is_loopback(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// Return whether the unsafe beta escape hatch allows non-loopback binding.
pub fn unsafe_allow_non_loopback_listen() -> bool {
    env_var_is_truthy(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV)
}

/// Resolve the local bearer token boundary used by the beta daemon.
pub fn resolve_local_auth(storage_dir: &std::path::Path) -> Result<LocalAuthConfig> {
    let auth_disabled = env_var_is_truthy(AUTH_DISABLED_ENV);
    let auth_token = std::env::var_os(AUTH_TOKEN_ENV);

    if auth_disabled && auth_token.is_some() {
        return Err(anyhow!(
            "{AUTH_DISABLED_ENV} and {AUTH_TOKEN_ENV} are mutually exclusive"
        ));
    }

    if auth_disabled {
        return Ok(LocalAuthConfig::Disabled);
    }

    if let Some(raw_token) = auth_token {
        let token = raw_token
            .as_os_str()
            .to_str()
            .ok_or_else(|| anyhow!("{AUTH_TOKEN_ENV} must contain valid UTF-8"))?
            .to_string();
        if token.is_empty() {
            return Err(anyhow!("{AUTH_TOKEN_ENV} must not be empty"));
        }
        if token.chars().next().is_some_and(|ch| ch.is_whitespace())
            || token.chars().last().is_some_and(|ch| ch.is_whitespace())
        {
            return Err(anyhow!(
                "{AUTH_TOKEN_ENV} must not have leading or trailing whitespace"
            ));
        }
        if token.chars().any(|ch| ch.is_whitespace()) {
            return Err(anyhow!(
                "{AUTH_TOKEN_ENV} must not contain internal whitespace"
            ));
        }
        if !token
            .as_bytes()
            .iter()
            .all(|byte| (0x20..=0x7e).contains(byte))
        {
            return Err(anyhow!(
                "{AUTH_TOKEN_ENV} must contain only visible ASCII characters"
            ));
        }
        if !local_auth_token_is_strong(&token) {
            return Err(anyhow!(
                "{AUTH_TOKEN_ENV} must be either exactly 64 hexadecimal characters or at least 43 base64url characters using only ASCII alphanumeric, '-' or '_'"
            ));
        }

        return Ok(LocalAuthConfig::BearerToken { token });
    }

    let token = load_or_create_session_token(storage_dir)?;
    Ok(LocalAuthConfig::BearerToken { token })
}

fn local_auth_token_path(storage_dir: &std::path::Path) -> std::path::PathBuf {
    storage_dir.join(AUTH_TOKEN_FILE_NAME)
}

/// Return whether an operator-provided token meets the beta strength floor.
fn local_auth_token_is_strong(token: &str) -> bool {
    let bytes = token.as_bytes();
    (bytes.len() == 64 && bytes.iter().all(|byte| byte.is_ascii_hexdigit()))
        || (bytes.len() >= 43
            && bytes
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')))
}

/// Load an existing generated session token or create one in the storage dir.
fn load_or_create_session_token(storage_dir: &std::path::Path) -> Result<String> {
    create_auth_storage_dir(storage_dir)?;

    let token_path = local_auth_token_path(storage_dir);
    match std::fs::symlink_metadata(&token_path) {
        Ok(_) => read_and_validate_session_token_with_retry(&token_path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let token = generate_session_token()?;
            write_or_reuse_session_token(&token_path, token)
        }
        Err(error) => {
            Err(error).with_context(|| format!("failed to read session token file {token_path:?}"))
        }
    }
}

#[cfg(unix)]
fn create_auth_storage_dir(storage_dir: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};

    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(storage_dir)
        .with_context(|| format!("failed to create auth storage dir {storage_dir:?}"))?;

    let metadata = std::fs::metadata(storage_dir)
        .with_context(|| format!("failed to inspect auth storage dir {storage_dir:?}"))?;
    let current_euid = unsafe { libc::geteuid() };
    if metadata.uid() != current_euid {
        return Err(anyhow!(
            "auth storage dir {storage_dir:?} must be owned by the current user"
        ));
    }

    std::fs::set_permissions(storage_dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to set restrictive permissions on {storage_dir:?}"))
}

#[cfg(not(unix))]
fn create_auth_storage_dir(storage_dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(storage_dir)
        .with_context(|| format!("failed to create auth storage dir {storage_dir:?}"))
}

/// Read a persisted session token after validating file safety properties.
fn read_and_validate_session_token(
    token_path: &std::path::Path,
    storage_dir: &std::path::Path,
) -> Result<String> {
    let mut token_file = open_existing_session_token_file(token_path)?;
    validate_existing_session_token_file(&token_file, storage_dir, token_path)?;

    use std::io::Read;

    let mut token = String::new();
    token_file
        .read_to_string(&mut token)
        .with_context(|| format!("failed to read session token file {token_path:?}"))?;
    validate_and_restrict_session_token(&token_file, token_path, token)
}

/// Validate that an existing token path is a safe regular file owned by this user.
fn validate_existing_session_token_file(
    token_file: &std::fs::File,
    storage_dir: &std::path::Path,
    token_path: &std::path::Path,
) -> Result<()> {
    let metadata = token_file
        .metadata()
        .with_context(|| format!("failed to inspect session token file {token_path:?}"))?;
    if !metadata.file_type().is_file() {
        return Err(anyhow!(
            "session token file {token_path:?} must be a regular file"
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let current_euid = unsafe { libc::geteuid() };
        let storage_owner = std::fs::metadata(storage_dir)
            .with_context(|| format!("failed to inspect auth storage dir {storage_dir:?}"))?
            .uid();

        if storage_owner != current_euid || metadata.uid() != current_euid {
            return Err(anyhow!(
                "session token file {token_path:?} must be owned by the current user and the auth storage dir owner"
            ));
        }
    }

    Ok(())
}

/// Normalize permissions and validate the generated-token file contents.
fn validate_and_restrict_session_token(
    token_file: &std::fs::File,
    token_path: &std::path::Path,
    token: String,
) -> Result<String> {
    set_restrictive_permissions(token_file, token_path)?;
    if token.len() != 64 || !token.chars().all(|ch| ch.is_ascii_hexdigit()) {
        Err(anyhow!(
            "session token file {token_path:?} must contain exactly 64 hexadecimal characters; delete it and restart to regenerate"
        ))
    } else {
        Ok(token)
    }
}

/// Atomically create a new session-token file.
fn write_session_token(token_path: &std::path::Path, token: &[u8]) -> Result<()> {
    let mut file = open_session_token_file(token_path)?;
    use std::io::Write;
    file.write_all(token)
        .with_context(|| format!("failed to write session token file {token_path:?}"))
        .and_then(|()| set_restrictive_permissions(&file, token_path))
}

/// Write a generated token, or reuse a concurrently created valid token file.
fn write_or_reuse_session_token(token_path: &std::path::Path, token: String) -> Result<String> {
    match write_session_token(token_path, token.as_bytes()) {
        Ok(()) => Ok(token),
        Err(error) if is_already_exists_error(&error) => {
            read_and_validate_session_token_with_retry(token_path)
        }
        Err(error) => Err(error),
    }
}

/// Retry token reads briefly while another process may be repairing permissions.
fn read_and_validate_session_token_with_retry(token_path: &std::path::Path) -> Result<String> {
    let storage_dir = token_path
        .parent()
        .ok_or_else(|| anyhow!("session token file path {token_path:?} has no parent directory"))?;
    const SESSION_TOKEN_READ_ATTEMPTS: usize = 10;
    let retry_delay = Duration::from_millis(25);

    for attempt in 0..SESSION_TOKEN_READ_ATTEMPTS {
        match read_and_validate_session_token(token_path, storage_dir) {
            Ok(token) => return Ok(token),
            Err(_error) if attempt + 1 < SESSION_TOKEN_READ_ATTEMPTS => {
                std::thread::sleep(retry_delay);
            }
            Err(error) => return Err(error),
        }
    }

    unreachable!("retry loop must return on success or failure")
}

/// Return whether an error chain contains an already-exists filesystem error.
fn is_already_exists_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists)
    })
}

#[cfg(unix)]
/// Open a new token file with owner-only permissions on Unix.
fn open_session_token_file(token_path: &std::path::Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(token_path)
        .with_context(|| format!("failed to create session token file {token_path:?}"))
}

#[cfg(not(unix))]
/// Refuse file-backed token creation where owner-only permissions are unavailable.
fn open_session_token_file(_token_path: &std::path::Path) -> Result<std::fs::File> {
    Err(anyhow!(
        "refusing to create session token file on non-Unix platforms because restrictive permissions are not implemented"
    ))
}

#[cfg(unix)]
fn open_existing_session_token_file(token_path: &std::path::Path) -> Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(token_path)
    {
        Ok(file) => Ok(file),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => Err(anyhow!(
            "session token file {token_path:?} must be a regular file"
        )),
        Err(error) => {
            Err(error).with_context(|| format!("failed to open session token file {token_path:?}"))
        }
    }
}

#[cfg(not(unix))]
fn open_existing_session_token_file(token_path: &std::path::Path) -> Result<std::fs::File> {
    std::fs::File::open(token_path)
        .with_context(|| format!("failed to open session token file {token_path:?}"))
}

/// Generate a fresh 32-byte local auth token encoded as lowercase hexadecimal.
fn generate_session_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .context("failed to obtain secure randomness for local auth token generation")?;
    Ok(encode_session_token(&bytes))
}

/// Encode raw token bytes as lowercase hexadecimal.
fn encode_session_token(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        token.push(HEX[(byte >> 4) as usize] as char);
        token.push(HEX[(byte & 0x0f) as usize] as char);
    }
    token
}

#[cfg(unix)]
/// Set owner-read/write-only permissions on a token file.
fn set_restrictive_permissions(file: &std::fs::File, path: &std::path::Path) -> Result<()> {
    use std::os::unix::io::AsRawFd;

    let result = unsafe { libc::fchmod(file.as_raw_fd(), 0o600) };
    if result == -1 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("failed to set restrictive permissions on {path:?}"));
    }

    Ok(())
}

#[cfg(not(unix))]
fn set_restrictive_permissions(_file: &std::fs::File, _path: &std::path::Path) -> Result<()> {
    Err(anyhow!(
        "session token files require Unix restrictive permissions and cannot be safely used on non-Unix platforms"
    ))
}

fn env_var_is_truthy(name: &str) -> bool {
    matches!(
        std::env::var(name)
            .ok()
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

fn parse_socket_addr(raw: &str) -> Result<SocketAddr> {
    let mut addrs = raw
        .to_socket_addrs()
        .with_context(|| format!("invalid listen address {raw}"))?;
    addrs
        .next()
        .ok_or_else(|| anyhow!("no socket addresses resolved for {raw}"))
}

fn parse_trust_state(
    value: Option<String>,
) -> Result<Option<rpc::RpcRepositoryTrustState>, String> {
    let Some(value) = value else {
        return Ok(None);
    };

    match value.as_str() {
        "trusted" => Ok(Some(rpc::RpcRepositoryTrustState::Trusted)),
        "readonly" | "read_only" => Ok(Some(rpc::RpcRepositoryTrustState::ReadOnly)),
        "blocked" => Ok(Some(rpc::RpcRepositoryTrustState::Blocked)),
        unknown => Err(format!("unknown trust_state '{unknown}'")),
    }
}

fn parse_list_repositories_payload(
    payload: ListRepositoriesRequestPayload,
) -> Result<rpc::ListRepositoriesRequest, String> {
    Ok(rpc::ListRepositoriesRequest {
        trust_state: parse_trust_state(payload.trust_state)?,
        page_size: payload.page_size,
        page_token: payload.page_token,
    })
}

fn parse_register_repository_payload(
    payload: RegisterRepositoryRequestPayload,
) -> Result<rpc::RegisterRepositoryRequest, String> {
    let allowed_scope = payload
        .allowed_scope
        .map(parse_path_scope_payload)
        .transpose()?;

    Ok(rpc::RegisterRepositoryRequest {
        display_name: payload.display_name,
        root_path: payload.root_path,
        allowed_scope,
        requester: parse_optional_rpc_actor(payload.requester),
    })
}

fn parse_list_repertoire_payload(
    _payload: ListRepertoireRequestPayload,
) -> rpc::ListRepertoireRequest {
    rpc::ListRepertoireRequest
}

fn parse_path_scope_kind(value: Option<String>) -> Result<rpc::RpcPathScopeKind, String> {
    let Some(value) = value else {
        return Ok(rpc::RpcPathScopeKind::Repository);
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "" => Err("path_scope.kind must not be empty".to_string()),
        "repository" => Ok(rpc::RpcPathScopeKind::Repository),
        "explicit_paths" | "explicit" => Ok(rpc::RpcPathScopeKind::ExplicitPaths),
        "read_only" | "readonly" => Ok(rpc::RpcPathScopeKind::ReadOnly),
        "unspecified" => Ok(rpc::RpcPathScopeKind::Unspecified),
        unknown => Err(format!("unknown path_scope.kind '{unknown}'")),
    }
}

fn parse_path_scope_payload(payload: PathScopePayload) -> Result<rpc::RpcPathScope, String> {
    let PathScopePayload {
        kind,
        roots,
        include_patterns,
        exclude_patterns,
    } = payload;
    let roots = roots.unwrap_or_default();
    let include_patterns = include_patterns.unwrap_or_default();
    let exclude_patterns = exclude_patterns.unwrap_or_default();

    if kind.is_none()
        && (!roots.is_empty() || !include_patterns.is_empty() || !exclude_patterns.is_empty())
    {
        return Err(
            "path_scope.kind is required when roots or include/exclude patterns are provided"
                .to_string(),
        );
    }

    Ok(rpc::RpcPathScope {
        kind: parse_path_scope_kind(kind)?,
        roots,
        include_patterns,
        exclude_patterns,
    })
}

fn parse_actor_payload(payload: ActorPayload) -> rpc::RpcActorDto {
    match payload {
        ActorPayload::User { id, display_name } => rpc::RpcActorDto::User { id, display_name },
        ActorPayload::Agent { id, display_name } => rpc::RpcActorDto::Agent { id, display_name },
        ActorPayload::Extension { id } => rpc::RpcActorDto::Extension { id },
        ActorPayload::System { id } => rpc::RpcActorDto::System { id },
    }
}

fn parse_optional_core_actor(payload: Option<ActorPayload>) -> rpc::RpcResult<Option<Actor>> {
    payload
        .map(parse_actor_payload)
        .map(Actor::try_from)
        .transpose()
}

fn parse_optional_rpc_actor(payload: Option<ActorPayload>) -> Option<rpc::RpcActorDto> {
    payload.map(parse_actor_payload)
}

fn package_http_request_source(request_source: Option<String>) -> Option<String> {
    request_source
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| Some("secretary.http".to_string()))
}

fn parse_job_status(value: Option<String>) -> Result<Option<rpc::RpcJobStatus>, String> {
    let Some(value) = value else {
        return Ok(None);
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "" | "unspecified" => Ok(None),
        "queued" => Ok(Some(rpc::RpcJobStatus::Queued)),
        "running" => Ok(Some(rpc::RpcJobStatus::Running)),
        "succeeded" => Ok(Some(rpc::RpcJobStatus::Succeeded)),
        "failed" => Ok(Some(rpc::RpcJobStatus::Failed)),
        "blocked" => Ok(Some(rpc::RpcJobStatus::Blocked)),
        "canceled" | "cancelled" => Ok(Some(rpc::RpcJobStatus::Canceled)),
        unknown => Err(format!("unknown job status '{unknown}'")),
    }
}

fn parse_submit_job_payload(
    payload: SubmitJobRequestPayload,
) -> Result<rpc::SubmitJobRequest, String> {
    let requested_capabilities = payload.requested_capabilities.unwrap_or_default();
    let path_scope = payload
        .path_scope
        .map(parse_path_scope_payload)
        .transpose()?;
    let tool_args = payload.tool_args.map(|payload| rpc::SubmitJobToolArgs {
        pattern: payload.pattern,
        max: payload.max,
        comparison_path: payload.comparison_path,
        max_bytes: payload.max_bytes,
        max_chars: payload.max_chars,
    });

    if requests_filesystem_path_operation(&requested_capabilities) {
        let roots = path_scope
            .as_ref()
            .map(|scope| scope.roots.as_slice())
            .ok_or_else(|| {
                "filesystem operation requires path_scope.roots to contain exactly one concrete path"
                    .to_string()
            })?;
        if roots.len() != 1 || roots[0].trim().is_empty() || roots[0].trim() == "." {
            return Err(
                "filesystem operation requires path_scope.roots to contain exactly one concrete path"
                    .to_string(),
            );
        }
    }

    Ok(rpc::SubmitJobRequest {
        repository_id: payload.repository_id,
        requester: parse_actor_payload(payload.requester),
        kind: payload.kind,
        goal: payload.goal,
        path_scope,
        requested_capabilities,
        tool_args,
        idempotency_key: payload.idempotency_key,
    })
}

fn parse_list_jobs_payload(
    payload: ListJobsRequestPayload,
) -> Result<rpc::ListJobsRequest, String> {
    Ok(rpc::ListJobsRequest {
        repository_id: payload.repository_id,
        status: parse_job_status(payload.status)?,
        requester: payload.requester.map(parse_actor_payload),
        page_size: payload.page_size,
        page_token: payload.page_token,
    })
}

fn parse_output_format(value: String) -> Result<rpc::RpcOutputFormat, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "toon" => Ok(rpc::RpcOutputFormat::Toon),
        "json" => Ok(rpc::RpcOutputFormat::Json),
        "text" => Ok(rpc::RpcOutputFormat::Text),
        unknown => Err(format!("unknown render format '{unknown}'")),
    }
}

fn parse_tool_result_ref_payload(payload: ToolResultRefPayload) -> rpc::ToolResultRef {
    rpc::ToolResultRef {
        tool_result_id: payload.tool_result_id,
        tool_invocation_id: payload.tool_invocation_id,
        job_id: payload.job_id,
        repository_id: payload.repository_id,
        content_type: payload.content_type,
    }
}

fn parse_render_tool_output_payload(
    payload: RenderToolOutputRequestPayload,
) -> Result<rpc::RenderToolOutputRequest, String> {
    Ok(rpc::RenderToolOutputRequest {
        tool_result: parse_tool_result_ref_payload(payload.tool_result),
        format: parse_output_format(payload.format)?,
    })
}

fn parse_event_cursor_payload(
    payload: EventCursorPayload,
) -> Result<rpc::EventCursorRequest, String> {
    Ok(match payload {
        EventCursorPayload::Beginning => rpc::EventCursorRequest::Beginning,
        EventCursorPayload::AfterSequence { sequence_number } => {
            rpc::EventCursorRequest::AfterSequence(sequence_number)
        }
        EventCursorPayload::AfterEventId { event_id } => {
            rpc::EventCursorRequest::AfterEventId(event_id)
        }
    })
}

fn serialize_event_cursor_request(cursor: &rpc::EventCursorRequest) -> serde_json::Value {
    match cursor {
        rpc::EventCursorRequest::Beginning => serde_json::json!({
            "kind": "beginning",
        }),
        rpc::EventCursorRequest::AfterSequence(sequence_number) => serde_json::json!({
            "kind": "after_sequence",
            "sequence_number": sequence_number,
        }),
        rpc::EventCursorRequest::AfterEventId(event_id) => serde_json::json!({
            "kind": "after_event_id",
            "event_id": event_id,
        }),
    }
}

fn parse_event_severity(value: Option<String>) -> Result<Option<rpc::RpcEventSeverity>, String> {
    let Some(value) = value else {
        return Ok(None);
    };

    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "debug" => Ok(Some(rpc::RpcEventSeverity::Debug)),
        "info" => Ok(Some(rpc::RpcEventSeverity::Info)),
        "warning" | "warn" => Ok(Some(rpc::RpcEventSeverity::Warning)),
        "error" => Ok(Some(rpc::RpcEventSeverity::Error)),
        _ => Err(format!("unknown min_severity '{value}'")),
    }
}

fn parse_list_events_payload(
    payload: ListEventsRequestPayload,
) -> Result<rpc::ListEventsRequest, String> {
    Ok(rpc::ListEventsRequest {
        repository_id: payload.repository_id,
        cursor: match payload.cursor {
            Some(cursor) => Some(parse_event_cursor_payload(cursor)?),
            None => None,
        },
        subject_ids: payload.subject_ids.unwrap_or_default(),
        job_ids: payload.job_ids.unwrap_or_default(),
        min_severity: parse_event_severity(payload.min_severity)?,
        page_size: payload.page_size,
        page_token: payload.page_token,
    })
}

fn parse_list_job_events_payload(
    job_id: String,
    payload: ListJobEventsRequestPayload,
) -> Result<rpc::ListEventsRequest, String> {
    Ok(rpc::ListEventsRequest {
        repository_id: payload.repository_id,
        cursor: payload.cursor.map(parse_event_cursor_payload).transpose()?,
        subject_ids: Vec::new(),
        job_ids: vec![job_id],
        min_severity: parse_event_severity(payload.min_severity)?,
        page_size: payload.page_size,
        page_token: payload.page_token,
    })
}

fn parse_replay_events_payload(
    payload: ReplayEventsRequestPayload,
) -> Result<rpc::WatchEventsRequest, String> {
    Ok(rpc::WatchEventsRequest {
        repository_id: payload.repository_id,
        cursor: match payload.cursor {
            Some(cursor) => Some(parse_event_cursor_payload(cursor)?),
            None => None,
        },
        subject_ids: payload.subject_ids.unwrap_or_default(),
        min_severity: parse_event_severity(payload.min_severity)?,
        limit: payload.limit,
    })
}

fn parse_watch_events_payload(
    payload: ReplayEventsRequestPayload,
) -> Result<rpc::WatchEventsRequest, String> {
    parse_replay_events_payload(payload)
}

fn core_tool_output_scope_to_rpc(
    scope: ToolOutputSettingsScope,
) -> Result<rpc::RpcToolOutputScope> {
    Ok(rpc::RpcToolOutputScope {
        level: match scope.level {
            atelia_core::ToolOutputSettingsLevel::Workspace => {
                rpc::RpcToolOutputScopeLevel::Workspace
            }
            atelia_core::ToolOutputSettingsLevel::Repository { repository_id } => {
                rpc::RpcToolOutputScopeLevel::Repository {
                    repository_id: repository_id.as_str().to_string(),
                }
            }
            atelia_core::ToolOutputSettingsLevel::Project { project_id } => {
                rpc::RpcToolOutputScopeLevel::Project {
                    project_id: rpc::project_id_to_string(&project_id)
                        .map_err(|error| anyhow!(error.reason))?,
                }
            }
            atelia_core::ToolOutputSettingsLevel::Session { session_id } => {
                rpc::RpcToolOutputScopeLevel::Session { session_id }
            }
            atelia_core::ToolOutputSettingsLevel::AgentProfile { agent_id } => {
                rpc::RpcToolOutputScopeLevel::AgentProfile { agent_id }
            }
        },
        tool_id: scope.tool_id,
    })
}
fn core_tool_output_overrides_to_rpc(
    overrides: ToolOutputOverrides,
) -> rpc::RpcToolOutputOverrides {
    rpc::RpcToolOutputOverrides {
        format: overrides.format.map(rpc::RpcOutputFormat::from),
        include_policy: overrides.include_policy,
        include_diagnostics: overrides.include_diagnostics,
        include_cost: overrides.include_cost,
        max_inline_bytes: overrides.max_inline_bytes,
        max_inline_lines: overrides.max_inline_lines,
        verbosity: overrides.verbosity.map(rpc::RpcToolOutputVerbosity::from),
        granularity: overrides
            .granularity
            .map(rpc::RpcToolOutputGranularity::from),
        oversize_policy: overrides
            .oversize_policy
            .map(rpc::RpcOversizeOutputPolicy::from),
    }
}

fn rpc_tool_output_scope_to_core(
    scope: &rpc::RpcToolOutputScope,
) -> Result<ToolOutputSettingsScope> {
    let level = match &scope.level {
        rpc::RpcToolOutputScopeLevel::Workspace => atelia_core::ToolOutputSettingsLevel::Workspace,
        rpc::RpcToolOutputScopeLevel::Repository { repository_id } => {
            atelia_core::ToolOutputSettingsLevel::Repository {
                repository_id: serde_json::from_str::<RepositoryId>(&format!(
                    "\"{repository_id}\""
                ))
                .with_context(|| {
                    format!("invalid repository_id in tool output scope: {repository_id}")
                })?,
            }
        }
        rpc::RpcToolOutputScopeLevel::Project { project_id } => {
            atelia_core::ToolOutputSettingsLevel::Project {
                project_id: serde_json::from_str::<ProjectId>(&format!("\"{project_id}\""))
                    .with_context(|| {
                        format!("invalid project_id in tool output scope: {project_id}")
                    })?,
            }
        }
        rpc::RpcToolOutputScopeLevel::Session { session_id } => {
            atelia_core::ToolOutputSettingsLevel::Session {
                session_id: session_id.clone(),
            }
        }
        rpc::RpcToolOutputScopeLevel::AgentProfile { agent_id } => {
            atelia_core::ToolOutputSettingsLevel::AgentProfile {
                agent_id: agent_id.clone(),
            }
        }
    };

    Ok(ToolOutputSettingsScope {
        level,
        tool_id: scope.tool_id.clone(),
    })
}

fn rpc_render_options_to_core(render_options: &rpc::RpcToolOutputRenderOptions) -> RenderOptions {
    RenderOptions {
        format: rpc_output_format_to_core(&render_options.format),
        include_policy: render_options.include_policy,
        include_diagnostics: render_options.include_diagnostics,
        include_cost: render_options.include_cost,
    }
}

fn rpc_defaults_to_core(defaults: &rpc::RpcToolOutputDefaults) -> ToolOutputDefaults {
    ToolOutputDefaults {
        render_options: rpc_render_options_to_core(&defaults.render_options),
        max_inline_bytes: defaults.max_inline_bytes,
        max_inline_lines: defaults.max_inline_lines,
        verbosity: rpc_tool_output_verbosity_to_core(&defaults.verbosity),
        granularity: rpc_tool_output_granularity_to_core(&defaults.granularity),
        oversize_policy: rpc_oversize_policy_to_core(&defaults.oversize_policy),
    }
}

fn rpc_change_to_core(
    change: &rpc::RpcToolOutputSettingsChange,
) -> Result<ToolOutputSettingsChange> {
    Ok(ToolOutputSettingsChange {
        schema_version: change.schema_version,
        actor: rpc_actor_to_core(&change.actor),
        scope: rpc_tool_output_scope_to_core(&change.scope)?,
        old_defaults: rpc_defaults_to_core(&change.old_defaults),
        new_defaults: rpc_defaults_to_core(&change.new_defaults),
        reason: change.reason.clone(),
        changed_at: LedgerTimestamp::from_unix_millis(change.changed_at_unix_ms),
    })
}

fn rpc_actor_to_core(actor: &rpc::RpcActorDto) -> Actor {
    match actor {
        rpc::RpcActorDto::User { id, display_name } => Actor::User {
            id: id.clone(),
            display_name: display_name.clone(),
        },
        rpc::RpcActorDto::Agent { id, display_name } => Actor::Agent {
            id: id.clone(),
            display_name: display_name.clone(),
        },
        rpc::RpcActorDto::Extension { id } => Actor::Extension { id: id.clone() },
        rpc::RpcActorDto::System { id } => Actor::System { id: id.clone() },
    }
}

fn rpc_output_format_to_core(format: &rpc::RpcOutputFormat) -> OutputFormat {
    match format {
        rpc::RpcOutputFormat::Toon => OutputFormat::Toon,
        rpc::RpcOutputFormat::Json => OutputFormat::Json,
        rpc::RpcOutputFormat::Text => OutputFormat::Text,
    }
}

fn rpc_tool_output_verbosity_to_core(
    verbosity: &rpc::RpcToolOutputVerbosity,
) -> ToolOutputVerbosity {
    match verbosity {
        rpc::RpcToolOutputVerbosity::Minimal => ToolOutputVerbosity::Minimal,
        rpc::RpcToolOutputVerbosity::Normal => ToolOutputVerbosity::Normal,
        rpc::RpcToolOutputVerbosity::Expanded => ToolOutputVerbosity::Expanded,
        rpc::RpcToolOutputVerbosity::Debug => ToolOutputVerbosity::Debug,
    }
}

fn rpc_tool_output_granularity_to_core(
    granularity: &rpc::RpcToolOutputGranularity,
) -> ToolOutputGranularity {
    match granularity {
        rpc::RpcToolOutputGranularity::Summary => ToolOutputGranularity::Summary,
        rpc::RpcToolOutputGranularity::KeyFields => ToolOutputGranularity::KeyFields,
        rpc::RpcToolOutputGranularity::Full => ToolOutputGranularity::Full,
    }
}

fn rpc_oversize_policy_to_core(policy: &rpc::RpcOversizeOutputPolicy) -> OversizeOutputPolicy {
    match policy {
        rpc::RpcOversizeOutputPolicy::TruncateWithMetadata => {
            OversizeOutputPolicy::TruncateWithMetadata
        }
        rpc::RpcOversizeOutputPolicy::SpillToArtifactRef => {
            OversizeOutputPolicy::SpillToArtifactRef
        }
        rpc::RpcOversizeOutputPolicy::RejectOversize => OversizeOutputPolicy::RejectOversize,
    }
}

fn serialize_tool_output_defaults_response(
    response: rpc::GetToolOutputDefaultsResponse,
) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "scope": serde_json::to_value(rpc_tool_output_scope_to_core(&response.scope)?)?,
        "defaults": serde_json::to_value(rpc_defaults_to_core(&response.defaults))?,
    }))
}

fn serialize_tool_output_update_response(
    response: rpc::UpdateToolOutputDefaultsResponse,
) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "change": serialize_tool_output_change(&response.change)?,
    }))
}

fn serialize_tool_output_history_response(
    response: rpc::ListToolOutputSettingsHistoryResponse,
) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "changes": response
            .changes
            .iter()
            .map(serialize_tool_output_change)
            .collect::<Result<Vec<_>>>()?,
        "next_page_token": response.next_page_token,
    }))
}

fn serialize_tool_output_change(
    change: &rpc::RpcToolOutputSettingsChange,
) -> Result<serde_json::Value> {
    let mut value = serde_json::to_value(rpc_change_to_core(change)?)?;

    if let serde_json::Value::Object(ref mut map) = value {
        if let Some(serde_json::Value::Object(changed_at)) = map.get("changed_at") {
            if let Some(unix_millis) = changed_at.get("unix_millis").cloned() {
                map.insert("changed_at_unix_ms".to_string(), unix_millis);
            }
        }
    }

    Ok(value)
}

fn parse_get_project_status_payload(
    payload: GetProjectStatusRequestPayload,
) -> rpc::GetProjectStatusRequest {
    rpc::GetProjectStatusRequest {
        repository_id: payload.repository_id,
    }
}

fn serialize_protocol_metadata(metadata: &rpc::ProtocolMetadata) -> serde_json::Value {
    serde_json::json!({
        "protocol_version": metadata.protocol_version,
        "daemon_version": metadata.daemon_version,
        "storage_version": metadata.storage_version,
        "capabilities": metadata.capabilities,
    })
}

fn serialize_path_scope_kind(kind: &rpc::RpcPathScopeKind) -> &'static str {
    match kind {
        rpc::RpcPathScopeKind::Unspecified => "unspecified",
        rpc::RpcPathScopeKind::Repository => "repository",
        rpc::RpcPathScopeKind::ExplicitPaths => "explicit_paths",
        rpc::RpcPathScopeKind::ReadOnly => "read_only",
    }
}

fn serialize_trust_state(state: &rpc::RpcRepositoryTrustState) -> &'static str {
    match state {
        rpc::RpcRepositoryTrustState::Unspecified => "unspecified",
        rpc::RpcRepositoryTrustState::Trusted => "trusted",
        rpc::RpcRepositoryTrustState::ReadOnly => "read_only",
        rpc::RpcRepositoryTrustState::Blocked => "blocked",
    }
}

fn serialize_health_response(response: rpc::HealthResponse) -> serde_json::Value {
    serde_json::json!({
        "status": response.status,
        "daemon_version": response.daemon_version,
        "protocol_version": response.protocol_version,
        "storage_version": response.storage_version,
        "storage_status": response.storage_status,
        "daemon_status": response.daemon_status,
        "beta_state": response.beta_state.map(|beta_state| serde_json::json!({
            "scope": beta_state.scope,
            "durability": beta_state.durability,
            "restart_semantics": beta_state.restart_semantics,
            "limits": beta_state.limits,
        })),
        "capabilities": response.capabilities,
    })
}

fn serialize_allowed_scope(scope: &rpc::RpcPathScope) -> serde_json::Value {
    serde_json::json!({
        "kind": serialize_path_scope_kind(&scope.kind),
        "roots": scope.roots,
        "include_patterns": scope.include_patterns,
        "exclude_patterns": scope.exclude_patterns,
    })
}

fn serialize_repository(repository: &rpc::Repository) -> serde_json::Value {
    serde_json::json!({
        "repository_id": repository.repository_id,
        "display_name": repository.display_name,
        "root_path": repository.root_path,
        "allowed_scope": serialize_allowed_scope(&repository.allowed_scope),
        "trust_state": serialize_trust_state(&repository.trust_state),
        "created_at_unix_ms": repository.created_at_unix_ms,
        "updated_at_unix_ms": repository.updated_at_unix_ms,
    })
}

fn serialize_actor(actor: &rpc::RpcActorDto) -> serde_json::Value {
    match actor {
        rpc::RpcActorDto::User { id, display_name } => serde_json::json!({
            "type": "user",
            "id": id,
            "display_name": display_name,
        }),
        rpc::RpcActorDto::Agent { id, display_name } => serde_json::json!({
            "type": "agent",
            "id": id,
            "display_name": display_name,
        }),
        rpc::RpcActorDto::Extension { id } => serde_json::json!({
            "type": "extension",
            "id": id,
        }),
        rpc::RpcActorDto::System { id } => serde_json::json!({
            "type": "system",
            "id": id,
        }),
    }
}

fn serialize_policy_summary(summary: &rpc::PolicySummary) -> serde_json::Value {
    serde_json::json!({
        "decision_id": summary.decision_id,
        "outcome": summary.outcome,
        "risk_tier": summary.risk_tier,
        "reason_code": summary.reason_code,
    })
}

fn serialize_job_cancellation(cancellation: &rpc::JobCancellation) -> serde_json::Value {
    serde_json::json!({
        "state": cancellation.state,
        "requested_by": cancellation.requested_by.as_ref().map(serialize_actor),
        "reason": cancellation.reason,
        "requested_at_unix_ms": cancellation.requested_at_unix_ms,
        "completed_at_unix_ms": cancellation.completed_at_unix_ms,
    })
}

fn serialize_job(job: &rpc::Job) -> serde_json::Value {
    let mut object = serde_json::Map::from_iter([
        (
            "job_id".to_string(),
            serde_json::Value::String(job.job_id.clone()),
        ),
        (
            "repository_id".to_string(),
            serde_json::Value::String(job.repository_id.clone()),
        ),
        ("requester".to_string(), serialize_actor(&job.requester)),
        (
            "kind".to_string(),
            serde_json::Value::String(job.kind.clone()),
        ),
        (
            "status".to_string(),
            serde_json::Value::String(job.status.clone()),
        ),
        (
            "policy_summary".to_string(),
            serde_json::to_value(job.policy_summary.as_ref().map(serialize_policy_summary))
                .expect("serialize policy summary"),
        ),
        (
            "created_at_unix_ms".to_string(),
            serde_json::Value::from(job.created_at_unix_ms),
        ),
        (
            "started_at_unix_ms".to_string(),
            serde_json::to_value(job.started_at_unix_ms).expect("serialize started_at_unix_ms"),
        ),
        (
            "completed_at_unix_ms".to_string(),
            serde_json::to_value(job.completed_at_unix_ms).expect("serialize completed_at_unix_ms"),
        ),
        (
            "latest_event_id".to_string(),
            serde_json::to_value(&job.latest_event_id).expect("serialize latest_event_id"),
        ),
        (
            "cancellation".to_string(),
            serialize_job_cancellation(&job.cancellation),
        ),
    ]);
    if let Some(goal) = &job.goal {
        object.insert("goal".to_string(), serde_json::Value::String(goal.clone()));
    }
    serde_json::Value::Object(object)
}

fn serialize_policy_decision(decision: &rpc::PolicyDecision) -> serde_json::Value {
    serde_json::json!({
        "decision_id": decision.decision_id,
        "outcome": decision.outcome,
        "risk_tier": decision.risk_tier,
        "requested_capability": decision.requested_capability,
        "reason_code": decision.reason_code,
        "reason": decision.reason,
        "approval_request_ref": decision.approval_request_ref,
        "audit_ref": decision.audit_ref,
    })
}

fn serialize_event_cursor(cursor: &rpc::EventCursor) -> serde_json::Value {
    serde_json::json!({
        "sequence": cursor.sequence,
        "event_id": cursor.event_id,
    })
}

fn serialize_list_repositories_response(
    response: rpc::ListRepositoriesResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "repositories": response
            .repositories
            .iter()
            .map(serialize_repository)
            .collect::<Vec<_>>(),
        "next_page_token": response.next_page_token,
    })
}

fn serialize_register_repository_response(
    response: rpc::RegisterRepositoryResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "repository": serialize_repository(&response.repository),
        "policy": response.policy.as_ref().map(serialize_policy_decision),
    })
}

fn serialize_repertoire_entry(entry: &rpc::RepertoireEntry) -> serde_json::Value {
    serde_json::json!({
        "tool_id": entry.tool_id,
        "name": entry.name,
        "description": entry.description,
        "provider_kind": entry.provider_kind,
        "provider_id": entry.provider_id,
        "risk_tier": entry.risk_tier,
        "default_result_format": entry.default_result_format,
        "supported_result_formats": entry.supported_result_formats,
        "idempotency": entry.idempotency,
        "cancellable": entry.cancellable,
        "streaming": entry.streaming,
        "timeout_ms": entry.timeout_ms,
    })
}

fn serialize_list_repertoire_response(response: rpc::ListRepertoireResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "entries": response
            .entries
            .iter()
            .map(serialize_repertoire_entry)
            .collect::<Vec<_>>(),
    })
}

fn serialize_submit_job_response(response: rpc::SubmitJobResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "job": serialize_job(&response.job),
        "policy": serialize_policy_decision(&response.policy),
    })
}

fn serialize_get_job_response(response: rpc::GetJobResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "job": serialize_job(&response.job),
    })
}

fn serialize_list_jobs_response(response: rpc::ListJobsResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "jobs": response.jobs.iter().map(serialize_job).collect::<Vec<_>>(),
        "next_page_token": response.next_page_token,
    })
}

fn serialize_cancel_job_response(response: rpc::CancelJobResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "job": serialize_job(&response.job),
        "cancellation": serialize_job_cancellation(&response.cancellation),
    })
}

fn serialize_install_extension_response(
    response: rpc::InstallExtensionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "record": response.record,
        "audit_record_id": response.audit_record_id,
    })
}

/// Serialize a successful manifest validation response for the HTTP boundary.
fn serialize_validate_extension_response(
    response: rpc::ValidateExtensionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "manifest": response.manifest,
        "boundary": response.boundary,
    })
}

fn serialize_update_extension_response(
    response: rpc::UpdateExtensionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "record": response.record,
        "audit_record_id": response.audit_record_id,
    })
}

fn serialize_extension_status_response(
    response: rpc::ExtensionStatusResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "extension": response.extension,
    })
}

/// Serializes the RPC inspect envelope into the public package inspect JSON shape.
fn serialize_package_inspect_response(response: rpc::PackageInspectResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "package_id": response.package_id,
        "extension": response.extension,
        "manifest": response.manifest,
        "block": response.block,
        "permissions": response.permissions,
        "services": response.services,
        "rollback_available": response.rollback_available,
        "rollback_snapshot": response.rollback_snapshot,
        "source": response.source,
        "trust": response.trust,
    })
}

fn serialize_list_extensions_response(response: rpc::ListExtensionsResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "extensions": response.extensions,
    })
}

fn serialize_list_package_trust_index_response(
    response: rpc::ListPackageTrustIndexResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "packages": response.packages,
    })
}

/// Serialize a package authoring-flow RPC response for the HTTP API.
fn serialize_package_authoring_flow_response(
    response: rpc::PackageAuthoringFlowResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "flow": response.flow,
    })
}

/// Serialize a package remix RPC response for the HTTP API.
fn serialize_package_remix_response(response: rpc::PackageRemixResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "flow": response.flow,
    })
}

/// Serialize a package publication RPC response for the HTTP API.
fn serialize_package_publication_response(
    response: rpc::PackagePublicationResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "flow": response.flow,
        "audit_record_id": response.audit_record_id,
    })
}

/// Serialize a package registry-submission RPC response for the HTTP API.
fn serialize_package_registry_submission_response(
    response: rpc::PackageRegistrySubmissionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "package_id": response.package_id,
        "state": response.state,
        "flow": response.flow,
        "audit_record_id": response.audit_record_id,
    })
}

fn serialize_rollback_extension_response(
    response: rpc::RollbackExtensionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "record": response.record,
        "audit_record_id": response.audit_record_id,
    })
}

fn serialize_disable_extension_response(
    response: rpc::DisableExtensionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "record": response.record,
        "audit_record_id": response.audit_record_id,
    })
}

fn serialize_enable_extension_response(
    response: rpc::EnableExtensionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "record": response.record,
        "audit_record_id": response.audit_record_id,
    })
}

fn serialize_remove_extension_response(
    response: rpc::RemoveExtensionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "record": response.record,
        "audit_record_id": response.audit_record_id,
    })
}

fn serialize_apply_blocklist_response(response: rpc::ApplyBlocklistResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "entry": response.entry,
        "audit_record_id": response.audit_record_id,
    })
}

fn serialize_list_blocklist_response(response: rpc::ListBlocklistResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "entries": response.entries,
    })
}

fn serialize_list_extension_registry_audit_records_response(
    response: rpc::ListExtensionRegistryAuditRecordsResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "records": response.records,
        "next_page_token": response.next_page_token,
    })
}

fn serialize_extension_execution_response(
    response: rpc::ExtensionExecutionResponse,
) -> serde_json::Value {
    let metadata = rpc::ProtocolMetadata::from(response.metadata);
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&metadata),
    })
}

fn serialize_output_format(format: &rpc::RpcOutputFormat) -> &'static str {
    match format {
        rpc::RpcOutputFormat::Toon => "toon",
        rpc::RpcOutputFormat::Json => "json",
        rpc::RpcOutputFormat::Text => "text",
    }
}

fn serialize_tool_result_ref(tool_result: &rpc::ToolResultRef) -> serde_json::Value {
    serde_json::json!({
        "tool_result_id": tool_result.tool_result_id,
        "tool_invocation_id": tool_result.tool_invocation_id,
        "job_id": tool_result.job_id,
        "repository_id": tool_result.repository_id,
        "content_type": tool_result.content_type,
    })
}

fn serialize_rendered_tool_output_metadata(
    metadata: &rpc::RenderedToolOutputMetadata,
) -> serde_json::Value {
    serde_json::json!({
        "degraded": metadata.degraded,
        "fallback_reason": metadata.fallback_reason,
        "truncation": metadata.truncation,
    })
}

fn serialize_render_tool_output_response(
    response: rpc::RenderToolOutputResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "tool_result": serialize_tool_result_ref(&response.tool_result),
        "format": serialize_output_format(&response.format),
        "rendered_output": response.rendered_output,
        "rendered_output_metadata": serialize_rendered_tool_output_metadata(
            &response.rendered_output_metadata
        ),
    })
}

fn serialize_list_events_response(response: rpc::ListEventsResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "events": response
            .events
            .iter()
            .map(serialize_event)
            .collect::<Vec<_>>(),
        "next_page_token": response.next_page_token,
    })
}

fn serialize_replay_events_response(response: rpc::WatchEventsReplayResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "events": response
            .events
            .iter()
            .map(serialize_event)
            .collect::<Vec<_>>(),
        "cursor": response.cursor.as_ref().map(serialize_event_cursor_request),
    })
}

fn serialize_watch_events_snapshot(response: &rpc::WatchEventsLiveResponse) -> serde_json::Value {
    serde_json::json!({
        "kind": "snapshot",
        "metadata": serialize_protocol_metadata(&response.metadata),
        "events": response
            .events
            .iter()
            .map(serialize_event)
            .collect::<Vec<_>>(),
        "cursor": response.cursor.as_ref().map(serialize_event_cursor_request),
    })
}

fn serialize_watch_events_recovery_error(reason: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "kind": "error",
        "error": {
            "code": "cursor_expired",
            "reason": reason.into(),
            "recoverable": true,
            "next_state": "refresh_status",
        },
    })
}

fn ndjson_body_from_frame(frame: serde_json::Value) -> Body {
    let stream = stream! {
        match serde_json::to_vec(&frame) {
            Ok(bytes) => {
                yield Ok::<Vec<u8>, std::io::Error>(bytes);
            }
            Err(error) => {
                yield Err::<Vec<u8>, std::io::Error>(std::io::Error::other(error));
                return;
            }
        }
        yield Ok::<Vec<u8>, std::io::Error>(b"\n".to_vec());
    };

    Body::from_stream(stream)
}

fn serialize_event(event: &rpc::RpcEvent) -> serde_json::Value {
    serde_json::json!({
        "event_id": event.event_id,
        "sequence": event.sequence,
        "occurred_at_unix_ms": event.occurred_at_unix_ms,
        "subject": serialize_event_subject(&event.subject),
        "kind": event.kind,
        "severity": serialize_event_severity(&event.severity),
        "message": event.message,
        "refs": serialize_event_refs(&event.refs),
    })
}

fn serialize_event_subject(subject: &rpc::RpcEventSubject) -> serde_json::Value {
    serde_json::json!({
        "type": serialize_event_subject_type(&subject.subject_type),
        "id": subject.id,
    })
}

fn serialize_event_subject_type(subject_type: &rpc::RpcEventSubjectType) -> &'static str {
    match subject_type {
        rpc::RpcEventSubjectType::Unspecified => "unspecified",
        rpc::RpcEventSubjectType::Daemon => "daemon",
        rpc::RpcEventSubjectType::Repository => "repository",
        rpc::RpcEventSubjectType::Job => "job",
        rpc::RpcEventSubjectType::PolicyDecision => "policy_decision",
        rpc::RpcEventSubjectType::LockDecision => "lock_decision",
        rpc::RpcEventSubjectType::ToolInvocation => "tool_invocation",
        rpc::RpcEventSubjectType::ToolResult => "tool_result",
        rpc::RpcEventSubjectType::AuditRecord => "audit_record",
    }
}

fn serialize_event_severity(severity: &rpc::RpcEventSeverity) -> &'static str {
    match severity {
        rpc::RpcEventSeverity::Debug => "debug",
        rpc::RpcEventSeverity::Info => "info",
        rpc::RpcEventSeverity::Warning => "warning",
        rpc::RpcEventSeverity::Error => "error",
    }
}

fn serialize_event_refs(refs: &rpc::RpcEventRefs) -> serde_json::Value {
    serde_json::json!({
        "repository_id": refs.repository_id,
        "job_id": refs.job_id,
        "policy_decision_id": refs.policy_decision_id,
        "lock_decision_id": refs.lock_decision_id,
        "tool_invocation_id": refs.tool_invocation_id,
        "tool_result_id": refs.tool_result_id,
        "audit_ref": refs.audit_ref,
    })
}

fn serialize_project_status_response(response: rpc::GetProjectStatusResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "repository": serialize_repository(&response.repository),
        "recent_jobs": response.recent_jobs.iter().map(serialize_job).collect::<Vec<_>>(),
        "recent_policy_decisions": response
            .recent_policy_decisions
            .iter()
            .map(serialize_policy_decision)
            .collect::<Vec<_>>(),
        "latest_cursor": response
            .latest_cursor
            .as_ref()
            .map(serialize_event_cursor),
        "daemon_status": response.daemon_status,
        "storage_status": response.storage_status,
    })
}

fn make_error_response(
    status_code: StatusCode,
    code: &'static str,
    reason: impl Into<String>,
    recoverable: bool,
    next_state: impl Into<String>,
) -> Response {
    (
        status_code,
        Json(ApiResponse::error(code, reason, recoverable, next_state)),
    )
        .into_response()
}

/// Build a standard bearer-auth failure response.
fn unauthorized_response(reason: impl Into<String>) -> Response {
    let mut response = make_error_response(
        StatusCode::UNAUTHORIZED,
        "unauthorized",
        reason,
        false,
        "authentication_required",
    );
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        header::HeaderValue::from_static(r#"Bearer realm="Atelia Secretary""#),
    );
    response
}

fn transport_error_response(next_state: String, reason: impl std::fmt::Display) -> Response {
    make_error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "transport_error",
        reason.to_string(),
        false,
        next_state,
    )
}

async fn request_timeout_response(
    state: RpcServerState,
    path: &str,
    request_timeout: Duration,
) -> Response {
    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    make_error_response(
        StatusCode::GATEWAY_TIMEOUT,
        "request_timeout",
        format!("request to {path} exceeded {request_timeout:?}"),
        true,
        next_state,
    )
}

async fn with_request_timeout<F>(
    state: RpcServerState,
    path: String,
    request_timeout: Duration,
    future: F,
) -> Response
where
    F: std::future::Future<Output = Response>,
{
    match tokio::time::timeout(request_timeout, future).await {
        Ok(response) => response,
        Err(_) => request_timeout_response(state, &path, request_timeout).await,
    }
}

/// Enforce the configured local-auth mode for every HTTP route.
async fn local_auth_middleware(
    State(auth): State<LocalAuthConfig>,
    request: Request<Body>,
    next: Next,
) -> Response {
    match auth {
        LocalAuthConfig::Disabled => next.run(request).await,
        LocalAuthConfig::BearerToken { token } => {
            let provided = request
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .and_then(parse_bearer_token);

            match provided {
                Some(provided) if bearer_token_matches(&token, provided) => next.run(request).await,
                _ => unauthorized_response("missing or invalid Authorization header"),
            }
        }
    }
}

/// Compare bearer tokens without data-dependent early exit.
fn bearer_token_matches(expected: &str, provided: &str) -> bool {
    expected.as_bytes().ct_eq(provided.as_bytes()).into()
}

/// Parse a single `Authorization: Bearer <token>` header value.
fn parse_bearer_token(value: &str) -> Option<&str> {
    let mut parts = value.split_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;

    if parts.next().is_some() {
        return None;
    }

    if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() {
        Some(token)
    } else {
        None
    }
}

fn rpc_next_state(server: &rpc::SecretaryRpcServer) -> String {
    server.health(rpc::HealthRequest).daemon_status
}

fn rpc_error_status(code: rpc::RpcErrorCode) -> (StatusCode, bool) {
    match code {
        rpc::RpcErrorCode::InvalidArgument => (StatusCode::BAD_REQUEST, false),
        rpc::RpcErrorCode::NotFound => (StatusCode::NOT_FOUND, false),
        rpc::RpcErrorCode::CursorExpired => (StatusCode::GONE, true),
        rpc::RpcErrorCode::Conflict => (StatusCode::CONFLICT, true),
        rpc::RpcErrorCode::UnsupportedCapability => (StatusCode::NOT_IMPLEMENTED, true),
        rpc::RpcErrorCode::Internal => (StatusCode::INTERNAL_SERVER_ERROR, false),
    }
}

fn rpc_error_response(next_state: String, error: rpc::RpcError) -> Response {
    let (status, recoverable) = rpc_error_status(error.code);
    make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
}

enum BodyParseError {
    TooLarge,
    InvalidJson(String),
}

impl BodyParseError {
    fn into_response(self, next_state: String) -> Response {
        match self {
            Self::TooLarge => make_error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "request_too_large",
                format!("request body exceeded {MAX_REQUEST_BODY_BYTES} byte limit"),
                false,
                next_state,
            ),
            Self::InvalidJson(reason) => make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_json",
                reason,
                false,
                next_state,
            ),
        }
    }
}

fn is_body_size_error(message: &str) -> bool {
    // This check is only a compatibility shim for older/alternate to_bytes errors.
    // The explicit `payload.len() > MAX_REQUEST_BODY_BYTES` check below is the
    // authoritative body-size guard.
    let lower = message.to_lowercase();
    lower.contains("too large") || lower.contains("exceeded") || lower.contains("limit")
}

async fn body_or_empty_json<T>(request: Request<Body>) -> Result<T, BodyParseError>
where
    T: DeserializeOwned,
{
    let payload = to_bytes(request.into_body(), MAX_REQUEST_BODY_BYTES + 1)
        .await
        .map_err(|error| {
            let message = error.to_string();
            if is_body_size_error(&message) {
                BodyParseError::TooLarge
            } else {
                BodyParseError::InvalidJson(format!("invalid JSON: {message}"))
            }
        })?;
    if payload.len() > MAX_REQUEST_BODY_BYTES {
        return Err(BodyParseError::TooLarge);
    }
    let raw = if payload.is_empty() {
        b"{}".to_vec()
    } else {
        payload.to_vec()
    };
    serde_json::from_slice(&raw)
        .map_err(|err| BodyParseError::InvalidJson(format!("invalid JSON: {err}")))
}

async fn dispatch_health(state: RpcServerState) -> Response {
    let rpc_server = state.read().await;
    let response = rpc_server.health(rpc::HealthRequest);
    (
        StatusCode::OK,
        Json(ApiResponse::ok(serialize_health_response(response))),
    )
        .into_response()
}

async fn dispatch_submit_job(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<SubmitJobRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let parsed = match parse_submit_job_payload(payload) {
        Ok(request) => request,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.submit_job(parsed) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_submit_job_response(response))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_get_job(state: RpcServerState, job_id: String) -> Response {
    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.get_job(rpc::GetJobRequest { job_id }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_get_job_response(response))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_list_jobs(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<ListJobsRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let parsed = match parse_list_jobs_payload(payload) {
        Ok(request) => request,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.list_jobs(parsed) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_list_jobs_response(response))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_cancel_job(
    state: RpcServerState,
    job_id: String,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<CancelJobRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.cancel_job(rpc::CancelJobRequest {
        job_id,
        requester: parse_actor_payload(payload.requester),
        reason: payload.reason,
    }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_cancel_job_response(response))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_list_repositories(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<ListRepositoriesRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let parsed = match parse_list_repositories_payload(payload) {
        Ok(request) => request,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.list_repositories(parsed) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_list_repositories_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_register_repository(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<RegisterRepositoryRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let parsed = match parse_register_repository_payload(payload) {
        Ok(request) => request,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.register_repository(parsed) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_register_repository_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_list_repertoire(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<ListRepertoireRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let parsed = parse_list_repertoire_payload(payload);

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.list_repertoire(parsed) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_list_repertoire_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_list_events(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<ListEventsRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let parsed = match parse_list_events_payload(payload) {
        Ok(request) => request,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.list_events(parsed) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_list_events_response(response))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_list_job_events(
    state: RpcServerState,
    job_id: String,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<ListJobEventsRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let parsed = match parse_list_job_events_payload(job_id, payload) {
        Ok(request) => request,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.list_events(parsed) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_list_events_response(response))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_replay_events(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<ReplayEventsRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let parsed = match parse_replay_events_payload(payload) {
        Ok(request) => request,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.watch_events(parsed) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_replay_events_response(response))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            let code = if error.code == rpc::RpcErrorCode::CursorExpired {
                "cursor_expired"
            } else {
                "rpc_error"
            };
            make_error_response(status, code, error.reason, recoverable, next_state)
        }
    }
}

fn watch_events_stream_body(
    response: rpc::WatchEventsLiveResponse,
    request: rpc::WatchEventsRequest,
) -> Body {
    let snapshot = serialize_watch_events_snapshot(&response);
    let rpc::WatchEventsLiveResponse { subscription, .. } = response;

    let stream = stream! {
        match serde_json::to_vec(&snapshot) {
            Ok(bytes) => {
                yield Ok::<Vec<u8>, std::io::Error>(bytes);
            }
            Err(error) => {
                yield Err::<Vec<u8>, std::io::Error>(std::io::Error::other(error));
                return;
            }
        }
        yield Ok::<Vec<u8>, std::io::Error>(b"\n".to_vec());

        let mut receiver = subscription.receiver;
        let mut replay_max_sequence = subscription
            .replay_max_sequence
            .or(subscription.resolved_cursor_sequence);
        loop {
            match receiver.recv().await {
                Some(Ok(event)) => {
                    if replay_max_sequence.is_some_and(|max| event.sequence_number <= max) {
                        continue;
                    }
                    let rpc_event = rpc::RpcEvent::from(event);
                    if !rpc::watch_event_matches_request(&rpc_event, &request) {
                        continue;
                    }
                    replay_max_sequence = Some(rpc_event.sequence);
                    let frame = serde_json::json!({
                        "kind": "event",
                        "event": serialize_event(&rpc_event),
                    });
                    match serde_json::to_vec(&frame) {
                        Ok(bytes) => {
                            yield Ok::<Vec<u8>, std::io::Error>(bytes);
                        }
                        Err(error) => {
                            yield Err::<Vec<u8>, std::io::Error>(std::io::Error::other(error));
                            return;
                        }
                    }
                    yield Ok::<Vec<u8>, std::io::Error>(b"\n".to_vec());
                }
                Some(Err(StoreError::CursorExpired { reason })) => {
                    let frame = serialize_watch_events_recovery_error(reason);
                    match serde_json::to_vec(&frame) {
                        Ok(bytes) => {
                            yield Ok::<Vec<u8>, std::io::Error>(bytes);
                        }
                        Err(error) => {
                            yield Err::<Vec<u8>, std::io::Error>(std::io::Error::other(error));
                            return;
                        }
                    }
                    yield Ok::<Vec<u8>, std::io::Error>(b"\n".to_vec());
                    return;
                }
                Some(Err(error)) => {
                    yield Err::<Vec<u8>, std::io::Error>(std::io::Error::other(error));
                    return;
                }
                None => break,
            }
        }
    };

    Body::from_stream(stream)
}

fn watch_events_cursor_expired_response(reason: impl Into<String>) -> Response {
    Response::builder()
        .status(StatusCode::GONE)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .body(ndjson_body_from_frame(
            serialize_watch_events_recovery_error(reason),
        ))
        .expect("cursor expired response")
}

async fn dispatch_watch_events(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<ReplayEventsRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let parsed = match parse_watch_events_payload(payload) {
        Ok(request) => request,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.watch_events_live(parsed.clone()) {
        Ok(response) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "application/x-ndjson")
            .body(watch_events_stream_body(response, parsed))
            .expect("stream response"),
        Err(error) if error.code == rpc::RpcErrorCode::CursorExpired => {
            watch_events_cursor_expired_response(error.reason)
        }
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_get_tool_output_defaults(
    state: RpcServerState,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<GetToolOutputDefaultsRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.get_tool_output_defaults(rpc::GetToolOutputDefaultsRequest {
        scope: match core_tool_output_scope_to_rpc(payload.scope) {
            Ok(scope) => scope,
            Err(error) => return transport_error_response(next_state, error),
        },
    }) {
        Ok(response) => match serialize_tool_output_defaults_response(response) {
            Ok(body) => (StatusCode::OK, Json(ApiResponse::ok(body))).into_response(),
            Err(error) => transport_error_response(next_state, error),
        },
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_update_tool_output_defaults(
    state: RpcServerState,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<UpdateToolOutputDefaultsRequestPayload>(request).await
    {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.update_tool_output_defaults(rpc::UpdateToolOutputDefaultsRequest {
        scope: match core_tool_output_scope_to_rpc(payload.scope) {
            Ok(scope) => scope,
            Err(error) => return transport_error_response(next_state, error),
        },
        actor: rpc::RpcActorDto::from(payload.actor),
        reason: payload.reason,
        overrides: core_tool_output_overrides_to_rpc(payload.overrides),
    }) {
        Ok(response) => match serialize_tool_output_update_response(response) {
            Ok(body) => (StatusCode::OK, Json(ApiResponse::ok(body))).into_response(),
            Err(error) => transport_error_response(next_state, error),
        },
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_list_tool_output_settings_history(
    state: RpcServerState,
    request: Request<Body>,
) -> Response {
    let payload =
        match body_or_empty_json::<ListToolOutputSettingsHistoryRequestPayload>(request).await {
            Ok(payload) => payload,
            Err(error) => {
                let rpc_server = state.read().await;
                let next_state = rpc_next_state(&rpc_server);
                return error.into_response(next_state);
            }
        };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.list_tool_output_settings_history(rpc::ListToolOutputSettingsHistoryRequest {
        scope: match payload.scope {
            Some(scope) => match core_tool_output_scope_to_rpc(scope) {
                Ok(scope) => Some(scope),
                Err(error) => return transport_error_response(next_state, error),
            },
            None => None,
        },
        limit: payload.limit,
        offset: payload.offset,
        cursor: payload.cursor,
    }) {
        Ok(response) => match serialize_tool_output_history_response(response) {
            Ok(body) => (StatusCode::OK, Json(ApiResponse::ok(body))).into_response(),
            Err(error) => transport_error_response(next_state, error),
        },
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_install_extension(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<InstallExtensionRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    let requester = match parse_optional_core_actor(payload.requester) {
        Ok(requester) => requester,
        Err(error) => return rpc_error_response(next_state, error),
    };
    match rpc_server.install_extension(atelia_core::InstallExtensionRequest {
        manifest: payload.manifest,
        approve_local_unsigned: payload.approve_local_unsigned,
        allow_local_process_runtime: payload.allow_local_process_runtime,
        approve_source_change: payload.approve_source_change,
        requester,
        request_source: package_http_request_source(payload.request_source),
        reason: payload.reason,
    }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_install_extension_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

/// Dispatch manifest validation without installing the submitted manifest.
async fn dispatch_validate_extension(state: RpcServerState, request: Request<Body>) -> Response {
    let payload =
        match body_or_empty_json::<atelia_core::ValidateExtensionManifestRequest>(request).await {
            Ok(payload) => payload,
            Err(error) => {
                let rpc_server = state.read().await;
                let next_state = rpc_next_state(&rpc_server);
                return error.into_response(next_state);
            }
        };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.validate_extension(payload) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_validate_extension_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_update_extension(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<UpdateExtensionRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    let requester = match parse_optional_core_actor(payload.requester) {
        Ok(requester) => requester,
        Err(error) => return rpc_error_response(next_state, error),
    };
    match rpc_server.update_extension(atelia_core::UpdateExtensionRequest {
        manifest: payload.manifest,
        approve_local_unsigned: payload.approve_local_unsigned,
        allow_local_process_runtime: payload.allow_local_process_runtime,
        approve_source_change: payload.approve_source_change,
        requester,
        request_source: package_http_request_source(payload.request_source),
        reason: payload.reason,
    }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_update_extension_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_project_status(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<GetProjectStatusRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let parsed = parse_get_project_status_payload(payload);
    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.get_project_status(parsed) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_project_status_response(response))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_list_extensions(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<atelia_core::ListExtensionsRequest>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.list_extensions(payload) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_list_extensions_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_list_package_trust_index(
    state: RpcServerState,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<rpc::ListPackageTrustIndexRequest>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.list_package_trust_index(payload) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(
                serialize_list_package_trust_index_response(response),
            )),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

/// Dispatch the package authoring-flow HTTP endpoint to the RPC server.
async fn dispatch_package_authoring_flow(
    state: RpcServerState,
    extension_id: String,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<PackageAuthoringFlowRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };
    let package_id = match package_id_from_path_and_payload(extension_id, payload.package_id) {
        Ok(package_id) => package_id,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.package_authoring_flow(rpc::PackageAuthoringFlowRequest {
        package_id,
        include_private_steps: payload.include_private_steps.unwrap_or(false),
    }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_package_authoring_flow_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

/// Dispatch the package remix preview HTTP endpoint to the RPC server.
async fn dispatch_package_remix(
    state: RpcServerState,
    extension_id: String,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<PackageRemixRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };
    let package_id = match package_id_from_path_and_payload(extension_id, payload.package_id) {
        Ok(package_id) => package_id,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };

    if payload.source.is_some() && payload.source_class.is_none() {
        let rpc_server = state.read().await;
        let next_state = rpc_next_state(&rpc_server);
        return make_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            "source_class is required when source is provided",
            false,
            next_state,
        );
    }
    if payload.source.is_some()
        && payload.source_class.is_some()
        && payload.source_class != Some(rpc::PackageSourceClass::UserSelected)
    {
        let rpc_server = state.read().await;
        let next_state = rpc_next_state(&rpc_server);
        return make_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            "github source cannot be paired with a non-user-selected source_class",
            false,
            next_state,
        );
    }
    if matches!(
        payload.source_class,
        Some(rpc::PackageSourceClass::UserSelected)
    ) && payload.source.is_none()
    {
        let rpc_server = state.read().await;
        let next_state = rpc_next_state(&rpc_server);
        return make_error_response(
            StatusCode::BAD_REQUEST,
            "invalid_argument",
            "github source is required when source_class is user-selected",
            false,
            next_state,
        );
    }

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.remix_package(rpc::PackageRemixRequest {
        package_id,
        source_class: payload
            .source_class
            .unwrap_or(rpc::PackageSourceClass::WorkspaceLocal),
        source: payload.source,
    }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_package_remix_response(response))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

/// Dispatch the package publication HTTP endpoint to the RPC server.
async fn dispatch_package_publication(
    state: RpcServerState,
    extension_id: String,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<PackagePublicationRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };
    let package_id = match package_id_from_path_and_payload(extension_id, payload.package_id) {
        Ok(package_id) => package_id,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.prepare_package_publication(rpc::PackagePublicationRequest {
        package_id,
        visibility: payload.visibility,
        requires_registry_submission: payload.requires_registry_submission,
        requester: parse_optional_rpc_actor(payload.requester),
        request_source: package_http_request_source(payload.request_source),
        reason: payload.reason,
    }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_package_publication_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

/// Dispatch the package registry-submission HTTP endpoint to the RPC server.
async fn dispatch_package_registry_submission(
    state: RpcServerState,
    extension_id: String,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<PackageRegistrySubmissionRequestPayload>(request).await
    {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };
    let package_id = match package_id_from_path_and_payload(extension_id, payload.package_id) {
        Ok(package_id) => package_id,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };
    let registry_identity = match payload.registry_identity {
        Some(registry_identity) => {
            let trimmed = registry_identity.trim();
            if trimmed.is_empty() {
                let rpc_server = state.read().await;
                let next_state = rpc_next_state(&rpc_server);
                return make_error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_argument",
                    "registry_identity must not be blank",
                    false,
                    next_state,
                );
            }
            if trimmed != registry_identity {
                let rpc_server = state.read().await;
                let next_state = rpc_next_state(&rpc_server);
                return make_error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_argument",
                    "registry_identity must not have leading or trailing whitespace",
                    false,
                    next_state,
                );
            }
            Some(registry_identity)
        }
        None => None,
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.submit_package_registry_submission(rpc::PackageRegistrySubmissionRequest {
        package_id,
        state: payload
            .state
            .unwrap_or(rpc::PackageRegistrySubmissionState::Submitted),
        registry_identity,
        requester: parse_optional_rpc_actor(payload.requester),
        request_source: package_http_request_source(payload.request_source),
        reason: payload.reason,
    }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(
                serialize_package_registry_submission_response(response),
            )),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_render_tool_output(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<RenderToolOutputRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let parsed = match parse_render_tool_output_payload(payload) {
        Ok(request) => request,
        Err(reason) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return make_error_response(
                StatusCode::BAD_REQUEST,
                "invalid_argument",
                reason,
                false,
                next_state,
            );
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.render_tool_output(parsed) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_render_tool_output_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_extension_status(
    state: RpcServerState,
    extension_id: String,
    request: Request<Body>,
) -> Response {
    if let Err(error) = body_or_empty_json::<serde_json::Value>(request).await {
        let rpc_server = state.read().await;
        let next_state = rpc_next_state(&rpc_server);
        return error.into_response(next_state);
    }

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.extension_status(atelia_core::ExtensionStatusRequest { extension_id }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_extension_status_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

/// Handles package inspect HTTP requests by delegating to the RPC boundary.
async fn dispatch_package_inspect(
    state: RpcServerState,
    extension_id: String,
    request: Request<Body>,
) -> Response {
    if let Err(error) = body_or_empty_json::<EmptyRequestPayload>(request).await {
        let rpc_server = state.read().await;
        let next_state = rpc_next_state(&rpc_server);
        return error.into_response(next_state);
    }

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.package_inspect(atelia_core::ExtensionStatusRequest { extension_id }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_package_inspect_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_rollback_extension(
    state: RpcServerState,
    extension_id: String,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<PackageMutationAuditPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };
    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    let requester = match parse_optional_core_actor(payload.requester) {
        Ok(requester) => requester,
        Err(error) => return rpc_error_response(next_state, error),
    };
    match rpc_server.rollback_extension(atelia_core::RollbackExtensionRequest {
        extension_id,
        requester,
        request_source: package_http_request_source(payload.request_source),
        reason: payload.reason,
    }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_rollback_extension_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_disable_extension(
    state: RpcServerState,
    extension_id: String,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<PackageMutationAuditPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };
    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    let requester = match parse_optional_core_actor(payload.requester) {
        Ok(requester) => requester,
        Err(error) => return rpc_error_response(next_state, error),
    };
    match rpc_server.disable_extension(atelia_core::DisableExtensionRequest {
        extension_id,
        requester,
        request_source: package_http_request_source(payload.request_source),
        reason: payload.reason,
    }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_disable_extension_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_enable_extension(
    state: RpcServerState,
    extension_id: String,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<PackageMutationAuditPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };
    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    let requester = match parse_optional_core_actor(payload.requester) {
        Ok(requester) => requester,
        Err(error) => return rpc_error_response(next_state, error),
    };
    match rpc_server.enable_extension(atelia_core::EnableExtensionRequest {
        extension_id,
        requester,
        request_source: package_http_request_source(payload.request_source),
        reason: payload.reason,
    }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_enable_extension_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_remove_extension(
    state: RpcServerState,
    extension_id: String,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<PackageMutationAuditPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };
    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    let requester = match parse_optional_core_actor(payload.requester) {
        Ok(requester) => requester,
        Err(error) => return rpc_error_response(next_state, error),
    };
    match rpc_server.remove_extension(atelia_core::RemoveExtensionRequest {
        extension_id,
        requester,
        request_source: package_http_request_source(payload.request_source),
        reason: payload.reason,
    }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_remove_extension_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_apply_blocklist(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<ApplyBlocklistRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    let requester = match parse_optional_core_actor(payload.requester) {
        Ok(requester) => requester,
        Err(error) => return rpc_error_response(next_state, error),
    };
    match rpc_server.apply_blocklist(atelia_core::ApplyBlocklistRequest {
        entry: payload.entry,
        requester,
        request_source: package_http_request_source(payload.request_source),
        reason: payload.reason,
    }) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_apply_blocklist_response(
                response,
            ))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_list_blocklist(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<atelia_core::ListBlocklistRequest>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.list_blocklist(payload) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(serialize_list_blocklist_response(response))),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}

async fn dispatch_list_extension_registry_audit_records(
    state: RpcServerState,
    request: Request<Body>,
) -> Response {
    let payload = match body_or_empty_json::<ListPackageAuditRequestPayload>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.list_extension_registry_audit_records(
        rpc::ListExtensionRegistryAuditRecordsRequest {
            limit: payload.limit,
            offset: payload.offset,
            cursor: payload.cursor,
        },
    ) {
        Ok(response) => (
            StatusCode::OK,
            Json(ApiResponse::ok(
                serialize_list_extension_registry_audit_records_response(response),
            )),
        )
            .into_response(),
        Err(error) => {
            let (status, recoverable) = rpc_error_status(error.code);
            make_error_response(status, "rpc_error", error.reason, recoverable, next_state)
        }
    }
}
async fn dispatch_route(State(state): State<RpcServerState>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();

    with_request_timeout(state.clone(), path.clone(), REQUEST_TIMEOUT, async move {
        match route_for_path(&path) {
            Route::Health => {
                if method != Method::GET {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("GET"));
                    response
                } else {
                    dispatch_health(state).await
                }
            }
            Route::SubmitJob => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_submit_job(state, request).await
                }
            }
            Route::ListJobs => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_list_jobs(state, request).await
                }
            }
            Route::GetJob { job_id } => {
                if method != Method::GET {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("GET"));
                    response
                } else {
                    dispatch_get_job(state, job_id).await
                }
            }
            Route::ListJobEvents { job_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_list_job_events(state, job_id, request).await
                }
            }
            Route::CancelJob { job_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_cancel_job(state, job_id, request).await
                }
            }
            Route::ListRepositories => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_list_repositories(state, request).await
                }
            }
            Route::RegisterRepository => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_register_repository(state, request).await
                }
            }
            Route::ListRepertoire => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_list_repertoire(state, request).await
                }
            }
            Route::ListEvents => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_list_events(state, request).await
                }
            }
            Route::WatchEvents => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_watch_events(state, request).await
                }
            }
            Route::ReplayEvents => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_replay_events(state, request).await
                }
            }
            Route::GetToolOutputDefaults => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_get_tool_output_defaults(state, request).await
                }
            }
            Route::UpdateToolOutputDefaults => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_update_tool_output_defaults(state, request).await
                }
            }
            Route::ListToolOutputSettingsHistory => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_list_tool_output_settings_history(state, request).await
                }
            }
            Route::InstallExtension => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_install_extension(state, request).await
                }
            }
            Route::ValidateExtension => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_validate_extension(state, request).await
                }
            }
            Route::UpdateExtension => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_update_extension(state, request).await
                }
            }
            Route::ListExtensions => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_list_extensions(state, request).await
                }
            }
            Route::ListPackageTrustIndex => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_list_package_trust_index(state, request).await
                }
            }
            Route::PackageAuthoringFlow { extension_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_package_authoring_flow(state, extension_id, request).await
                }
            }
            Route::PackageRemix { extension_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_package_remix(state, extension_id, request).await
                }
            }
            Route::PackagePublication { extension_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_package_publication(state, extension_id, request).await
                }
            }
            Route::PackageRegistrySubmission { extension_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_package_registry_submission(state, extension_id, request).await
                }
            }
            Route::ExtensionExecution { extension_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    let rpc_server = state.read().await;
                    let next_state = rpc_next_state(&rpc_server);
                    match rpc_server
                        .execute_extension(rpc::ExtensionExecutionRequest { extension_id })
                    {
                        Ok(response) => Json(ApiResponse::ok(
                            serialize_extension_execution_response(response),
                        ))
                        .into_response(),
                        Err(error) => {
                            let (status, recoverable) = rpc_error_status(error.code);
                            make_error_response(
                                status,
                                "rpc_error",
                                error.reason,
                                recoverable,
                                next_state,
                            )
                        }
                    }
                }
            }
            Route::RenderToolOutput => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_render_tool_output(state, request).await
                }
            }
            Route::ExtensionStatus { extension_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_extension_status(state, extension_id, request).await
                }
            }
            Route::PackageInspect { extension_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_package_inspect(state, extension_id, request).await
                }
            }
            Route::RollbackExtension { extension_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_rollback_extension(state, extension_id, request).await
                }
            }
            Route::DisableExtension { extension_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_disable_extension(state, extension_id, request).await
                }
            }
            Route::EnableExtension { extension_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_enable_extension(state, extension_id, request).await
                }
            }
            Route::RemoveExtension { extension_id } => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_remove_extension(state, extension_id, request).await
                }
            }
            Route::ApplyBlocklist => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_apply_blocklist(state, request).await
                }
            }
            Route::ListBlocklist => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_list_blocklist(state, request).await
                }
            }
            Route::ListExtensionRegistryAuditRecords => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_list_extension_registry_audit_records(state, request).await
                }
            }
            Route::ProjectStatus => {
                if method != Method::POST {
                    let mut response = make_error_response(
                        StatusCode::METHOD_NOT_ALLOWED,
                        "method_not_allowed",
                        format!("{} is not supported on {path}", method),
                        false,
                        {
                            let rpc_server = state.read().await;
                            rpc_next_state(&rpc_server)
                        },
                    );
                    response
                        .headers_mut()
                        .insert(header::ALLOW, header::HeaderValue::from_static("POST"));
                    response
                } else {
                    dispatch_project_status(state, request).await
                }
            }
            Route::Unsupported => make_error_response(
                StatusCode::NOT_FOUND,
                "unsupported_endpoint",
                format!("{path} is not a supported endpoint"),
                false,
                {
                    let rpc_server = state.read().await;
                    rpc_next_state(&rpc_server)
                },
            ),
        }
    })
    .await
}

async fn fallback_route(State(state): State<RpcServerState>, request: Request<Body>) -> Response {
    let path = request.uri().path().to_string();
    with_request_timeout(state.clone(), path.clone(), REQUEST_TIMEOUT, async move {
        make_error_response(
            StatusCode::NOT_FOUND,
            "unsupported_endpoint",
            format!("{path} is not a supported endpoint"),
            false,
            {
                let rpc_server = state.read().await;
                rpc_next_state(&rpc_server)
            },
        )
    })
    .await
}

/// Build the daemon router with the selected local auth boundary.
pub fn build_router(rpc_server: RpcServerState, auth: LocalAuthConfig) -> Router {
    let auth_layer = middleware::from_fn_with_state(auth, local_auth_middleware);

    Router::new()
        .route("/v1/health", any(dispatch_route))
        .route("/v1/jobs/submit", any(dispatch_route))
        .route("/v1/jobs/list", any(dispatch_route))
        .route("/v1/jobs/{*path}", any(dispatch_route))
        .route("/v1/repositories:list", any(dispatch_route))
        .route("/v1/repositories:register", any(dispatch_route))
        .route("/v1/repertoire:list", any(dispatch_route))
        .route("/v1/events/list", any(dispatch_route))
        .route("/v1/events/watch", any(dispatch_route))
        .route("/v1/events/replay", any(dispatch_route))
        .route("/v1/tool-output/settings/get", any(dispatch_route))
        .route("/v1/tool-output/settings/update", any(dispatch_route))
        .route("/v1/tool-output/settings/history:list", any(dispatch_route))
        .route("/v1/packages/install", any(dispatch_route))
        .route("/v1/packages/validate", any(dispatch_route))
        .route("/v1/packages/update", any(dispatch_route))
        .route("/v1/packages/list", any(dispatch_route))
        .route("/v1/package-trust-index:list", any(dispatch_route))
        .route("/v1/packages/blocklist/apply", any(dispatch_route))
        .route("/v1/packages/blocklist/list", any(dispatch_route))
        .route("/v1/packages/{*path}", any(dispatch_route))
        .route("/v1/tool-results:render", any(dispatch_route))
        .route("/v1/project-status:get", any(dispatch_route))
        .fallback(fallback_route)
        .layer(auth_layer)
        .with_state(rpc_server)
}

/// Bind the daemon TCP listener to the already validated address.
pub async fn bind_listener(listen_addr: SocketAddr) -> Result<TcpListener> {
    TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind {listen_addr}"))
}

/// Run the daemon listener with the selected local auth boundary.
pub async fn run_listener(
    rpc_server: RpcServerState,
    auth: LocalAuthConfig,
    listener: TcpListener,
    shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    validate_local_auth_binding(&auth, &listener.local_addr()?)?;
    axum::serve(listener, build_router(rpc_server, auth))
        .with_graceful_shutdown(async move {
            let _ = shutdown.await;
        })
        .await
        .context("daemon listener failed")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service;
    use atelia_core::{
        BlockKey, BlockReason, DegradeBehavior, EventRefs, EventSeverity, EventSubject,
        ExtensionCompatibility, ExtensionEntrypoints, ExtensionFailure, ExtensionKind,
        ExtensionManifest, ExtensionPermission, ExtensionPublisher, ExtensionRealm,
        ExtensionRuntime, ExtensionServices, JobEvent, JobEventId, JobEventKind, LedgerTimestamp,
        ProvenanceSource, RepositoryId, RetryPolicy, EXTENSION_MANIFEST_SCHEMA,
        EXTENSION_RPC_PROTOCOL,
    };
    use axum::{http::StatusCode, Router};
    use serde_json::Value;
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::fs;
    use std::io::ErrorKind;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, MutexGuard};
    use tower::util::ServiceExt;

    static LISTEN_ADDR_ENV_MUTEX: Mutex<()> = Mutex::new(());
    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    struct ListenAddrEnvGuard {
        previous: Option<OsString>,
        previous_unsafe_allow_non_loopback_listen: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl ListenAddrEnvGuard {
        fn lock() -> Self {
            let lock = LISTEN_ADDR_ENV_MUTEX.lock().unwrap();
            let previous = std::env::var_os(LISTEN_ADDR_ENV);
            let previous_unsafe_allow_non_loopback_listen =
                std::env::var_os(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV);
            Self {
                previous,
                previous_unsafe_allow_non_loopback_listen,
                _lock: lock,
            }
        }
    }

    impl Drop for ListenAddrEnvGuard {
        fn drop(&mut self) {
            match self.previous.as_ref() {
                Some(value) => std::env::set_var(LISTEN_ADDR_ENV, value),
                None => std::env::remove_var(LISTEN_ADDR_ENV),
            }
            match self.previous_unsafe_allow_non_loopback_listen.as_ref() {
                Some(value) => std::env::set_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV, value),
                None => std::env::remove_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV),
            }
        }
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

    struct UnsafeAllowNonLoopbackListenEnvGuard {
        previous: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl UnsafeAllowNonLoopbackListenEnvGuard {
        fn lock() -> Self {
            let lock = LISTEN_ADDR_ENV_MUTEX.lock().unwrap();
            let previous = std::env::var_os(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV);
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for UnsafeAllowNonLoopbackListenEnvGuard {
        fn drop(&mut self) {
            match self.previous.as_ref() {
                Some(value) => std::env::set_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV, value),
                None => std::env::remove_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV),
            }
        }
    }

    struct LocalAuthEnvGuard {
        previous_auth_disabled: Option<OsString>,
        previous_auth_token: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl LocalAuthEnvGuard {
        fn lock() -> Self {
            let lock = LISTEN_ADDR_ENV_MUTEX.lock().unwrap();
            let previous_auth_disabled = std::env::var_os(AUTH_DISABLED_ENV);
            let previous_auth_token = std::env::var_os(AUTH_TOKEN_ENV);
            Self {
                previous_auth_disabled,
                previous_auth_token,
                _lock: lock,
            }
        }
    }

    impl Drop for LocalAuthEnvGuard {
        fn drop(&mut self) {
            match self.previous_auth_disabled.as_ref() {
                Some(value) => std::env::set_var(AUTH_DISABLED_ENV, value),
                None => std::env::remove_var(AUTH_DISABLED_ENV),
            }
            match self.previous_auth_token.as_ref() {
                Some(value) => std::env::set_var(AUTH_TOKEN_ENV, value),
                None => std::env::remove_var(AUTH_TOKEN_ENV),
            }
        }
    }

    fn test_repo_dir(name: &str) -> std::path::PathBuf {
        let id = TEST_COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "atelia-transport-test-{}-{}-{name}",
            std::process::id(),
            id
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    fn ready_rpc_server() -> RpcServerState {
        let mut service = service::SecretaryService::new();
        service.set_ready();
        Arc::new(RwLock::new(rpc::SecretaryRpcServer::new(service)))
    }

    fn disabled_auth() -> LocalAuthConfig {
        LocalAuthConfig::Disabled
    }

    fn bearer_auth(token: &str) -> LocalAuthConfig {
        LocalAuthConfig::BearerToken {
            token: token.to_string(),
        }
    }

    #[test]
    fn local_auth_config_debug_redacts_token() {
        let debug = format!("{:?}", bearer_auth("super-secret-token"));
        assert!(!debug.contains("super-secret-token"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn bearer_token_matches_rejects_different_tokens() {
        assert!(bearer_token_matches(
            "super-secret-token",
            "super-secret-token"
        ));
        assert!(!bearer_token_matches(
            "super-secret-token",
            "super-secret-t0ken"
        ));
        assert!(!bearer_token_matches("super-secret-token", "super-secret"));
    }

    fn test_router(state: &RpcServerState) -> Router {
        build_router(state.clone(), disabled_auth())
    }

    async fn send_request(state: &RpcServerState, method: Method, path: &str) -> Response {
        let app = test_router(state);
        app.oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .body(Body::empty())
                .expect("valid request"),
        )
        .await
        .expect("request should succeed")
    }

    async fn send_json_request(
        state: &RpcServerState,
        method: Method,
        path: &str,
        body: serde_json::Value,
    ) -> Response {
        let app = test_router(state);
        app.oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(serde_json::to_vec(&body).expect("json body")))
                .expect("valid request"),
        )
        .await
        .expect("request should succeed")
    }

    async fn send_authenticated_request(
        state: &RpcServerState,
        method: Method,
        path: &str,
        token: &str,
    ) -> Response {
        let app = build_router(state.clone(), bearer_auth(token));
        app.oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .expect("valid request"),
        )
        .await
        .expect("request should succeed")
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
                degrade: DegradeBehavior::ReturnUnavailable,
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

    /// Verifies package inspect paths map only at the supported route depth.
    #[test]
    fn route_parser_distinguishes_supported_endpoints() {
        let job_id = JobId::new().as_str().to_string();
        assert_eq!(route_for_path("/v1/health"), Route::Health);
        assert_eq!(route_for_path("/v1/jobs/submit"), Route::SubmitJob);
        assert_eq!(route_for_path("/v1/jobs/list"), Route::ListJobs);
        assert_eq!(
            route_for_path(&format!("/v1/jobs/{job_id}")),
            Route::GetJob {
                job_id: job_id.clone()
            }
        );
        assert_eq!(
            route_for_path(&format!("/v1/jobs/{job_id}/events")),
            Route::ListJobEvents {
                job_id: job_id.clone()
            }
        );
        assert_eq!(
            route_for_path(&format!("/v1/jobs/{job_id}/cancel")),
            Route::CancelJob {
                job_id: job_id.clone()
            }
        );
        assert_eq!(
            route_for_path("/v1/repositories:list"),
            Route::ListRepositories
        );
        assert_eq!(
            route_for_path("/v1/repositories:register"),
            Route::RegisterRepository
        );
        assert_eq!(route_for_path("/v1/repertoire:list"), Route::ListRepertoire);
        assert_eq!(route_for_path("/v1/events/list"), Route::ListEvents);
        assert_eq!(route_for_path("/v1/events/watch"), Route::WatchEvents);
        assert_eq!(route_for_path("/v1/events/replay"), Route::ReplayEvents);
        assert_eq!(
            route_for_path("/v1/tool-output/settings/get"),
            Route::GetToolOutputDefaults
        );
        assert_eq!(
            route_for_path("/v1/tool-output/settings/update"),
            Route::UpdateToolOutputDefaults
        );
        assert_eq!(
            route_for_path("/v1/tool-output/settings/history:list"),
            Route::ListToolOutputSettingsHistory
        );
        assert_eq!(
            route_for_path("/v1/packages/install"),
            Route::InstallExtension
        );
        assert_eq!(
            route_for_path("/v1/packages/validate"),
            Route::ValidateExtension
        );
        assert_eq!(
            route_for_path("/v1/packages/update"),
            Route::UpdateExtension
        );
        assert_eq!(route_for_path("/v1/packages/list"), Route::ListExtensions);
        assert_eq!(
            route_for_path("/v1/package-trust-index:list"),
            Route::ListPackageTrustIndex
        );
        assert_eq!(
            route_for_path("/v1/packages/blocklist/apply"),
            Route::ApplyBlocklist
        );
        assert_eq!(
            route_for_path("/v1/packages/blocklist/list"),
            Route::ListBlocklist
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/authoring-flow"),
            Route::PackageAuthoringFlow {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/remix"),
            Route::PackageRemix {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/publication"),
            Route::PackagePublication {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/registry-submission"),
            Route::PackageRegistrySubmission {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/status"),
            Route::ExtensionStatus {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/inspect"),
            Route::PackageInspect {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/rollback"),
            Route::RollbackExtension {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/disable"),
            Route::DisableExtension {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/enable"),
            Route::EnableExtension {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/remove"),
            Route::RemoveExtension {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/execute"),
            Route::ExtensionExecution {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/inspect/settings"),
            Route::Unsupported
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/inspect/"),
            Route::Unsupported
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/invoke"),
            Route::Unsupported
        );
        assert_eq!(
            route_for_path("/v1/packages/com.example.extension/run"),
            Route::Unsupported
        );
        assert_eq!(route_for_path("/v1/packages//status"), Route::Unsupported);
        assert_eq!(
            route_for_path("/v1/packages/a/b/status"),
            Route::Unsupported
        );
        assert_eq!(route_for_path("/v1/packages//rollback"), Route::Unsupported);
        assert_eq!(
            route_for_path("/v1/packages/a/b/rollback"),
            Route::Unsupported
        );
        assert_eq!(
            route_for_path("/v1/tool-results:render"),
            Route::RenderToolOutput
        );
        assert_eq!(
            route_for_path("/v1/project-status:get"),
            Route::ProjectStatus
        );
        assert_eq!(route_for_path("/unknown"), Route::Unsupported);
        assert_eq!(route_for_path("/v1/health/"), Route::Unsupported);
        assert_eq!(route_for_path("/v1/jobs//cancel"), Route::Unsupported);
        assert_eq!(route_for_path("/v1/jobs/a/b/cancel"), Route::Unsupported);
        assert_eq!(
            route_for_path("/v1/jobs/not-a-job-id/cancel"),
            Route::Unsupported
        );
    }

    #[tokio::test]
    async fn request_timeout_returns_structured_gateway_timeout_error() {
        let state = ready_rpc_server();
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            with_request_timeout(
                state,
                "/v1/health".to_string(),
                Duration::from_millis(25),
                std::future::pending::<Response>(),
            ),
        )
        .await
        .expect("timed request should complete")
        .into_response();

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let json: Value = serde_json::from_slice(&body).expect("timeout response json");
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "request_timeout");
        assert_eq!(json["error"]["recoverable"], true);
        assert_eq!(
            json["error"]["reason"],
            "request to /v1/health exceeded 25ms"
        );
    }

    #[tokio::test]
    async fn local_auth_rejects_missing_authorization_header() {
        let rpc_server = ready_rpc_server();
        let app = build_router(rpc_server, bearer_auth("test-token"));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/v1/health")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(header::WWW_AUTHENTICATE),
            Some(&header::HeaderValue::from_static(
                r#"Bearer realm="Atelia Secretary""#
            ))
        );
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let json: Value = serde_json::from_slice(&body).expect("response json");
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn local_auth_rejects_invalid_authorization_header() {
        let rpc_server = ready_rpc_server();
        let app = build_router(rpc_server, bearer_auth("test-token"));

        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/v1/health")
                    .header(header::AUTHORIZATION, "Bearer wrong-token")
                    .body(Body::empty())
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let json: Value = serde_json::from_slice(&body).expect("response json");
        assert_eq!(json["status"], "error");
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn local_auth_allows_valid_authorization_header() {
        let rpc_server = ready_rpc_server();
        let response =
            send_authenticated_request(&rpc_server, Method::GET, "/v1/health", "test-token").await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        let json: Value = serde_json::from_slice(&body).expect("response json");
        assert_eq!(json["status"], "ok");
        assert_eq!(json["data"]["daemon_status"], "ready");
    }

    #[cfg(unix)]
    #[test]
    fn resolve_local_auth_creates_and_reuses_session_token() {
        let _guard = LocalAuthEnvGuard::lock();
        std::env::remove_var(AUTH_DISABLED_ENV);
        std::env::remove_var(AUTH_TOKEN_ENV);

        let storage_dir = test_repo_dir("local-auth");
        let resolved = resolve_local_auth(&storage_dir).expect("resolve local auth");
        let token_path = local_auth_token_path(&storage_dir);
        let token = fs::read_to_string(&token_path).expect("session token file");

        match resolved {
            LocalAuthConfig::BearerToken {
                token: resolved_token,
            } => {
                assert_eq!(resolved_token, token);
                assert_eq!(resolved_token.len(), 64);
            }
            LocalAuthConfig::Disabled => panic!("expected bearer token"),
        }

        let resolved_again = resolve_local_auth(&storage_dir).expect("resolve local auth again");
        assert_eq!(resolved_again, LocalAuthConfig::BearerToken { token });
    }

    #[cfg(unix)]
    #[test]
    fn resolve_local_auth_creates_restrictive_storage_dir() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = LocalAuthEnvGuard::lock();
        std::env::remove_var(AUTH_DISABLED_ENV);
        std::env::remove_var(AUTH_TOKEN_ENV);

        let storage_dir = test_repo_dir("local-auth-fresh-storage").join("auth");
        assert!(!storage_dir.exists());

        resolve_local_auth(&storage_dir).expect("resolve local auth");

        let mode = fs::metadata(&storage_dir)
            .expect("auth storage dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_local_auth_normalizes_existing_storage_dir_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = LocalAuthEnvGuard::lock();
        std::env::remove_var(AUTH_DISABLED_ENV);
        std::env::remove_var(AUTH_TOKEN_ENV);

        let storage_dir = test_repo_dir("local-auth-storage-permissions");
        let permissions = fs::Permissions::from_mode(0o755);
        fs::set_permissions(&storage_dir, permissions).expect("seed auth storage dir permissions");

        resolve_local_auth(&storage_dir).expect("resolve local auth");

        let mode = fs::metadata(&storage_dir)
            .expect("auth storage dir metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn local_auth_write_or_reuse_session_token_recovers_from_already_exists() {
        let storage_dir = test_repo_dir("local-auth-already-exists");
        let token_path = local_auth_token_path(&storage_dir);
        let existing_token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        fs::write(&token_path, existing_token).expect("seed session token");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = fs::metadata(&token_path)
                .expect("seed token metadata")
                .permissions();
            permissions.set_mode(0o644);
            fs::set_permissions(&token_path, permissions).expect("seed token permissions");
        }

        let recovered = write_or_reuse_session_token(&token_path, "f".repeat(64))
            .expect("recover existing session token");

        assert_eq!(recovered, existing_token);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = fs::metadata(&token_path)
                .expect("session token metadata")
                .permissions()
                .mode()
                & 0o777;

            assert_eq!(mode, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn local_auth_write_or_reuse_session_token_retries_after_transient_invalid_read() {
        let storage_dir = test_repo_dir("local-auth-already-exists-transient");
        let token_path = local_auth_token_path(&storage_dir);
        fs::write(&token_path, "broken-token").expect("seed invalid session token");

        let repaired_token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let writer_token_path = token_path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            fs::write(&writer_token_path, repaired_token).expect("repair session token");
        });

        let recovered = write_or_reuse_session_token(&token_path, "f".repeat(64))
            .expect("recover repaired session token");

        writer.join().expect("repair thread");
        assert_eq!(recovered, repaired_token);
    }

    #[cfg(unix)]
    #[test]
    fn resolve_local_auth_retries_when_existing_token_is_repaired_during_read() {
        let _guard = LocalAuthEnvGuard::lock();
        std::env::remove_var(AUTH_DISABLED_ENV);
        std::env::remove_var(AUTH_TOKEN_ENV);

        let storage_dir = test_repo_dir("local-auth-read-retry");
        let token_path = local_auth_token_path(&storage_dir);
        fs::write(&token_path, "broken-token").expect("seed invalid session token");

        let repaired_token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let writer_token_path = token_path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            fs::write(&writer_token_path, repaired_token).expect("repair session token");
        });

        let resolved = resolve_local_auth(&storage_dir).expect("resolve repaired local auth");

        writer.join().expect("repair thread");
        assert_eq!(
            resolved,
            LocalAuthConfig::BearerToken {
                token: repaired_token.to_string(),
            }
        );
    }

    #[test]
    fn resolve_local_auth_rejects_bearer_token_with_internal_whitespace() {
        let _guard = LocalAuthEnvGuard::lock();
        std::env::remove_var(AUTH_DISABLED_ENV);
        std::env::set_var(AUTH_TOKEN_ENV, "abc def");

        let err = resolve_local_auth(&test_repo_dir("local-auth-env-token"))
            .expect_err("internal whitespace should be rejected");

        assert!(err
            .to_string()
            .contains("ATELIA_DAEMON_AUTH_TOKEN must not contain internal whitespace"));
    }

    #[test]
    fn resolve_local_auth_rejects_bearer_token_with_boundary_whitespace() {
        let _guard = LocalAuthEnvGuard::lock();
        std::env::remove_var(AUTH_DISABLED_ENV);

        for token in [" abcdef", "abcdef "] {
            std::env::set_var(AUTH_TOKEN_ENV, token);

            let err = resolve_local_auth(&test_repo_dir("local-auth-env-token-boundary"))
                .expect_err("boundary whitespace should be rejected");

            assert!(err
                .to_string()
                .contains("ATELIA_DAEMON_AUTH_TOKEN must not have leading or trailing whitespace"));
        }
    }

    #[test]
    fn resolve_local_auth_rejects_bearer_token_with_non_visible_ascii() {
        let _guard = LocalAuthEnvGuard::lock();
        std::env::remove_var(AUTH_DISABLED_ENV);

        for (label, token) in [("control", "abc\u{0007}def"), ("non_ascii", "abcédef")] {
            std::env::set_var(AUTH_TOKEN_ENV, token);

            let err = resolve_local_auth(&test_repo_dir(&format!("local-auth-env-token-{label}")))
                .expect_err("non-visible ASCII should be rejected");

            assert!(err
                .to_string()
                .contains("ATELIA_DAEMON_AUTH_TOKEN must contain only visible ASCII characters"));
        }
    }

    #[test]
    fn resolve_local_auth_rejects_weak_pinned_token() {
        let _guard = LocalAuthEnvGuard::lock();
        std::env::remove_var(AUTH_DISABLED_ENV);
        std::env::set_var(AUTH_TOKEN_ENV, "abcdefghijklmnopqrstuvwxyz0123456789-_abcd");

        let err = resolve_local_auth(&test_repo_dir("local-auth-env-weak-token"))
            .expect_err("weak pinned token should be rejected");

        assert!(err.to_string().contains(
            "ATELIA_DAEMON_AUTH_TOKEN must be either exactly 64 hexadecimal characters or at least 43 base64url characters using only ASCII alphanumeric, '-' or '_'"
        ));
    }

    #[test]
    fn resolve_local_auth_accepts_valid_pinned_token() {
        let _guard = LocalAuthEnvGuard::lock();
        std::env::remove_var(AUTH_DISABLED_ENV);
        std::env::set_var(
            AUTH_TOKEN_ENV,
            "abcdefghijklmnopqrstuvwxyz0123456789-_abcde",
        );

        let resolved = resolve_local_auth(&test_repo_dir("local-auth-env-valid-token"))
            .expect("valid pinned token should be accepted");

        assert_eq!(
            resolved,
            LocalAuthConfig::BearerToken {
                token: "abcdefghijklmnopqrstuvwxyz0123456789-_abcde".to_string(),
            }
        );
    }

    #[test]
    fn resolve_local_auth_rejects_conflicting_disabled_and_token_env() {
        let _guard = LocalAuthEnvGuard::lock();
        std::env::set_var(AUTH_DISABLED_ENV, "1");
        std::env::set_var(AUTH_TOKEN_ENV, "abcdef");

        let err = resolve_local_auth(&test_repo_dir("local-auth-env-conflict"))
            .expect_err("conflicting envs should be rejected");

        assert!(err.to_string().contains(
            "ATELIA_DAEMON_AUTH_DISABLED and ATELIA_DAEMON_AUTH_TOKEN are mutually exclusive"
        ));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_local_auth_rejects_invalid_persisted_session_token_file() {
        let _guard = LocalAuthEnvGuard::lock();
        std::env::remove_var(AUTH_DISABLED_ENV);
        std::env::remove_var(AUTH_TOKEN_ENV);

        for (name, contents) in [
            ("empty", String::new()),
            ("truncated", String::from("abc123")),
            ("corrupt", "z".repeat(64)),
        ] {
            let storage_dir = test_repo_dir(&format!("local-auth-invalid-{name}"));
            let token_path = local_auth_token_path(&storage_dir);
            fs::write(&token_path, contents).expect("seed invalid session token");

            let err = resolve_local_auth(&storage_dir)
                .expect_err("invalid session token file should fail closed");

            let message = err.to_string();
            assert!(message.contains("session token file"));
            assert!(message.contains("must contain exactly 64 hexadecimal characters"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_local_auth_rejects_symlinked_session_token_file() {
        use std::os::unix::fs::symlink;

        let _guard = LocalAuthEnvGuard::lock();
        std::env::remove_var(AUTH_DISABLED_ENV);
        std::env::remove_var(AUTH_TOKEN_ENV);

        let storage_dir = test_repo_dir("local-auth-symlink");
        let token_path = local_auth_token_path(&storage_dir);
        let real_token_path = storage_dir.join("real-daemon-auth.token");
        fs::write(
            &real_token_path,
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("seed real token");
        symlink(&real_token_path, &token_path).expect("seed symlinked token");

        let err = resolve_local_auth(&storage_dir)
            .expect_err("symlinked session token file should fail closed");

        assert!(err.to_string().contains("session token file"));
        assert!(err.to_string().contains("must be a regular file"));
    }

    #[test]
    fn encode_session_token_formats_lowercase_hex() {
        let token = encode_session_token(&[0x00, 0x01, 0xab, 0xcd, 0xef, 0xff]);

        assert_eq!(token, "0001abcdefff");
        assert!(token.chars().all(|ch| ch.is_ascii_hexdigit()));
        assert!(token.chars().all(|ch| !ch.is_ascii_uppercase()));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_local_auth_enforces_restrictive_permissions_on_existing_token_file() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = LocalAuthEnvGuard::lock();
        std::env::remove_var(AUTH_DISABLED_ENV);
        std::env::remove_var(AUTH_TOKEN_ENV);

        let storage_dir = test_repo_dir("local-auth-permissions");
        let token_path = local_auth_token_path(&storage_dir);
        let token = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        fs::write(&token_path, token).expect("seed session token");
        let mut permissions = fs::metadata(&token_path)
            .expect("seed token metadata")
            .permissions();
        permissions.set_mode(0o644);
        fs::set_permissions(&token_path, permissions).expect("seed token permissions");

        let resolved = resolve_local_auth(&storage_dir).expect("resolve local auth");
        assert_eq!(
            resolved,
            LocalAuthConfig::BearerToken {
                token: token.to_string(),
            }
        );

        let token_path = local_auth_token_path(&storage_dir);
        let mode = fs::metadata(&token_path)
            .expect("session token metadata")
            .permissions()
            .mode()
            & 0o777;

        assert_eq!(mode, 0o600);
    }

    #[test]
    fn validate_local_auth_binding_rejects_auth_disabled_on_unsafe_non_loopback_listener() {
        let _guard = UnsafeAllowNonLoopbackListenEnvGuard::lock();
        std::env::set_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV, "1");
        let listen_addr = SocketAddr::from(([0, 0, 0, 0], 8080));

        let err = validate_local_auth_binding(&LocalAuthConfig::Disabled, &listen_addr)
            .expect_err("auth-disabled non-loopback listener should fail closed");

        assert!(err
            .to_string()
            .contains("refusing to combine ATELIA_DAEMON_AUTH_DISABLED=1"));
    }

    #[test]
    fn validate_local_auth_binding_rejects_auth_disabled_on_non_loopback_without_escape_hatch() {
        let _guard = UnsafeAllowNonLoopbackListenEnvGuard::lock();
        std::env::remove_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV);
        let listen_addr = SocketAddr::from(([0, 0, 0, 0], 8080));

        let err = validate_local_auth_binding(&LocalAuthConfig::Disabled, &listen_addr)
            .expect_err("auth-disabled non-loopback listener should fail closed");

        assert!(err
            .to_string()
            .contains("refusing to combine ATELIA_DAEMON_AUTH_DISABLED=1"));
    }

    #[tokio::test]
    async fn watch_events_stream_skips_replayed_duplicates() {
        let repository_id = RepositoryId::new();
        let replay_event = test_job_event(&repository_id, 1);
        let duplicate_event = test_job_event(&repository_id, 1);
        let live_event = test_job_event(&repository_id, 2);
        let metadata =
            rpc::ProtocolMetadata::from(service::SecretaryService::new().protocol_metadata());
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        sender
            .try_send(Ok(duplicate_event))
            .expect("duplicate event should broadcast");
        sender
            .try_send(Ok(live_event.clone()))
            .expect("live event should broadcast");
        drop(sender);

        let response = rpc::WatchEventsLiveResponse {
            metadata,
            events: vec![rpc::RpcEvent::from(replay_event.clone())],
            cursor: Some(rpc::EventCursorRequest::AfterSequence(
                replay_event.sequence_number,
            )),
            subscription: rpc::WatchEventsLiveSubscription {
                receiver,
                replay_max_sequence: Some(replay_event.sequence_number),
                resolved_cursor_sequence: Some(replay_event.sequence_number),
                last_sequence: Some(replay_event.sequence_number),
            },
        };
        let request = rpc::WatchEventsRequest {
            repository_id: repository_id.as_str().to_string(),
            cursor: Some(rpc::EventCursorRequest::Beginning),
            subject_ids: Vec::new(),
            min_severity: None,
            limit: Some(1),
        };

        let body = watch_events_stream_body(response, request);
        let payload = to_bytes(body, usize::MAX)
            .await
            .expect("watch events stream should serialize");
        let lines = std::str::from_utf8(&payload)
            .expect("stream should be utf8")
            .lines()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);

        let snapshot: Value = serde_json::from_str(lines[0]).expect("snapshot should parse");
        assert_eq!(snapshot["kind"], "snapshot");
        assert_eq!(snapshot["cursor"]["kind"], "after_sequence");
        assert_eq!(
            snapshot["cursor"]["sequence_number"],
            replay_event.sequence_number
        );
        assert_eq!(
            snapshot["events"]
                .as_array()
                .expect("snapshot events should be an array")
                .len(),
            1
        );

        let event_frame: Value = serde_json::from_str(lines[1]).expect("event frame should parse");
        assert_eq!(event_frame["kind"], "event");
        assert_eq!(event_frame["event"]["sequence"], 2);
    }

    #[tokio::test]
    async fn watch_events_stream_reports_filtered_terminal_cursor_expiry() {
        let repository_id = RepositoryId::new();
        let metadata =
            rpc::ProtocolMetadata::from(service::SecretaryService::new().protocol_metadata());
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        sender
            .try_send(Err(StoreError::CursorExpired {
                reason: "watch_events live filter fell behind and missed 7 events".to_string(),
            }))
            .expect("terminal error should broadcast");
        drop(sender);

        let response = rpc::WatchEventsLiveResponse {
            metadata,
            events: Vec::new(),
            cursor: Some(rpc::EventCursorRequest::Beginning),
            subscription: rpc::WatchEventsLiveSubscription {
                receiver,
                replay_max_sequence: None,
                resolved_cursor_sequence: None,
                last_sequence: None,
            },
        };
        let request = rpc::WatchEventsRequest {
            repository_id: repository_id.as_str().to_string(),
            cursor: Some(rpc::EventCursorRequest::Beginning),
            subject_ids: Vec::new(),
            min_severity: None,
            limit: Some(1),
        };

        let body = watch_events_stream_body(response, request);
        let payload = to_bytes(body, usize::MAX)
            .await
            .expect("watch events stream should serialize");
        let lines = std::str::from_utf8(&payload)
            .expect("stream should be utf8")
            .lines()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);

        let error_frame: Value = serde_json::from_str(lines[1]).expect("error frame should parse");
        assert_eq!(error_frame["kind"], "error");
        assert_eq!(error_frame["error"]["code"], "cursor_expired");
        assert_eq!(
            error_frame["error"]["reason"],
            "watch_events live filter fell behind and missed 7 events"
        );
    }

    #[tokio::test]
    async fn watch_events_stream_reports_cursor_expired_recovery() {
        let response = watch_events_cursor_expired_response("event id is not retained");
        assert_eq!(response.status(), StatusCode::GONE);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .expect("content type")
                .to_str()
                .expect("valid content type"),
            "application/x-ndjson"
        );

        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("watch events cursor expired response should serialize");
        let lines = std::str::from_utf8(&payload)
            .expect("stream should be utf8")
            .lines()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 1);

        let error_frame: Value = serde_json::from_str(lines[0]).expect("error frame should parse");
        assert_eq!(error_frame["kind"], "error");
        assert_eq!(error_frame["error"]["code"], "cursor_expired");
        assert_eq!(error_frame["error"]["recoverable"], true);
        assert_eq!(error_frame["error"]["next_state"], "refresh_status");
    }

    #[tokio::test]
    async fn watch_events_stream_allows_first_live_event_when_replay_is_empty() {
        let repository_id = RepositoryId::new();
        let live_event = test_job_event(&repository_id, 1);
        let metadata =
            rpc::ProtocolMetadata::from(service::SecretaryService::new().protocol_metadata());
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        sender
            .try_send(Ok(live_event.clone()))
            .expect("live event should broadcast");
        drop(sender);

        let response = rpc::WatchEventsLiveResponse {
            metadata,
            events: Vec::new(),
            cursor: Some(rpc::EventCursorRequest::Beginning),
            subscription: rpc::WatchEventsLiveSubscription {
                receiver,
                replay_max_sequence: None,
                resolved_cursor_sequence: None,
                last_sequence: None,
            },
        };
        let request = rpc::WatchEventsRequest {
            repository_id: repository_id.as_str().to_string(),
            cursor: Some(rpc::EventCursorRequest::Beginning),
            subject_ids: Vec::new(),
            min_severity: None,
            limit: Some(1),
        };

        let body = watch_events_stream_body(response, request);
        let payload = to_bytes(body, usize::MAX)
            .await
            .expect("watch events stream should serialize");
        let lines = std::str::from_utf8(&payload)
            .expect("stream should be utf8")
            .lines()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);

        let event_frame: Value = serde_json::from_str(lines[1]).expect("event frame should parse");
        assert_eq!(event_frame["kind"], "event");
        assert_eq!(event_frame["event"]["sequence"], 1);
    }

    #[tokio::test]
    async fn watch_events_stream_skips_events_before_resolved_cursor_sequence() {
        let repository_id = RepositoryId::new();
        let anchor_event = test_job_event(&repository_id, 1);
        let live_event = test_job_event(&repository_id, 2);
        let metadata =
            rpc::ProtocolMetadata::from(service::SecretaryService::new().protocol_metadata());
        let (sender, receiver) = tokio::sync::mpsc::channel(8);
        sender
            .try_send(Ok(anchor_event.clone()))
            .expect("anchor event should broadcast");
        sender
            .try_send(Ok(live_event.clone()))
            .expect("live event should broadcast");
        drop(sender);

        let response = rpc::WatchEventsLiveResponse {
            metadata,
            events: Vec::new(),
            cursor: Some(rpc::EventCursorRequest::Beginning),
            subscription: rpc::WatchEventsLiveSubscription {
                receiver,
                replay_max_sequence: None,
                resolved_cursor_sequence: Some(anchor_event.sequence_number),
                last_sequence: Some(anchor_event.sequence_number),
            },
        };
        let request = rpc::WatchEventsRequest {
            repository_id: repository_id.as_str().to_string(),
            cursor: Some(rpc::EventCursorRequest::Beginning),
            subject_ids: Vec::new(),
            min_severity: None,
            limit: Some(1),
        };

        let body = watch_events_stream_body(response, request);
        let payload = to_bytes(body, usize::MAX)
            .await
            .expect("watch events stream should serialize");
        let lines = std::str::from_utf8(&payload)
            .expect("stream should be utf8")
            .lines()
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);

        let event_frame: Value = serde_json::from_str(lines[1]).expect("event frame should parse");
        assert_eq!(event_frame["kind"], "event");
        assert_eq!(event_frame["event"]["sequence"], 2);
    }

    #[test]
    fn path_scope_payload_requires_kind_when_details_are_present() {
        let err = parse_path_scope_payload(PathScopePayload {
            kind: None,
            roots: Some(vec!["src".to_string()]),
            include_patterns: None,
            exclude_patterns: None,
        })
        .expect_err("scope details without kind should fail");
        assert!(err.contains("path_scope.kind is required"));

        let parsed = parse_path_scope_payload(PathScopePayload {
            kind: None,
            roots: None,
            include_patterns: None,
            exclude_patterns: None,
        })
        .expect("empty scope can use repository default");
        assert_eq!(parsed.kind, rpc::RpcPathScopeKind::Repository);

        for kind in ["", "   "] {
            let err = parse_path_scope_payload(PathScopePayload {
                kind: Some(kind.to_string()),
                roots: Some(vec!["src".to_string()]),
                include_patterns: Some(vec!["**/*.rs".to_string()]),
                exclude_patterns: Some(vec!["target/**".to_string()]),
            })
            .expect_err("blank scope kind should fail");
            assert!(err.contains("path_scope.kind must not be empty"));
        }
    }

    #[test]
    fn register_repository_payload_parses_allowed_scope_and_requester() {
        let parsed = parse_register_repository_payload(RegisterRepositoryRequestPayload {
            display_name: "register-test-repo".to_string(),
            root_path: "/tmp/register-test-repo".to_string(),
            allowed_scope: Some(PathScopePayload {
                kind: Some("read_only".to_string()),
                roots: Some(vec![".".to_string()]),
                include_patterns: None,
                exclude_patterns: None,
            }),
            requester: Some(ActorPayload::Agent {
                id: "agent:register".to_string(),
                display_name: Some("Register Agent".to_string()),
            }),
        })
        .expect("register payload parse should succeed");

        assert_eq!(parsed.display_name, "register-test-repo");
        assert_eq!(parsed.root_path, "/tmp/register-test-repo");
        assert_eq!(
            parsed.allowed_scope.as_ref().expect("scope").kind,
            rpc::RpcPathScopeKind::ReadOnly
        );
        assert_eq!(
            parsed.requester,
            Some(rpc::RpcActorDto::Agent {
                id: "agent:register".to_string(),
                display_name: Some("Register Agent".to_string()),
            })
        );
    }

    #[test]
    fn register_repository_payload_rejects_unknown_allowed_scope_kind() {
        let err = parse_register_repository_payload(RegisterRepositoryRequestPayload {
            display_name: "register-test-repo".to_string(),
            root_path: "/tmp/register-test-repo".to_string(),
            allowed_scope: Some(PathScopePayload {
                kind: Some("not-a-scope".to_string()),
                roots: None,
                include_patterns: None,
                exclude_patterns: None,
            }),
            requester: None,
        })
        .expect_err("bad scope kind should fail");

        assert!(err.contains("unknown path_scope.kind"));
    }

    #[test]
    fn submit_job_payload_rejects_empty_filesystem_read_scope() {
        let err = parse_submit_job_payload(SubmitJobRequestPayload {
            repository_id: RepositoryId::new().as_str().to_string(),
            requester: ActorPayload::Agent {
                id: "agent:transport".to_string(),
                display_name: None,
            },
            kind: "read".to_string(),
            goal: Some("read over HTTP".to_string()),
            path_scope: Some(PathScopePayload {
                kind: None,
                roots: None,
                include_patterns: None,
                exclude_patterns: None,
            }),
            requested_capabilities: Some(vec!["filesystem.read".to_string()]),
            tool_args: None,
            idempotency_key: None,
        })
        .expect_err("filesystem.read should reject empty path_scope");

        assert!(err.contains("filesystem operation requires path_scope.roots"));
    }

    #[test]
    fn submit_job_payload_accepts_missing_goal() {
        let repository_id = RepositoryId::new();
        let payload: SubmitJobRequestPayload = serde_json::from_str(&format!(
            r#"{{"repository_id":"{}","requester":{{"type":"agent","id":"agent:transport","display_name":null}},"kind":"read"}}"#,
            repository_id.as_str()
        ))
        .expect("payload should deserialize without a goal key");

        let parsed = parse_submit_job_payload(payload).expect("missing goal should be accepted");

        assert_eq!(parsed.goal, None);
    }

    #[test]
    fn submit_job_payload_accepts_supported_filesystem_caps_with_explicit_path_scope() {
        for capability in [
            "filesystem.read",
            "filesystem.list",
            "filesystem.stat",
            "filesystem.delete",
            "filesystem.search",
            "filesystem.diff",
            "fs.read",
            "fs.list",
            "fs.stat",
            "fs.delete",
            "fs.search",
            "fs.diff",
        ] {
            let parsed = parse_submit_job_payload(SubmitJobRequestPayload {
                repository_id: RepositoryId::new().as_str().to_string(),
                requester: ActorPayload::Agent {
                    id: "agent:transport".to_string(),
                    display_name: None,
                },
                kind: "read".to_string(),
                goal: Some("read over HTTP".to_string()),
                path_scope: Some(PathScopePayload {
                    kind: Some("explicit_paths".to_string()),
                    roots: Some(vec!["notes".to_string()]),
                    include_patterns: None,
                    exclude_patterns: None,
                }),
                requested_capabilities: Some(vec![capability.to_string()]),
                tool_args: None,
                idempotency_key: None,
            })
            .expect("filesystem capability payload should parse");

            assert_eq!(
                parsed.path_scope.as_ref().expect("path scope").roots[0],
                "notes"
            );
            assert_eq!(parsed.requested_capabilities, vec![capability.to_string()]);
        }
    }

    #[tokio::test]
    async fn job_routes_submit_blank_goal_omits_goal_field() {
        let rpc_server = ready_rpc_server();
        let root = test_repo_dir("job-route-blank-goal");
        let repository = {
            let server = rpc_server.read().await;
            server
                .register_repository(rpc::RegisterRepositoryRequest {
                    display_name: "job-route-repo".to_string(),
                    root_path: root.to_string_lossy().to_string(),
                    allowed_scope: None,
                    requester: None,
                })
                .expect("register should succeed")
                .repository
        };

        let submit_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/jobs/submit",
            serde_json::json!({
                "repository_id": repository.repository_id,
                "requester": {
                    "type": "agent",
                    "id": "agent:job-route",
                    "display_name": "Job Route Agent"
                },
                "kind": "read",
                "goal": " \t\n ",
                "requested_capabilities": [],
                "idempotency_key": "job-route-blank-goal"
            }),
        )
        .await;
        assert_eq!(submit_response.status(), StatusCode::OK);
        let submit_payload = to_bytes(submit_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        let job = submit_payload["data"]["job"]
            .as_object()
            .expect("job object should be serialized");
        assert!(!job.contains_key("goal"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_payload_rejects_ambiguous_filesystem_read_scopes() {
        for roots in [
            vec![".".to_string()],
            vec!["src".to_string(), "docs".to_string()],
        ] {
            let err = parse_submit_job_payload(SubmitJobRequestPayload {
                repository_id: RepositoryId::new().as_str().to_string(),
                requester: ActorPayload::Agent {
                    id: "agent:transport".to_string(),
                    display_name: None,
                },
                kind: "read".to_string(),
                goal: Some("read over HTTP".to_string()),
                path_scope: Some(PathScopePayload {
                    kind: Some("repository".to_string()),
                    roots: Some(roots),
                    include_patterns: None,
                    exclude_patterns: None,
                }),
                requested_capabilities: Some(vec!["filesystem.read".to_string()]),
                tool_args: None,
                idempotency_key: None,
            })
            .expect_err("filesystem.read should reject ambiguous path_scope roots");

            assert!(err.contains(
                "filesystem operation requires path_scope.roots to contain exactly one concrete path"
            ));
        }
    }

    #[test]
    fn parse_trust_state_accepts_known_values_and_rejects_unknown() {
        assert_eq!(
            parse_trust_state(Some("trusted".to_string())).expect("trusted"),
            Some(rpc::RpcRepositoryTrustState::Trusted)
        );
        assert_eq!(
            parse_trust_state(Some("read_only".to_string())).expect("read_only"),
            Some(rpc::RpcRepositoryTrustState::ReadOnly)
        );
        assert_eq!(
            parse_trust_state(Some("readonly".to_string())).expect("readonly"),
            Some(rpc::RpcRepositoryTrustState::ReadOnly)
        );
        assert_eq!(
            parse_trust_state(Some("blocked".to_string())).expect("blocked"),
            Some(rpc::RpcRepositoryTrustState::Blocked)
        );
        assert!(parse_trust_state(Some("bad-value".to_string())).is_err());
    }

    #[test]
    fn parse_event_severity_trims_and_normalizes_known_values() {
        assert_eq!(
            parse_event_severity(Some("Info".to_string())).expect("info"),
            Some(rpc::RpcEventSeverity::Info)
        );
        assert_eq!(
            parse_event_severity(Some(" warning ".to_string())).expect("warning"),
            Some(rpc::RpcEventSeverity::Warning)
        );
        assert_eq!(
            parse_event_severity(Some("WARN".to_string())).expect("warn"),
            Some(rpc::RpcEventSeverity::Warning)
        );
        assert!(parse_event_severity(Some("not-a-severity".to_string())).is_err());
    }

    #[test]
    fn core_tool_output_scope_to_rpc_preserves_project_id() {
        let project_id = ProjectId::new();
        let project_id_string = serde_json::to_value(&project_id)
            .expect("project id should serialize")
            .as_str()
            .expect("project id should serialize to a string")
            .to_string();
        let rpc_scope =
            core_tool_output_scope_to_rpc(ToolOutputSettingsScope::project(project_id.clone()))
                .expect("project scope should serialize");

        assert_eq!(
            rpc_scope,
            rpc::RpcToolOutputScope {
                level: rpc::RpcToolOutputScopeLevel::Project {
                    project_id: project_id_string,
                },
                tool_id: None,
            }
        );
    }

    #[test]
    fn parse_listen_addr_uses_default_when_env_not_set() {
        let _guard = ListenAddrEnvGuard::lock();
        std::env::remove_var(LISTEN_ADDR_ENV);
        std::env::remove_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV);
        let (addr, from_env) = listen_addr().expect("listen addr");
        assert!(!from_env);
        assert!(is_loopback(&addr));
        assert_eq!(addr.port(), 8080);
        assert!(addr.ip().is_loopback());
    }

    #[test]
    fn parse_listen_addr_uses_explicit_loopback_when_env_is_set() {
        let _guard = ListenAddrEnvGuard::lock();
        std::env::set_var(LISTEN_ADDR_ENV, "127.0.0.1:0");
        std::env::remove_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV);

        let (addr, from_env) = listen_addr().expect("listen addr");

        assert!(from_env);
        assert!(is_loopback(&addr));
        assert_eq!(addr.port(), 0);
    }

    #[test]
    fn validate_listen_addr_rejects_default_non_loopback_binding() {
        let _guard = UnsafeAllowNonLoopbackListenEnvGuard::lock();
        let previous_unsafe_allow_non_loopback_listen =
            std::env::var_os(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV);
        std::env::remove_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV);
        let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

        let err = validate_listen_addr(&addr, false).expect_err("default non-loopback should fail");

        assert!(err
            .to_string()
            .contains("refusing default non-loopback listener address"));

        match previous_unsafe_allow_non_loopback_listen.as_ref() {
            Some(value) => std::env::set_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV, value),
            None => std::env::remove_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV),
        }
    }

    #[test]
    fn validate_listen_addr_rejects_explicit_non_loopback_without_escape_hatch() {
        let _guard = UnsafeAllowNonLoopbackListenEnvGuard::lock();
        std::env::remove_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV);
        let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

        let err =
            validate_listen_addr(&addr, true).expect_err("non-loopback should require opt-in");

        assert!(err
            .to_string()
            .contains("ATELIA_DAEMON_UNSAFE_ALLOW_NON_LOOPBACK_LISTEN=1"));
    }

    #[test]
    fn validate_listen_addr_allows_explicit_non_loopback_with_escape_hatch() {
        let _guard = UnsafeAllowNonLoopbackListenEnvGuard::lock();
        std::env::set_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV, "true");
        let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

        assert!(unsafe_allow_non_loopback_listen());
        validate_listen_addr(&addr, true).expect("opted-in non-loopback should be allowed");
        assert!(!is_loopback(&addr));
    }

    #[test]
    fn rpc_error_status_maps_request_errors_to_client_responses() {
        assert_eq!(
            rpc_error_status(rpc::RpcErrorCode::InvalidArgument),
            (StatusCode::BAD_REQUEST, false)
        );
        assert_eq!(
            rpc_error_status(rpc::RpcErrorCode::NotFound),
            (StatusCode::NOT_FOUND, false)
        );
        assert_eq!(
            rpc_error_status(rpc::RpcErrorCode::CursorExpired),
            (StatusCode::GONE, true)
        );
    }

    #[test]
    fn rpc_tool_output_scope_to_core_rejects_invalid_deserialization() {
        let repository_scope = rpc::RpcToolOutputScope {
            level: rpc::RpcToolOutputScopeLevel::Repository {
                repository_id: "not-a-valid-repository-id".to_string(),
            },
            tool_id: None,
        };
        let repository_error = rpc_tool_output_scope_to_core(&repository_scope)
            .expect_err("repository id should fail");
        assert!(repository_error
            .to_string()
            .contains("invalid repository_id in tool output scope"));

        let project_scope = rpc::RpcToolOutputScope {
            level: rpc::RpcToolOutputScopeLevel::Project {
                project_id: "not-a-valid-project-id".to_string(),
            },
            tool_id: None,
        };
        let project_error =
            rpc_tool_output_scope_to_core(&project_scope).expect_err("project id should fail");
        assert!(project_error
            .to_string()
            .contains("invalid project_id in tool output scope"));
    }

    #[tokio::test]
    async fn health_endpoint_is_reachable_inprocess() {
        let rpc_server = ready_rpc_server();
        let response = send_request(&rpc_server, Method::GET, "/v1/health").await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(payload["status"], "ok");
        assert!(payload["data"]["daemon_status"].is_string());
        assert!(payload["data"]["capabilities"].is_array());
    }

    #[tokio::test]
    async fn event_routes_are_reachable_inprocess() {
        let rpc_server = ready_rpc_server();
        let root = test_repo_dir("events-route");
        let other_root = test_repo_dir("events-route-other");

        let repository = {
            let server = rpc_server.read().await;
            server
                .register_repository(rpc::RegisterRepositoryRequest {
                    display_name: "event-route-repo".to_string(),
                    root_path: root.to_string_lossy().to_string(),
                    allowed_scope: None,
                    requester: None,
                })
                .expect("register should succeed")
                .repository
        };
        let repository_id = repository.repository_id.clone();
        let other_repository = {
            let server = rpc_server.read().await;
            server
                .register_repository(rpc::RegisterRepositoryRequest {
                    display_name: "event-route-other-repo".to_string(),
                    root_path: other_root.to_string_lossy().to_string(),
                    allowed_scope: None,
                    requester: None,
                })
                .expect("other register should succeed")
                .repository
        };
        let other_repository_id = other_repository.repository_id.clone();

        {
            let server = rpc_server.read().await;
            server
                .submit_job(rpc::SubmitJobRequest {
                    repository_id: repository_id.clone(),
                    requester: rpc::RpcActorDto::Agent {
                        id: "agent:event-route".to_string(),
                        display_name: Some("Event Route Agent".to_string()),
                    },
                    kind: "read".to_string(),
                    goal: Some("first".to_string()),
                    path_scope: None,
                    requested_capabilities: Vec::new(),
                    tool_args: None,
                    idempotency_key: None,
                })
                .expect("first submit should succeed");
            server
                .submit_job(rpc::SubmitJobRequest {
                    repository_id: other_repository_id.clone(),
                    requester: rpc::RpcActorDto::Agent {
                        id: "agent:event-route".to_string(),
                        display_name: Some("Event Route Agent".to_string()),
                    },
                    kind: "read".to_string(),
                    goal: Some("between".to_string()),
                    path_scope: None,
                    requested_capabilities: Vec::new(),
                    tool_args: None,
                    idempotency_key: None,
                })
                .expect("other repo submit should succeed");
            server
                .submit_job(rpc::SubmitJobRequest {
                    repository_id: repository_id.clone(),
                    requester: rpc::RpcActorDto::Agent {
                        id: "agent:event-route".to_string(),
                        display_name: Some("Event Route Agent".to_string()),
                    },
                    kind: "read".to_string(),
                    goal: Some("second".to_string()),
                    path_scope: None,
                    requested_capabilities: Vec::new(),
                    tool_args: None,
                    idempotency_key: None,
                })
                .expect("second submit should succeed");
        }

        let list_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/events/list",
            serde_json::json!({
                "repository_id": repository_id.clone(),
                "cursor": { "kind": "beginning" },
                "subject_ids": [],
                "page_size": 1,
            }),
        )
        .await;
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_payload = to_bytes(list_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(list_payload["status"], "ok");
        assert_eq!(list_payload["data"]["events"].as_array().unwrap().len(), 1);
        let first_event = &list_payload["data"]["events"][0];
        assert_eq!(
            first_event["subject"]["type"],
            Value::String("job".to_string())
        );
        assert!(!first_event["kind"].as_str().unwrap().is_empty());
        assert!(first_event["occurred_at_unix_ms"].is_i64());
        let first_event_id = first_event["event_id"]
            .as_str()
            .expect("event id")
            .to_string();

        let watch_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/events/replay",
            serde_json::json!({
                "repository_id": repository_id.clone(),
                "cursor": {
                    "kind": "after_event_id",
                    "event_id": first_event_id,
                },
                "subject_ids": [],
                "min_severity": "info",
                "limit": 1,
            }),
        )
        .await;
        assert_eq!(watch_response.status(), StatusCode::OK);
        let watch_payload = to_bytes(watch_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(watch_payload["status"], "ok");
        assert_eq!(watch_payload["data"]["events"].as_array().unwrap().len(), 1);
        assert_eq!(
            watch_payload["data"]["events"][0]["refs"]["repository_id"],
            Value::String(repository_id)
        );
        assert!(!watch_payload["data"]["events"][0]["kind"]
            .as_str()
            .unwrap()
            .is_empty());
        assert_eq!(
            watch_payload["data"]["cursor"]["kind"],
            Value::String("after_sequence".to_string())
        );
        assert_eq!(
            watch_payload["data"]["cursor"]["sequence_number"],
            watch_payload["data"]["events"][0]["sequence"]
        );

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(other_root);
    }

    #[tokio::test]
    async fn watch_events_endpoint_returns_ndjson_stream() {
        let rpc_server = ready_rpc_server();
        let root = test_repo_dir("watch-events-stream");
        let repository = {
            let server = rpc_server.read().await;
            server
                .register_repository(rpc::RegisterRepositoryRequest {
                    display_name: "watch-events-stream-repo".to_string(),
                    root_path: root.to_string_lossy().to_string(),
                    allowed_scope: None,
                    requester: None,
                })
                .expect("register should succeed")
                .repository
        };

        let response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/events/watch",
            serde_json::json!({
                "repository_id": repository.repository_id,
                "cursor": { "kind": "beginning" },
                "subject_ids": [],
                "min_severity": "info",
                "limit": 1,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .expect("content type")
                .to_str()
                .expect("valid content type"),
            "application/x-ndjson"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn events_list_accepts_job_ids_and_filters_to_that_job() {
        let rpc_server = ready_rpc_server();
        let root = test_repo_dir("events-list-job-ids");
        let repository = {
            let server = rpc_server.read().await;
            server
                .register_repository(rpc::RegisterRepositoryRequest {
                    display_name: "events-list-job-ids-repo".to_string(),
                    root_path: root.to_string_lossy().to_string(),
                    allowed_scope: None,
                    requester: None,
                })
                .expect("register should succeed")
                .repository
        };
        let repository_id = repository.repository_id.clone();

        let first_job_id = {
            let server = rpc_server.read().await;
            server
                .submit_job(rpc::SubmitJobRequest {
                    repository_id: repository_id.clone(),
                    requester: rpc::RpcActorDto::Agent {
                        id: "agent:events-list".to_string(),
                        display_name: Some("Events List Agent".to_string()),
                    },
                    kind: "read".to_string(),
                    goal: Some("first".to_string()),
                    path_scope: None,
                    requested_capabilities: Vec::new(),
                    tool_args: None,
                    idempotency_key: Some("events-list-first".to_string()),
                })
                .expect("first submit should succeed")
                .job
                .job_id
        };

        let selected_job_id = {
            let server = rpc_server.read().await;
            server
                .submit_job(rpc::SubmitJobRequest {
                    repository_id: repository_id.clone(),
                    requester: rpc::RpcActorDto::Agent {
                        id: "agent:events-list".to_string(),
                        display_name: Some("Events List Agent".to_string()),
                    },
                    kind: "read".to_string(),
                    goal: Some("second".to_string()),
                    path_scope: None,
                    requested_capabilities: Vec::new(),
                    tool_args: None,
                    idempotency_key: Some("events-list-second".to_string()),
                })
                .expect("second submit should succeed")
                .job
                .job_id
        };

        let response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/events/list",
            serde_json::json!({
                "repository_id": repository_id,
                "subject_ids": [],
                "job_ids": [selected_job_id.clone()],
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(payload["status"], "ok");
        let events = payload["data"]["events"].as_array().expect("events array");
        assert!(!events.is_empty());
        assert!(events
            .iter()
            .all(|event| event["refs"]["job_id"] == selected_job_id));
        assert!(events
            .iter()
            .all(|event| event["refs"]["job_id"] != first_job_id));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn job_routes_submit_list_and_report_cancel_errors() {
        let rpc_server = ready_rpc_server();
        let root = test_repo_dir("job-route");
        let missing_job_id = JobId::new().as_str().to_string();
        let repository = {
            let server = rpc_server.read().await;
            server
                .register_repository(rpc::RegisterRepositoryRequest {
                    display_name: "job-route-repo".to_string(),
                    root_path: root.to_string_lossy().to_string(),
                    allowed_scope: None,
                    requester: None,
                })
                .expect("register should succeed")
                .repository
        };

        let submit_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/jobs/submit",
            serde_json::json!({
                "repository_id": repository.repository_id,
                "requester": {
                    "type": "agent",
                    "id": "agent:job-route",
                    "display_name": "Job Route Agent"
                },
                "kind": "read",
                "goal": "submit over HTTP",
                "requested_capabilities": [],
                "idempotency_key": "job-route-1"
            }),
        )
        .await;
        assert_eq!(submit_response.status(), StatusCode::OK);
        let submit_payload = to_bytes(submit_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(submit_payload["status"], "ok");
        assert_eq!(submit_payload["data"]["job"]["status"], "succeeded");
        assert_eq!(
            submit_payload["data"]["policy"]["requested_capability"],
            "capability.discovery"
        );
        let job_id = submit_payload["data"]["job"]["job_id"]
            .as_str()
            .expect("job id should be serialized")
            .to_string();

        let get_response =
            send_request(&rpc_server, Method::GET, &format!("/v1/jobs/{job_id}")).await;
        assert_eq!(get_response.status(), StatusCode::OK);
        let get_payload = to_bytes(get_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(get_payload["data"]["job"]["job_id"], job_id);

        let events_response = send_json_request(
            &rpc_server,
            Method::POST,
            &format!("/v1/jobs/{job_id}/events"),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(events_response.status(), StatusCode::OK);
        let events_payload = to_bytes(events_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert!(!events_payload["data"]["events"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(events_payload["data"]["events"]
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["refs"]["job_id"] == job_id));

        let list_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/jobs/list",
            serde_json::json!({
                "repository_id": repository.repository_id,
                "status": "succeeded",
                "page_size": 10,
            }),
        )
        .await;
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_payload = to_bytes(list_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(list_payload["data"]["jobs"].as_array().unwrap().len(), 1);

        let cancel_response = send_json_request(
            &rpc_server,
            Method::POST,
            &format!("/v1/jobs/{missing_job_id}/cancel"),
            serde_json::json!({
                "requester": {
                    "type": "agent",
                    "id": "agent:job-route",
                    "display_name": "Job Route Agent"
                },
                "reason": "exercise transport error mapping"
            }),
        )
        .await;
        assert_eq!(cancel_response.status(), StatusCode::NOT_FOUND);
        let cancel_payload = to_bytes(cancel_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(cancel_payload["error"]["code"], "rpc_error");

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn tool_output_settings_routes_are_reachable_inprocess() {
        let rpc_server = ready_rpc_server();

        let get_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/tool-output/settings/get",
            serde_json::json!({
                "scope": serde_json::to_value(ToolOutputSettingsScope::workspace())
                    .expect("scope json"),
            }),
        )
        .await;
        assert_eq!(get_response.status(), StatusCode::OK);
        let get_payload = to_bytes(get_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(get_payload["status"], "ok");
        assert_eq!(
            get_payload["data"]["defaults"]["max_inline_lines"],
            Value::from(200)
        );

        let update_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/tool-output/settings/update",
            serde_json::json!({
                "scope": serde_json::to_value(ToolOutputSettingsScope::workspace())
                    .expect("scope json"),
                "actor": serde_json::to_value(Actor::User {
                    id: "user:tool-output".to_string(),
                    display_name: Some("Tool Output User".to_string()),
                })
                .expect("actor json"),
                "reason": "tighten workspace defaults",
                "overrides": serde_json::to_value(ToolOutputOverrides {
                    format: Some(OutputFormat::Json),
                    include_policy: Some(true),
                    include_diagnostics: None,
                    include_cost: None,
                    max_inline_bytes: None,
                    max_inline_lines: Some(42),
                    verbosity: Some(ToolOutputVerbosity::Expanded),
                    granularity: Some(ToolOutputGranularity::Full),
                    oversize_policy: Some(OversizeOutputPolicy::RejectOversize),
                })
                .expect("overrides json"),
            }),
        )
        .await;
        assert_eq!(update_response.status(), StatusCode::OK);
        let update_payload = to_bytes(update_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(update_payload["status"], "ok");
        assert_eq!(
            update_payload["data"]["change"]["new_defaults"]["max_inline_lines"],
            Value::from(42)
        );
        assert!(update_payload["data"]["change"]["changed_at_unix_ms"].is_i64());
        assert!(update_payload["data"]["change"]["changed_at"]["unix_millis"].is_i64());
        assert_eq!(
            update_payload["data"]["change"]["actor"]["user"]["id"],
            Value::String("user:tool-output".to_string())
        );

        let history_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/tool-output/settings/history:list",
            serde_json::json!({
                "scope": serde_json::to_value(ToolOutputSettingsScope::workspace())
                    .expect("scope json"),
                "limit": 10,
            }),
        )
        .await;
        assert_eq!(history_response.status(), StatusCode::OK);
        let history_payload = to_bytes(history_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(history_payload["status"], "ok");
        assert_eq!(
            history_payload["data"]["changes"].as_array().unwrap().len(),
            1
        );
        assert_eq!(
            history_payload["data"]["changes"][0]["new_defaults"]["max_inline_lines"],
            Value::from(42)
        );
        assert!(history_payload["data"]["changes"][0]["changed_at_unix_ms"].is_i64());
        assert!(history_payload["data"]["changes"][0]["changed_at"]["unix_millis"].is_i64());
        assert_eq!(
            history_payload["data"]["changes"][0]["actor"]["user"]["id"],
            Value::String("user:tool-output".to_string())
        );

        let defaults_again = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/tool-output/settings/get",
            serde_json::json!({
                "scope": serde_json::to_value(ToolOutputSettingsScope::workspace())
                    .expect("scope json"),
            }),
        )
        .await;
        assert_eq!(defaults_again.status(), StatusCode::OK);
        let defaults_again_payload = to_bytes(defaults_again.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(
            defaults_again_payload["data"]["defaults"]["max_inline_lines"],
            Value::from(42)
        );
    }

    #[tokio::test]
    /// Exercises extension registry endpoints, including validation, through HTTP dispatch.
    async fn extension_registry_endpoints_are_reachable_inprocess() {
        const ARTIFACT_V1: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const MANIFEST_V1: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        const ARTIFACT_V2: &str =
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        const MANIFEST_V2: &str =
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

        let rpc_server = ready_rpc_server();
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
        let mut accepted_manifest_v1 = manifest_v1.clone();
        accepted_manifest_v1.provenance.publication = Some(atelia_core::ExtensionPublication {
            visibility: atelia_core::ExtensionPublicationVisibility::PublicSearchable,
            registry_submission: atelia_core::ExtensionRegistrySubmission::Accepted,
        });

        let accepted_install_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/install",
            serde_json::json!({
                "manifest": accepted_manifest_v1,
                "approve_local_unsigned": false,
                "allow_local_process_runtime": false,
            }),
        )
        .await;
        assert_eq!(accepted_install_response.status(), StatusCode::BAD_REQUEST);
        let mut rejected_manifest_v1 = manifest_v1.clone();
        rejected_manifest_v1.provenance.publication = Some(atelia_core::ExtensionPublication {
            visibility: atelia_core::ExtensionPublicationVisibility::PublicSearchable,
            registry_submission: atelia_core::ExtensionRegistrySubmission::Rejected,
        });
        let rejected_validate_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/validate",
            serde_json::json!({
                "manifest": rejected_manifest_v1,
                "approve_local_unsigned": false,
                "allow_local_process_runtime": false,
            }),
        )
        .await;
        assert_eq!(rejected_validate_response.status(), StatusCode::BAD_REQUEST);

        let install_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/install",
            serde_json::json!({
                "manifest": manifest_v1,
                "approve_local_unsigned": false,
                "allow_local_process_runtime": false,
            }),
        )
        .await;
        assert_eq!(install_response.status(), StatusCode::OK);
        let install_payload = to_bytes(install_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(install_payload["status"], "ok");
        assert_eq!(install_payload["data"]["record"]["version"], "1.0.0");

        let list_before_validate_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/list",
            serde_json::json!({
                "include_blocked": true,
            }),
        )
        .await;
        assert_eq!(list_before_validate_response.status(), StatusCode::OK);
        let list_before_validate_payload =
            to_bytes(list_before_validate_response.into_body(), usize::MAX)
                .await
                .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
                .expect("response bytes");

        let validate_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/validate",
            serde_json::json!({
                "manifest": manifest_v1,
                "approve_local_unsigned": false,
                "allow_local_process_runtime": false,
            }),
        )
        .await;
        assert_eq!(validate_response.status(), StatusCode::OK);
        let validate_payload = to_bytes(validate_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(validate_payload["status"], "ok");
        assert_eq!(
            validate_payload["data"]["manifest"]["id"],
            "com.example.review.extension"
        );
        assert_eq!(validate_payload["data"]["manifest"]["version"], "1.0.0");
        assert_eq!(validate_payload["data"]["boundary"], "third_party");

        let list_after_validate_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/list",
            serde_json::json!({
                "include_blocked": true,
            }),
        )
        .await;
        assert_eq!(list_after_validate_response.status(), StatusCode::OK);
        let list_after_validate_payload =
            to_bytes(list_after_validate_response.into_body(), usize::MAX)
                .await
                .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
                .expect("response bytes");
        assert_eq!(
            list_after_validate_payload["data"]["extensions"],
            list_before_validate_payload["data"]["extensions"]
        );

        let mut accepted_manifest_v2 = manifest_v2.clone();
        accepted_manifest_v2.provenance.publication = Some(atelia_core::ExtensionPublication {
            visibility: atelia_core::ExtensionPublicationVisibility::PublicSearchable,
            registry_submission: atelia_core::ExtensionRegistrySubmission::Accepted,
        });
        let accepted_update_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/update",
            serde_json::json!({
                "manifest": accepted_manifest_v2,
                "approve_local_unsigned": false,
                "allow_local_process_runtime": false,
            }),
        )
        .await;
        assert_eq!(accepted_update_response.status(), StatusCode::BAD_REQUEST);

        let update_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/update",
            serde_json::json!({
                "manifest": manifest_v2,
                "approve_local_unsigned": false,
                "allow_local_process_runtime": false,
            }),
        )
        .await;
        assert_eq!(update_response.status(), StatusCode::OK);

        let status_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/status",
        )
        .await;
        assert_eq!(status_response.status(), StatusCode::OK);
        let status_payload = to_bytes(status_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(
            status_payload["data"]["extension"]["extension_id"],
            "com.example.review.extension"
        );
        assert_eq!(
            status_payload["data"]["extension"]["record"]["version"],
            "2.0.0"
        );

        let inspect_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/inspect",
        )
        .await;
        assert_eq!(inspect_response.status(), StatusCode::OK);
        let inspect_payload = to_bytes(inspect_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(
            inspect_payload["data"]["package_id"],
            "com.example.review.extension"
        );
        assert_eq!(
            inspect_payload["data"]["extension"]["record"]["version"],
            "2.0.0"
        );
        assert!(inspect_payload["data"]["rollback_available"]
            .as_bool()
            .unwrap());
        assert_eq!(inspect_payload["data"]["manifest"]["version"], "2.0.0");
        assert_eq!(
            inspect_payload["data"]["manifest"]["services"],
            serde_json::json!({
                "provides": [],
                "consumes": []
            })
        );
        assert_eq!(
            inspect_payload["data"]["permissions"],
            serde_json::json!(["service.review.comments"])
        );
        assert!(inspect_payload["data"]["source"].is_object());
        assert!(inspect_payload["data"]["trust"].is_null());
        assert_eq!(
            inspect_payload["data"]["rollback_snapshot"],
            serde_json::json!({
                "manifest_digest": MANIFEST_V1,
                "artifact_digest": ARTIFACT_V1
            })
        );
        assert!(inspect_payload["data"]["block"].is_null());

        let rollback_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/rollback",
        )
        .await;
        assert_eq!(rollback_response.status(), StatusCode::OK);
        let rollback_payload = to_bytes(rollback_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(rollback_payload["data"]["record"]["version"], "1.0.0");

        let disable_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/disable",
        )
        .await;
        assert_eq!(disable_response.status(), StatusCode::OK);
        let disable_payload = to_bytes(disable_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(disable_payload["data"]["record"]["status"], "disabled");

        let enable_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/enable",
        )
        .await;
        assert_eq!(enable_response.status(), StatusCode::OK);
        let enable_payload = to_bytes(enable_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(enable_payload["data"]["record"]["status"], "installed");

        let authoring_flow_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/authoring-flow",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "include_private_steps": true,
            }),
        )
        .await;
        assert_eq!(authoring_flow_response.status(), StatusCode::OK);
        let authoring_flow_payload = to_bytes(authoring_flow_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(
            authoring_flow_payload["data"]["flow"]["package_id"],
            "com.example.review.extension"
        );
        assert_eq!(
            authoring_flow_payload["data"]["flow"]["source_class"],
            "verified-registry"
        );
        assert!(authoring_flow_payload["data"]["flow"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["id"] == "remix"));

        let remix_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/remix",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "source_class": "workspace-local"
            }),
        )
        .await;
        assert_eq!(remix_response.status(), StatusCode::OK);
        let remix_payload = to_bytes(remix_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(
            remix_payload["data"]["flow"]["source_class"],
            "workspace-local"
        );
        assert!(remix_payload["data"]["flow"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["id"] == "remix" && step["state"] == "complete"));

        let invalid_remix_source_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/remix",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "source": {
                    "repository": "https://github.com/example/package",
                    "manifest_path": "atelia.package.yaml"
                }
            }),
        )
        .await;
        assert_eq!(
            invalid_remix_source_response.status(),
            StatusCode::BAD_REQUEST
        );
        let invalid_remix_missing_source_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/remix",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "source_class": "user-selected"
            }),
        )
        .await;
        assert_eq!(
            invalid_remix_missing_source_response.status(),
            StatusCode::BAD_REQUEST
        );
        let invalid_remix_registry_source_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/remix",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "source_class": "verified-registry",
                "source": {
                    "repository": "https://github.com/example/package",
                    "manifest_path": "atelia.package.yaml"
                }
            }),
        )
        .await;
        assert_eq!(
            invalid_remix_registry_source_response.status(),
            StatusCode::BAD_REQUEST
        );

        let unlisted_publication_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/publication",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "visibility": "unlisted_share",
                "requires_registry_submission": false
            }),
        )
        .await;
        assert_eq!(unlisted_publication_response.status(), StatusCode::OK);
        let unlisted_publication_payload =
            to_bytes(unlisted_publication_response.into_body(), usize::MAX)
                .await
                .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
                .expect("response bytes");
        assert!(unlisted_publication_payload["data"]["audit_record_id"]
            .as_str()
            .is_some());
        assert_eq!(
            unlisted_publication_payload["data"]["flow"]["publication_plan"]
                ["requires_registry_submission"],
            false
        );
        assert!(
            !unlisted_publication_payload["data"]["flow"]["publication_plan"]["github_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action == "submit_registry_metadata")
        );
        assert!(unlisted_publication_payload["data"]["flow"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["id"] == "registry_search" && step["state"] == "complete"));

        let publication_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/publication",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "visibility": "public_searchable",
                "requires_registry_submission": true
            }),
        )
        .await;
        assert_eq!(publication_response.status(), StatusCode::OK);
        let publication_payload = to_bytes(publication_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert!(publication_payload["data"]["audit_record_id"]
            .as_str()
            .is_some());
        assert_eq!(
            publication_payload["data"]["flow"]["publication_plan"]["visibility"],
            "public_searchable"
        );
        assert!(publication_payload["data"]["flow"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["id"] == "registry_search" && step["state"] == "requires_consent"));

        let github_remix_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/remix",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "source_class": "user-selected",
                "source": {
                    "repository": "https://github.com/example/package",
                    "manifest_path": "atelia.package.yaml"
                }
            }),
        )
        .await;
        assert_eq!(github_remix_response.status(), StatusCode::OK);
        let github_remix_payload = to_bytes(github_remix_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(
            github_remix_payload["data"]["flow"]["source_class"],
            "user-selected"
        );
        assert_eq!(
            github_remix_payload["data"]["flow"]["publication_plan"]["source_class"],
            "user-selected"
        );
        assert_eq!(
            github_remix_payload["data"]["flow"]["publication_plan"]["source"]["repository"],
            "https://github.com/example/package"
        );

        let blank_registry_identity_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/registry-submission",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "state": "submitted",
                "registry_identity": " \t "
            }),
        )
        .await;
        assert_eq!(
            blank_registry_identity_response.status(),
            StatusCode::BAD_REQUEST
        );
        let padded_registry_identity_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/registry-submission",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "state": "submitted",
                "registry_identity": " third-party-registry "
            }),
        )
        .await;
        assert_eq!(
            padded_registry_identity_response.status(),
            StatusCode::BAD_REQUEST
        );

        let self_accepted_registry_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/registry-submission",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "state": "accepted",
                "registry_identity": "third-party-registry"
            }),
        )
        .await;
        assert_eq!(
            self_accepted_registry_response.status(),
            StatusCode::BAD_REQUEST
        );
        let self_rejected_registry_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/registry-submission",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "state": "rejected",
                "registry_identity": "third-party-registry"
            }),
        )
        .await;
        assert_eq!(
            self_rejected_registry_response.status(),
            StatusCode::BAD_REQUEST
        );

        let registry_submission_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/registry-submission",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "state": "submitted",
                "registry_identity": "third-party-registry"
            }),
        )
        .await;
        assert_eq!(registry_submission_response.status(), StatusCode::OK);
        let registry_submission_payload =
            to_bytes(registry_submission_response.into_body(), usize::MAX)
                .await
                .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
                .expect("response bytes");
        assert_eq!(registry_submission_payload["data"]["state"], "submitted");
        assert!(registry_submission_payload["data"]["audit_record_id"]
            .as_str()
            .is_some());
        assert_eq!(
            registry_submission_payload["data"]["flow"]["publication_plan"]
                ["requires_registry_submission"],
            true
        );
        assert!(registry_submission_payload["data"]["flow"]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["id"] == "registry_search" && step["state"] == "in_progress"));

        let authoring_flow_after_submission_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/authoring-flow",
        )
        .await;
        assert_eq!(
            authoring_flow_after_submission_response.status(),
            StatusCode::OK
        );
        let authoring_flow_after_submission_payload = to_bytes(
            authoring_flow_after_submission_response.into_body(),
            usize::MAX,
        )
        .await
        .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
        .expect("response bytes");
        assert_eq!(
            authoring_flow_after_submission_payload["data"]["flow"]["publication_plan"]
                ["visibility"],
            "public_searchable"
        );
        assert!(
            authoring_flow_after_submission_payload["data"]["flow"]["steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|step| step["id"] == "registry_search" && step["state"] == "in_progress")
        );

        let audit_response =
            send_request(&rpc_server, Method::POST, "/v1/packages/audit:list").await;
        assert_eq!(audit_response.status(), StatusCode::OK);
        let audit_payload = to_bytes(audit_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        let audit_records = audit_payload["data"]["records"].as_array().unwrap();
        assert!(audit_records
            .iter()
            .any(|record| record["kind"] == "publication_update"));
        assert!(audit_records
            .iter()
            .any(|record| record["kind"] == "registry_submission_update"));

        let preserve_submission_publication_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/publication",
            serde_json::json!({
                "package_id": "com.example.review.extension",
                "visibility": "unlisted_share",
                "requires_registry_submission": false
            }),
        )
        .await;
        assert_eq!(
            preserve_submission_publication_response.status(),
            StatusCode::OK
        );
        let preserve_submission_publication_payload = to_bytes(
            preserve_submission_publication_response.into_body(),
            usize::MAX,
        )
        .await
        .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
        .expect("response bytes");
        assert_eq!(
            preserve_submission_publication_payload["data"]["flow"]["publication_plan"]
                ["requires_registry_submission"],
            true
        );
        assert!(
            preserve_submission_publication_payload["data"]["flow"]["steps"]
                .as_array()
                .unwrap()
                .iter()
                .any(|step| step["id"] == "registry_search" && step["state"] == "in_progress")
        );

        let block_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/blocklist/apply",
            serde_json::json!({
                "entry": {
                    "key": serde_json::to_value(BlockKey::ExtensionId("com.example.review.extension".to_string())).expect("block key"),
                    "reason": BlockReason::UserBlocked,
                    "note": "policy review"
                }
            }),
        )
        .await;
        assert_eq!(block_response.status(), StatusCode::OK);

        let blocked_status_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/status",
        )
        .await;
        assert_eq!(blocked_status_response.status(), StatusCode::OK);
        let blocked_status_payload = to_bytes(blocked_status_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(
            blocked_status_payload["data"]["extension"]["record"]["status"],
            "blocked"
        );

        let blocked_inspect_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/inspect",
        )
        .await;
        assert_eq!(blocked_inspect_response.status(), StatusCode::OK);
        let blocked_inspect_payload = to_bytes(blocked_inspect_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(
            blocked_inspect_payload["data"]["block"]["reason"],
            "user_blocked"
        );
        assert_eq!(
            blocked_inspect_payload["data"]["extension"]["record"]["status"],
            "blocked"
        );
        assert_eq!(blocked_inspect_payload["data"]["rollback_available"], false);

        let list_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/list",
            serde_json::json!({ "include_blocked": false }),
        )
        .await;
        assert_eq!(list_response.status(), StatusCode::OK);
        let list_payload = to_bytes(list_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert!(list_payload["data"]["extensions"]
            .as_array()
            .unwrap()
            .is_empty());

        let trust_index_response =
            send_request(&rpc_server, Method::POST, "/v1/package-trust-index:list").await;
        assert_eq!(trust_index_response.status(), StatusCode::OK);
        let trust_index_payload = to_bytes(trust_index_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(
            trust_index_payload["data"]["packages"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            trust_index_payload["data"]["packages"][0]["package_id"],
            "com.example.review.extension"
        );
        assert_eq!(
            trust_index_payload["data"]["packages"][0]["status"],
            "blocked"
        );
        assert!(trust_index_payload["data"]["packages"][0]
            .get("approved_permissions")
            .is_none());

        let filtered_trust_index_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/package-trust-index:list",
            serde_json::json!({
                "include_blocked": false,
                "discovery_only": true,
            }),
        )
        .await;
        assert_eq!(filtered_trust_index_response.status(), StatusCode::OK);
        let filtered_trust_index_payload =
            to_bytes(filtered_trust_index_response.into_body(), usize::MAX)
                .await
                .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
                .expect("response bytes");
        assert_eq!(
            filtered_trust_index_payload["data"]["packages"]
                .as_array()
                .unwrap()
                .len(),
            0
        );

        let execute_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/execute",
        )
        .await;
        assert_eq!(execute_response.status(), StatusCode::NOT_IMPLEMENTED);
        let execute_payload = to_bytes(execute_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(execute_payload["status"], "error");
        assert_eq!(execute_payload["error"]["code"], "rpc_error");
        assert!(execute_payload["error"]["recoverable"].as_bool().unwrap());
        assert!(execute_payload["error"]["reason"]
            .as_str()
            .unwrap()
            .contains("install, status, and blocklist management APIs"));

        let remove_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/remove",
        )
        .await;
        assert_eq!(remove_response.status(), StatusCode::OK);
        let remove_payload = to_bytes(remove_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(remove_payload["data"]["record"]["status"], "disabled");

        let removed_status_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/status",
        )
        .await;
        assert_eq!(removed_status_response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn extension_update_endpoint_requires_explicit_source_change_approval() {
        const ARTIFACT_V1: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const MANIFEST_V1: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        const ARTIFACT_V2: &str =
            "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
        const MANIFEST_V2: &str =
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

        let rpc_server = ready_rpc_server();
        let manifest_v1 = extension_manifest(
            "com.example.source-boundary",
            "1.0.0",
            ARTIFACT_V1,
            MANIFEST_V1,
        );
        let mut manifest_v2 = extension_manifest(
            "com.example.source-boundary",
            "2.0.0",
            ARTIFACT_V2,
            MANIFEST_V2,
        );
        manifest_v2.provenance.source_ref = Some("refs/heads/release".to_string());

        let install_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/install",
            serde_json::json!({
                "manifest": manifest_v1,
                "approve_local_unsigned": false,
                "allow_local_process_runtime": false,
            }),
        )
        .await;
        assert_eq!(install_response.status(), StatusCode::OK);

        let denied_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/update",
            serde_json::json!({
                "manifest": manifest_v2,
                "approve_local_unsigned": false,
                "allow_local_process_runtime": false,
            }),
        )
        .await;
        assert_eq!(denied_response.status(), StatusCode::CONFLICT);
        let denied_payload = to_bytes(denied_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(denied_payload["status"], "error");
        assert_eq!(denied_payload["error"]["code"], "rpc_error");
        assert!(denied_payload["error"]["recoverable"].as_bool().unwrap());
        assert!(denied_payload["error"]["reason"]
            .as_str()
            .unwrap()
            .contains("changed source provenance"));

        let approved_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/update",
            serde_json::json!({
                "manifest": manifest_v2,
                "approve_local_unsigned": false,
                "allow_local_process_runtime": false,
                "approve_source_change": true,
            }),
        )
        .await;
        assert_eq!(approved_response.status(), StatusCode::OK);
        let approved_payload = to_bytes(approved_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(approved_payload["data"]["record"]["version"], "2.0.0");
        assert_eq!(
            approved_payload["data"]["record"]["source"]["ref"],
            "refs/heads/release"
        );
    }

    /// Verifies the validation route rejects non-POST methods with an Allow header.
    #[tokio::test]
    async fn extension_validate_route_rejects_get_with_allow_post() {
        let rpc_server = ready_rpc_server();
        let response = send_request(&rpc_server, Method::GET, "/v1/packages/validate").await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response.headers().get(header::ALLOW),
            Some(&header::HeaderValue::from_static("POST"))
        );
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(payload["status"], "error");
        assert_eq!(payload["error"]["code"], "method_not_allowed");
    }

    /// Verifies the inspect route rejects non-POST methods with an Allow header.
    #[tokio::test]
    async fn extension_inspect_route_rejects_get_with_allow_post() {
        let rpc_server = ready_rpc_server();
        let response = send_request(
            &rpc_server,
            Method::GET,
            "/v1/packages/com.example.review.extension/inspect",
        )
        .await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response.headers().get(header::ALLOW),
            Some(&header::HeaderValue::from_static("POST"))
        );
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(payload["status"], "error");
        assert_eq!(payload["error"]["code"], "method_not_allowed");
    }

    /// Verifies the inspect route rejects unexpected request fields.
    #[tokio::test]
    async fn extension_inspect_route_rejects_unexpected_payload() {
        let rpc_server = ready_rpc_server();
        let response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/com.example.review.extension/inspect",
            serde_json::json!({
                "unexpected": true,
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(payload["status"], "error");
        assert_eq!(payload["error"]["code"], "invalid_json");
    }

    /// Verifies invalid HTTP validation requests leave installed extension state unchanged.
    #[tokio::test]
    async fn extension_validate_route_rejects_invalid_manifest_without_state_change() {
        const ARTIFACT_V1: &str =
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const MANIFEST_V1: &str =
            "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

        let rpc_server = ready_rpc_server();
        let manifest_v1 = extension_manifest(
            "com.example.review.extension",
            "1.0.0",
            ARTIFACT_V1,
            MANIFEST_V1,
        );

        let install_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/install",
            serde_json::json!({
                "manifest": manifest_v1.clone(),
                "approve_local_unsigned": false,
                "allow_local_process_runtime": false,
            }),
        )
        .await;
        assert_eq!(install_response.status(), StatusCode::OK);

        let list_before = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/list",
            serde_json::json!({ "include_blocked": true }),
        )
        .await;
        assert_eq!(list_before.status(), StatusCode::OK);
        let list_before_payload = to_bytes(list_before.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");

        let mut invalid_manifest = manifest_v1;
        invalid_manifest.version = "not-semver".to_string();
        let invalid_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/validate",
            serde_json::json!({
                "manifest": invalid_manifest,
                "approve_local_unsigned": false,
                "allow_local_process_runtime": false,
            }),
        )
        .await;
        assert_eq!(invalid_response.status(), StatusCode::BAD_REQUEST);
        let invalid_payload = to_bytes(invalid_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(invalid_payload["status"], "error");
        assert_eq!(invalid_payload["error"]["code"], "rpc_error");

        let list_after = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/packages/list",
            serde_json::json!({ "include_blocked": true }),
        )
        .await;
        assert_eq!(list_after.status(), StatusCode::OK);
        let list_after_payload = to_bytes(list_after.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(
            list_after_payload["data"]["extensions"],
            list_before_payload["data"]["extensions"]
        );
    }

    #[tokio::test]
    async fn render_tool_output_endpoint_returns_rendered_output() {
        let rpc_server = ready_rpc_server();
        let root = test_repo_dir("render-tool-output");
        let request_json = {
            let mut server = rpc_server.write().await;
            let repository = server
                .service_mut()
                .register_repository(service::RegisterRepositoryRequest {
                    display_name: "render-tool-output".to_string(),
                    root_path: root.to_string_lossy().to_string(),
                    trust_state: atelia_core::RepositoryTrustState::Trusted,
                    allowed_scope: None,
                    requester: None,
                })
                .expect("repository registration should succeed");

            let receipt = server
                .service_mut()
                .submit_job(service::SubmitJobRequest {
                    requester: atelia_core::Actor::Agent {
                        id: "agent:render".to_string(),
                        display_name: Some("Render Test".to_string()),
                    },
                    repository_id: repository.id.clone(),
                    kind: atelia_core::JobKind::Read,
                    goal: Some("render tool output".to_string()),
                    resource_scope: None,
                    requested_capabilities: Vec::new(),
                    tool_args: None,
                    idempotency_key: None,
                })
                .expect("job submission should succeed");
            let tool_result = receipt.tool_result.expect("tool result should be stored");

            server
                .service_mut()
                .update_tool_output_defaults(
                    atelia_core::Actor::Agent {
                        id: "agent:render".to_string(),
                        display_name: Some("Render Test".to_string()),
                    },
                    atelia_core::ToolOutputSettingsScope::workspace()
                        .for_tool(tool_result.tool_id.clone()),
                    atelia_core::ToolOutputOverrides {
                        granularity: Some(atelia_core::ToolOutputGranularity::Summary),
                        ..atelia_core::ToolOutputOverrides::default()
                    },
                    "Compact rendered stored results".to_string(),
                )
                .expect("tool output settings update should succeed");

            serde_json::json!({
                "tool_result": {
                    "tool_result_id": tool_result.id.as_str(),
                    "tool_invocation_id": tool_result.invocation_id.as_str(),
                    "job_id": receipt.job.id.as_str(),
                    "repository_id": repository.id.as_str(),
                    "content_type": "application/json",
                },
                "format": "json",
            })
        };

        let app = build_router(rpc_server.clone(), disabled_auth());
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/tool-results:render")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&request_json).expect("request json"),
                    ))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), StatusCode::OK);
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["data"]["format"], "json");
        assert_eq!(
            payload["data"]["tool_result"]["content_type"],
            "application/json"
        );
        assert_eq!(
            payload["data"]["rendered_output_metadata"]["degraded"],
            true
        );
        assert!(
            payload["data"]["rendered_output_metadata"]["fallback_reason"]
                .as_str()
                .expect("fallback reason")
                .contains("render policy compacted output")
        );
        let rendered_output: serde_json::Value =
            serde_json::from_str(payload["data"]["rendered_output"].as_str().unwrap())
                .expect("rendered json");
        assert_eq!(rendered_output["fields"].as_array().unwrap().len(), 1);
        assert_eq!(rendered_output["fields"][0]["key"], "summary");
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn render_tool_output_endpoint_rejects_get_with_allow_post() {
        let rpc_server = ready_rpc_server();
        let response = send_request(&rpc_server, Method::GET, "/v1/tool-results:render").await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response.headers().get(header::ALLOW),
            Some(&header::HeaderValue::from_static("POST"))
        );
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(payload["status"], "error");
        assert_eq!(payload["error"]["code"], "method_not_allowed");
    }

    #[tokio::test]
    async fn unsupported_endpoint_returns_structured_json() {
        let rpc_server = ready_rpc_server();
        let response = send_request(&rpc_server, Method::GET, "/v1/does-not-exist").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(payload["status"], "error");
        assert_eq!(payload["error"]["code"], "unsupported_endpoint");
    }

    #[tokio::test]
    async fn list_repositories_router_rejects_post_to_health() {
        let rpc_server = ready_rpc_server();
        let response = send_request(&rpc_server, Method::POST, "/v1/health").await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn register_repository_router_registers_repository() {
        let rpc_server = ready_rpc_server();
        let root = test_repo_dir("repositories-register-route");
        let normalized_root = fs::canonicalize(&root)
            .expect("canonicalize test repository path")
            .to_string_lossy()
            .to_string();
        let response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/repositories:register",
            serde_json::json!({
                "display_name": "register-route-repo",
                "root_path": root.to_string_lossy().to_string(),
                "allowed_scope": {
                    "kind": "repository",
                },
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");

        assert_eq!(payload["status"], "ok");
        assert_eq!(
            payload["data"]["repository"]["display_name"],
            "register-route-repo"
        );
        assert_eq!(
            payload["data"]["repository"]["root_path"],
            serde_json::Value::String(normalized_root)
        );
        assert_eq!(
            payload["data"]["repository"]["trust_state"],
            serde_json::Value::String("trusted".to_string())
        );

        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn repositories_register_router_rejects_get() {
        let rpc_server = ready_rpc_server();
        let response = send_request(&rpc_server, Method::GET, "/v1/repositories:register").await;
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            response.headers().get(header::ALLOW),
            Some(&header::HeaderValue::from_static("POST"))
        );
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(payload["status"], "error");
        assert_eq!(payload["error"]["code"], "method_not_allowed");
    }

    #[tokio::test]
    async fn oversized_list_repositories_request_returns_too_large() {
        let rpc_server = ready_rpc_server();
        let payload = vec![b'a'; MAX_REQUEST_BODY_BYTES + 1];
        let app = build_router(rpc_server, disabled_auth());
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/v1/repositories:list")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(payload))
                    .expect("valid request"),
            )
            .await
            .expect("request should succeed");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = to_bytes(response.into_body(), MAX_REQUEST_BODY_BYTES + 1)
            .await
            .expect("response bytes");
        let payload = serde_json::from_slice::<Value>(&body).expect("response json");
        assert_eq!(payload["status"], "error");
        assert_eq!(payload["error"]["code"], "request_too_large");
    }

    #[tokio::test]
    async fn list_repertoire_route_returns_builtin_projection() {
        let rpc_server = ready_rpc_server();
        let response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/repertoire:list",
            serde_json::json!({}),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(payload["status"], "ok");
        let entries = payload["data"]["entries"]
            .as_array()
            .expect("repertoire entries array");
        let tool_ids: Vec<&str> = entries
            .iter()
            .map(|entry| entry["tool_id"].as_str().expect("tool id"))
            .collect();
        assert_eq!(
            tool_ids,
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
        assert_eq!(entries[0]["timeout_ms"].as_u64(), Some(0));
        assert_eq!(entries[1]["timeout_ms"].as_u64(), Some(0));
    }

    #[tokio::test]
    async fn list_repertoire_route_rejects_unknown_fields() {
        let rpc_server = ready_rpc_server();
        let response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/repertoire:list",
            serde_json::json!({
                "unexpected": true,
            }),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let payload = to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(payload["status"], "error");
        assert_eq!(payload["error"]["code"], "invalid_json");
        assert!(payload["error"]["reason"]
            .as_str()
            .expect("error reason")
            .contains("unknown field"));
    }

    #[tokio::test]
    async fn local_tcp_health_endpoint_is_reachable_if_socket_allowed() {
        use tokio::net::TcpListener;

        let rpc_server = ready_rpc_server();
        let listener = match TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind test listener: {error}"),
        };
        let addr = listener.local_addr().expect("listener local addr");

        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let server_task = tokio::spawn(run_listener(
            rpc_server.clone(),
            disabled_auth(),
            listener,
            shutdown_rx,
        ));

        let response = reqwest::get(format!("http://{addr}/v1/health"))
            .await
            .expect("request should succeed")
            .json::<Value>()
            .await
            .expect("response json");

        assert_eq!(response["status"], "ok");
        assert!(response["data"]["daemon_status"].is_string());
        assert!(response["data"]["capabilities"].is_array());
        assert_eq!(response["data"]["beta_state"]["scope"], "process_local");
        assert_eq!(response["data"]["beta_state"]["durability"], "in_memory");
        assert_eq!(
            response["data"]["beta_state"]["restart_semantics"],
            "reset_on_restart"
        );

        shutdown_tx.send(()).expect("shutdown signal");
        let _ = server_task
            .await
            .expect("server task should complete after shutdown");
    }

    #[tokio::test]
    async fn run_listener_rejects_disabled_auth_on_unsafe_non_loopback_listener() {
        use tokio::net::TcpListener;

        let _guard = UnsafeAllowNonLoopbackListenEnvGuard::lock();
        std::env::set_var(UNSAFE_ALLOW_NON_LOOPBACK_LISTEN_ENV, "1");

        let rpc_server = ready_rpc_server();
        let listener = match TcpListener::bind("0.0.0.0:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind test listener: {error}"),
        };

        let (_shutdown_tx, shutdown_rx) = oneshot::channel();
        let server_task = tokio::spawn(run_listener(
            rpc_server,
            disabled_auth(),
            listener,
            shutdown_rx,
        ));

        let err = tokio::time::timeout(Duration::from_secs(1), server_task)
            .await
            .expect("run_listener should fail before serving")
            .expect("server task should not panic")
            .expect_err("run_listener should reject unsafe listener");

        assert!(err
            .to_string()
            .contains("refusing to combine ATELIA_DAEMON_AUTH_DISABLED=1"));
    }

    #[tokio::test]
    async fn bind_listener_reports_busy_socket_address() {
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == ErrorKind::PermissionDenied => return,
            Err(error) => panic!("failed to bind test listener: {error}"),
        };
        let busy_addr = listener.local_addr().expect("listener local addr");
        let bind_result = bind_listener(busy_addr).await;
        assert!(
            bind_result.is_err(),
            "binding to an in-use local address should fail"
        );
    }
}
