use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::Arc;

use crate::rpc;
use anyhow::{anyhow, Context, Result};
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
    ListRepositories,
    InstallExtension,
    ListExtensions,
    ExtensionStatus { extension_id: String },
    RollbackExtension { extension_id: String },
    ApplyBlocklist,
    ListBlocklist,
    RenderToolOutput,
    Unsupported,
}

fn route_for_path(path: &str) -> Route {
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
    match path {
        "/v1/health" => Route::Health,
        "/v1/repositories:list" => Route::ListRepositories,
        "/v1/extensions/install" => Route::InstallExtension,
        "/v1/extensions/list" => Route::ListExtensions,
        "/v1/extensions/blocklist/apply" => Route::ApplyBlocklist,
        "/v1/extensions/blocklist/list" => Route::ListBlocklist,
        "/v1/tool-results:render" => Route::RenderToolOutput,
        _ => Route::Unsupported,
    }
}

fn valid_extension_id(extension_id: &str) -> Option<&str> {
    if extension_id.is_empty() || extension_id.contains('/') {
        None
    } else {
        Some(extension_id)
    }
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

fn serialize_install_extension_response(
    response: rpc::InstallExtensionResponse,
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
        .route("/v1/repositories:list", any(dispatch_route))
        .route("/v1/extensions/install", any(dispatch_route))
        .route("/v1/extensions/list", any(dispatch_route))
        .route("/v1/extensions/blocklist/apply", any(dispatch_route))
        .route("/v1/extensions/blocklist/list", any(dispatch_route))
        .route("/v1/extensions/*path", any(dispatch_route))
        .route("/v1/tool-results:render", any(dispatch_route))
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
    use std::sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, MutexGuard,
    };
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
        assert_eq!(route_for_path("/v1/health"), Route::Health);
        assert_eq!(
            route_for_path("/v1/repositories:list"),
            Route::ListRepositories
        );
        assert_eq!(
            route_for_path("/v1/extensions/install"),
            Route::InstallExtension
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
        assert_eq!(route_for_path("/unknown"), Route::Unsupported);
        assert_eq!(route_for_path("/v1/health/"), Route::Unsupported);
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
            "/v1/extensions/install",
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
                        include_policy: Some(true),
                        ..atelia_core::ToolOutputOverrides::default()
                    },
                    "Expose policy fields in rendered tool output".to_string(),
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
            false
        );
        assert!(payload["data"]["rendered_output_metadata"]["fallback_reason"].is_null());
        assert!(payload["data"]["rendered_output"]
            .as_str()
            .expect("rendered output string")
            .contains("policy.state"));
        let _ = fs::remove_dir_all(root);
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
