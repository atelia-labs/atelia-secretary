use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use crate::rpc;
use anyhow::{anyhow, Context, Result};
use atelia_core::{
    Actor, JobId, LedgerTimestamp, OutputFormat, OversizeOutputPolicy, ProjectId, RenderOptions,
    RepositoryId, ToolOutputDefaults, ToolOutputGranularity, ToolOutputOverrides,
    ToolOutputSettingsChange, ToolOutputSettingsScope, ToolOutputVerbosity,
};
use axum::{
    body::{to_bytes, Body},
    extract::{Request, State},
    http::{header, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{oneshot, RwLock};

const DEFAULT_LISTEN_ADDR: &str = "127.0.0.1:8080";
const LISTEN_ADDR_ENV: &str = "ATELIA_DAEMON_LISTEN_ADDR";
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024; // 1 MiB

pub type RpcServerState = Arc<RwLock<rpc::SecretaryRpcServer>>;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Route {
    Health,
    SubmitJob,
    GetJob { job_id: String },
    ListJobs,
    ListJobEvents { job_id: String },
    CancelJob { job_id: String },
    ListRepositories,
    ListEvents,
    ReplayEvents,
    GetToolOutputDefaults,
    UpdateToolOutputDefaults,
    ListToolOutputSettingsHistory,
    InstallExtension,
    UpdateExtension,
    ListExtensions,
    ExtensionStatus { extension_id: String },
    RollbackExtension { extension_id: String },
    DisableExtension { extension_id: String },
    EnableExtension { extension_id: String },
    RemoveExtension { extension_id: String },
    ApplyBlocklist,
    ListBlocklist,
    RenderToolOutput,
    ProjectStatus,
    Unsupported,
}

fn route_for_path(path: &str) -> Route {
    if let Some(job_id) = path
        .strip_prefix("/v1/jobs/")
        .and_then(|path| path.strip_suffix("/cancel"))
        .and_then(valid_path_id)
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
        .strip_prefix("/v1/extensions/")
        .and_then(|path| path.strip_suffix("/status"))
        .and_then(valid_extension_id)
    {
        return Route::ExtensionStatus {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/extensions/")
        .and_then(|path| path.strip_suffix("/rollback"))
        .and_then(valid_extension_id)
    {
        return Route::RollbackExtension {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/extensions/")
        .and_then(|path| path.strip_suffix("/disable"))
        .and_then(valid_extension_id)
    {
        return Route::DisableExtension {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/extensions/")
        .and_then(|path| path.strip_suffix("/enable"))
        .and_then(valid_extension_id)
    {
        return Route::EnableExtension {
            extension_id: extension_id.to_string(),
        };
    }
    if let Some(extension_id) = path
        .strip_prefix("/v1/extensions/")
        .and_then(|path| path.strip_suffix("/remove"))
        .and_then(valid_extension_id)
    {
        return Route::RemoveExtension {
            extension_id: extension_id.to_string(),
        };
    }
    match path {
        "/v1/health" => Route::Health,
        "/v1/jobs/submit" => Route::SubmitJob,
        "/v1/jobs/list" => Route::ListJobs,
        "/v1/repositories:list" => Route::ListRepositories,
        "/v1/events/list" => Route::ListEvents,
        "/v1/events/replay" => Route::ReplayEvents,
        "/v1/tool-output/settings/get" => Route::GetToolOutputDefaults,
        "/v1/tool-output/settings/update" => Route::UpdateToolOutputDefaults,
        "/v1/tool-output/settings/history:list" => Route::ListToolOutputSettingsHistory,
        "/v1/extensions/install" => Route::InstallExtension,
        "/v1/extensions/update" => Route::UpdateExtension,
        "/v1/extensions/list" => Route::ListExtensions,
        "/v1/extensions/blocklist/apply" => Route::ApplyBlocklist,
        "/v1/extensions/blocklist/list" => Route::ListBlocklist,
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

#[derive(Debug, Deserialize)]
struct ListRepositoriesRequestPayload {
    trust_state: Option<String>,
    page_size: Option<usize>,
    page_token: Option<String>,
}

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
    goal: String,
    path_scope: Option<PathScopePayload>,
    requested_capabilities: Option<Vec<String>>,
    idempotency_key: Option<String>,
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
    actor: ActorPayload,
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

pub fn is_loopback(addr: &SocketAddr) -> bool {
    addr.ip().is_loopback()
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

fn parse_path_scope_kind(value: Option<String>) -> Result<rpc::RpcPathScopeKind, String> {
    let Some(value) = value else {
        return Ok(rpc::RpcPathScopeKind::Repository);
    };

    match value.trim().to_ascii_lowercase().as_str() {
        "" | "repository" => Ok(rpc::RpcPathScopeKind::Repository),
        "explicit_paths" | "explicit" => Ok(rpc::RpcPathScopeKind::ExplicitPaths),
        "read_only" | "readonly" => Ok(rpc::RpcPathScopeKind::ReadOnly),
        "unspecified" => Ok(rpc::RpcPathScopeKind::Unspecified),
        unknown => Err(format!("unknown path_scope.kind '{unknown}'")),
    }
}

fn parse_path_scope_payload(payload: PathScopePayload) -> Result<rpc::RpcPathScope, String> {
    Ok(rpc::RpcPathScope {
        kind: parse_path_scope_kind(payload.kind)?,
        roots: payload.roots.unwrap_or_default(),
        include_patterns: payload.include_patterns.unwrap_or_default(),
        exclude_patterns: payload.exclude_patterns.unwrap_or_default(),
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
    Ok(rpc::SubmitJobRequest {
        repository_id: payload.repository_id,
        requester: parse_actor_payload(payload.requester),
        kind: payload.kind,
        goal: payload.goal,
        path_scope: payload
            .path_scope
            .map(parse_path_scope_payload)
            .transpose()?,
        requested_capabilities: payload.requested_capabilities.unwrap_or_default(),
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
        map.insert("actor".to_string(), serialize_actor(&change.actor));

        if let Some(serde_json::Value::Object(changed_at)) = map.remove("changed_at") {
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
    serde_json::json!({
        "job_id": job.job_id,
        "repository_id": job.repository_id,
        "requester": serialize_actor(&job.requester),
        "kind": job.kind,
        "goal": job.goal,
        "status": job.status,
        "policy_summary": job.policy_summary.as_ref().map(serialize_policy_summary),
        "created_at_unix_ms": job.created_at_unix_ms,
        "started_at_unix_ms": job.started_at_unix_ms,
        "completed_at_unix_ms": job.completed_at_unix_ms,
        "latest_event_id": job.latest_event_id,
        "cancellation": serialize_job_cancellation(&job.cancellation),
    })
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
    })
}

fn serialize_update_extension_response(
    response: rpc::UpdateExtensionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "record": response.record,
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

fn serialize_list_extensions_response(response: rpc::ListExtensionsResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "extensions": response.extensions,
    })
}

fn serialize_rollback_extension_response(
    response: rpc::RollbackExtensionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "record": response.record,
    })
}

fn serialize_disable_extension_response(
    response: rpc::DisableExtensionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "record": response.record,
    })
}

fn serialize_enable_extension_response(
    response: rpc::EnableExtensionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "record": response.record,
    })
}

fn serialize_remove_extension_response(
    response: rpc::RemoveExtensionResponse,
) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "record": response.record,
    })
}

fn serialize_apply_blocklist_response(response: rpc::ApplyBlocklistResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "entry": response.entry,
    })
}

fn serialize_list_blocklist_response(response: rpc::ListBlocklistResponse) -> serde_json::Value {
    serde_json::json!({
        "metadata": serialize_protocol_metadata(&response.metadata),
        "entries": response.entries,
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

fn transport_error_response(next_state: String, reason: impl std::fmt::Display) -> Response {
    make_error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "transport_error",
        reason.to_string(),
        false,
        next_state,
    )
}

fn rpc_next_state(server: &rpc::SecretaryRpcServer) -> String {
    server.health(rpc::HealthRequest).daemon_status
}

fn rpc_error_status(code: rpc::RpcErrorCode) -> (StatusCode, bool) {
    match code {
        rpc::RpcErrorCode::InvalidArgument => (StatusCode::BAD_REQUEST, false),
        rpc::RpcErrorCode::NotFound => (StatusCode::NOT_FOUND, false),
        rpc::RpcErrorCode::Conflict => (StatusCode::CONFLICT, true),
        rpc::RpcErrorCode::Internal => (StatusCode::INTERNAL_SERVER_ERROR, false),
    }
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
        actor: parse_actor_payload(payload.actor),
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
    let payload = match body_or_empty_json::<atelia_core::InstallExtensionRequest>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.install_extension(payload) {
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

async fn dispatch_update_extension(state: RpcServerState, request: Request<Body>) -> Response {
    let payload = match body_or_empty_json::<atelia_core::UpdateExtensionRequest>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.update_extension(payload) {
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

async fn dispatch_rollback_extension(
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
    match rpc_server.rollback_extension(atelia_core::RollbackExtensionRequest { extension_id }) {
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
    if let Err(error) = body_or_empty_json::<serde_json::Value>(request).await {
        let rpc_server = state.read().await;
        let next_state = rpc_next_state(&rpc_server);
        return error.into_response(next_state);
    }

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.disable_extension(atelia_core::DisableExtensionRequest { extension_id }) {
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
    if let Err(error) = body_or_empty_json::<serde_json::Value>(request).await {
        let rpc_server = state.read().await;
        let next_state = rpc_next_state(&rpc_server);
        return error.into_response(next_state);
    }

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.enable_extension(atelia_core::EnableExtensionRequest { extension_id }) {
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
    if let Err(error) = body_or_empty_json::<serde_json::Value>(request).await {
        let rpc_server = state.read().await;
        let next_state = rpc_next_state(&rpc_server);
        return error.into_response(next_state);
    }

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.remove_extension(atelia_core::RemoveExtensionRequest { extension_id }) {
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
    let payload = match body_or_empty_json::<atelia_core::ApplyBlocklistRequest>(request).await {
        Ok(payload) => payload,
        Err(error) => {
            let rpc_server = state.read().await;
            let next_state = rpc_next_state(&rpc_server);
            return error.into_response(next_state);
        }
    };

    let rpc_server = state.read().await;
    let next_state = rpc_next_state(&rpc_server);
    match rpc_server.apply_blocklist(payload) {
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
async fn dispatch_route(State(state): State<RpcServerState>, request: Request<Body>) -> Response {
    let method = request.method().clone();
    let path = request.uri().path();

    match route_for_path(path) {
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
}

async fn fallback_route(State(state): State<RpcServerState>, request: Request<Body>) -> Response {
    let path = request.uri().path();
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
}

pub fn build_router(rpc_server: RpcServerState) -> Router {
    Router::new()
        .route("/v1/health", any(dispatch_route))
        .route("/v1/jobs/submit", any(dispatch_route))
        .route("/v1/jobs/list", any(dispatch_route))
        .route("/v1/jobs/*path", any(dispatch_route))
        .route("/v1/repositories:list", any(dispatch_route))
        .route("/v1/events/list", any(dispatch_route))
        .route("/v1/events/replay", any(dispatch_route))
        .route("/v1/tool-output/settings/get", any(dispatch_route))
        .route("/v1/tool-output/settings/update", any(dispatch_route))
        .route("/v1/tool-output/settings/history:list", any(dispatch_route))
        .route("/v1/extensions/install", any(dispatch_route))
        .route("/v1/extensions/update", any(dispatch_route))
        .route("/v1/extensions/list", any(dispatch_route))
        .route("/v1/extensions/blocklist/apply", any(dispatch_route))
        .route("/v1/extensions/blocklist/list", any(dispatch_route))
        .route("/v1/extensions/*path", any(dispatch_route))
        .route("/v1/tool-results:render", any(dispatch_route))
        .route("/v1/project-status:get", any(dispatch_route))
        .fallback(fallback_route)
        .with_state(rpc_server)
}

pub async fn bind_listener(listen_addr: SocketAddr) -> Result<TcpListener> {
    TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind {listen_addr}"))
}

pub async fn run_listener(
    rpc_server: RpcServerState,
    listener: TcpListener,
    shutdown: oneshot::Receiver<()>,
) -> Result<()> {
    axum::serve(listener, build_router(rpc_server))
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
        BlockKey, BlockReason, DegradeBehavior, ExtensionCompatibility, ExtensionEntrypoints,
        ExtensionFailure, ExtensionKind, ExtensionManifest, ExtensionPermission,
        ExtensionPublisher, ExtensionRealm, ExtensionRuntime, ExtensionServices, ProvenanceSource,
        RetryPolicy, EXTENSION_MANIFEST_SCHEMA, EXTENSION_RPC_PROTOCOL,
    };
    use axum::http::StatusCode;
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
        _lock: MutexGuard<'static, ()>,
    }

    impl ListenAddrEnvGuard {
        fn lock() -> Self {
            let lock = LISTEN_ADDR_ENV_MUTEX.lock().unwrap();
            let previous = std::env::var_os(LISTEN_ADDR_ENV);
            Self {
                previous,
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

    async fn send_request(state: &RpcServerState, method: Method, path: &str) -> Response {
        let app = build_router(state.clone());
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
        let app = build_router(state.clone());
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
            route_for_path("/v1/jobs/job_123/cancel"),
            Route::CancelJob {
                job_id: "job_123".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/repositories:list"),
            Route::ListRepositories
        );
        assert_eq!(route_for_path("/v1/events/list"), Route::ListEvents);
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
            route_for_path("/v1/extensions/install"),
            Route::InstallExtension
        );
        assert_eq!(
            route_for_path("/v1/extensions/update"),
            Route::UpdateExtension
        );
        assert_eq!(route_for_path("/v1/extensions/list"), Route::ListExtensions);
        assert_eq!(
            route_for_path("/v1/extensions/blocklist/apply"),
            Route::ApplyBlocklist
        );
        assert_eq!(
            route_for_path("/v1/extensions/blocklist/list"),
            Route::ListBlocklist
        );
        assert_eq!(
            route_for_path("/v1/extensions/com.example.extension/status"),
            Route::ExtensionStatus {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/extensions/com.example.extension/rollback"),
            Route::RollbackExtension {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/extensions/com.example.extension/disable"),
            Route::DisableExtension {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/extensions/com.example.extension/enable"),
            Route::EnableExtension {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(
            route_for_path("/v1/extensions/com.example.extension/remove"),
            Route::RemoveExtension {
                extension_id: "com.example.extension".to_string()
            }
        );
        assert_eq!(route_for_path("/v1/extensions//status"), Route::Unsupported);
        assert_eq!(
            route_for_path("/v1/extensions/a/b/status"),
            Route::Unsupported
        );
        assert_eq!(
            route_for_path("/v1/extensions//rollback"),
            Route::Unsupported
        );
        assert_eq!(
            route_for_path("/v1/extensions/a/b/rollback"),
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
        let (addr, from_env) = listen_addr().expect("listen addr");
        assert!(!from_env);
        assert!(is_loopback(&addr));
        assert_eq!(addr.port(), 8080);
        assert!(addr.ip().is_loopback());
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
                    goal: "first".to_string(),
                    path_scope: None,
                    requested_capabilities: Vec::new(),
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
                    goal: "between".to_string(),
                    path_scope: None,
                    requested_capabilities: Vec::new(),
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
                    goal: "second".to_string(),
                    path_scope: None,
                    requested_capabilities: Vec::new(),
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
                    goal: "first".to_string(),
                    path_scope: None,
                    requested_capabilities: Vec::new(),
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
                    goal: "second".to_string(),
                    path_scope: None,
                    requested_capabilities: Vec::new(),
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
            "/v1/jobs/not-a-job-id/cancel",
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
        assert_eq!(cancel_response.status(), StatusCode::BAD_REQUEST);

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
                "actor": {
                    "type": "user",
                    "id": "user:tool-output",
                    "display_name": "Tool Output User"
                },
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
        assert_eq!(
            update_payload["data"]["change"]["actor"]["id"],
            Value::String("user:tool-output".to_string())
        );
        assert_eq!(
            update_payload["data"]["change"]["actor"]["type"],
            Value::String("user".to_string())
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

        let install_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/extensions/install",
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

        let update_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/extensions/update",
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
            "/v1/extensions/com.example.review.extension/status",
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

        let rollback_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/extensions/com.example.review.extension/rollback",
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
            "/v1/extensions/com.example.review.extension/disable",
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
            "/v1/extensions/com.example.review.extension/enable",
        )
        .await;
        assert_eq!(enable_response.status(), StatusCode::OK);
        let enable_payload = to_bytes(enable_response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice::<Value>(&bytes).expect("response json"))
            .expect("response bytes");
        assert_eq!(enable_payload["data"]["record"]["status"], "installed");

        let block_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/extensions/blocklist/apply",
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
            "/v1/extensions/com.example.review.extension/status",
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

        let list_response = send_json_request(
            &rpc_server,
            Method::POST,
            "/v1/extensions/list",
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

        let remove_response = send_request(
            &rpc_server,
            Method::POST,
            "/v1/extensions/com.example.review.extension/remove",
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
            "/v1/extensions/com.example.review.extension/status",
        )
        .await;
        assert_eq!(removed_status_response.status(), StatusCode::NOT_FOUND);
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
                    goal: "render tool output".to_string(),
                    resource_scope: None,
                    requested_capabilities: Vec::new(),
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

        let app = build_router(rpc_server.clone());
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
    async fn oversized_list_repositories_request_returns_too_large() {
        let rpc_server = ready_rpc_server();
        let payload = vec![b'a'; MAX_REQUEST_BODY_BYTES + 1];
        let app = build_router(rpc_server);
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
        let server_task = tokio::spawn(run_listener(rpc_server.clone(), listener, shutdown_rx));

        let response = reqwest::get(format!("http://{addr}/v1/health"))
            .await
            .expect("request should succeed")
            .json::<Value>()
            .await
            .expect("response json");

        assert_eq!(response["status"], "ok");
        assert!(response["data"]["daemon_status"].is_string());
        assert!(response["data"]["capabilities"].is_array());

        shutdown_tx.send(()).expect("shutdown signal");
        let _ = server_task
            .await
            .expect("server task should complete after shutdown");
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
