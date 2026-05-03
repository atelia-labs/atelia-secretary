//! Transport-neutral RPC boundary for the daemon.
//!
//! `ateliad` does not currently depend on tonic/prost server generation, so
//! this module keeps the proto-facing shape explicit without forcing a broad
//! dependency migration. A future transport layer should delegate to this
//! boundary rather than reimplementing service behavior.

#![allow(dead_code)]

use crate::service::{
    DaemonHealth, DaemonStatus, RegisterRepositoryRequest as ServiceRegisterRepositoryRequest,
    SecretaryService, ServiceError, StorageStatus, SubmitJobRequest as ServiceSubmitJobRequest,
};
use atelia_core::{
    Actor, CancellationState, JobId, JobKind, JobRecord, JobStatus, PolicyOutcome, RepositoryId,
    RepositoryRecord, RepositoryTrustState, RiskTier, StoreError,
};

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
        let repository = self
            .service
            .register_repository(ServiceRegisterRepositoryRequest {
                display_name: request.display_name,
                root_path: request.root_path,
                trust_state: request.trust_state,
            })?;

        Ok(RegisterRepositoryResponse {
            metadata: self.metadata(),
            repository: Repository::from(repository),
        })
    }

    pub fn list_repositories(
        &self,
        _request: ListRepositoriesRequest,
    ) -> RpcResult<ListRepositoriesResponse> {
        let repositories = self
            .service
            .list_repositories()?
            .into_iter()
            .map(Repository::from)
            .collect();

        Ok(ListRepositoriesResponse {
            metadata: self.metadata(),
            repositories,
        })
    }

    pub fn submit_job(&self, request: SubmitJobRequest) -> RpcResult<SubmitJobResponse> {
        let repository_id = parse_repository_id(&request.repository_id)?;
        let receipt = self.service.submit_job(ServiceSubmitJobRequest {
            requester: request.requester,
            repository_id,
            kind: parse_job_kind(&request.kind)?,
            goal: request.goal,
        })?;

        Ok(SubmitJobResponse {
            metadata: self.metadata(),
            job: Job::from(receipt.job),
            policy: PolicyDecision {
                decision_id: receipt.policy_decision.id.as_str().to_string(),
                outcome: policy_outcome_label(receipt.policy_decision.outcome).to_string(),
                risk_tier: risk_tier_label(receipt.policy_decision.risk_tier).to_string(),
                requested_capability: receipt.policy_decision.requested_capability,
                reason_code: receipt.policy_decision.reason_code,
                reason: receipt.policy_decision.user_reason,
            },
        })
    }

    pub fn get_job(&self, request: GetJobRequest) -> RpcResult<GetJobResponse> {
        let job_id = parse_job_id(&request.job_id)?;
        let job = self.service.get_job(&job_id)?;

        Ok(GetJobResponse {
            metadata: self.metadata(),
            job: Job::from(job),
        })
    }

    pub fn list_jobs(&self, request: ListJobsRequest) -> RpcResult<ListJobsResponse> {
        let repository_id = request
            .repository_id
            .as_deref()
            .map(parse_repository_id)
            .transpose()?;
        let page = self.service.list_jobs(repository_id, request.status)?;

        Ok(ListJobsResponse {
            metadata: self.metadata(),
            jobs: page.jobs.into_iter().map(Job::from).collect(),
            next_page_token: page.next_page_token,
        })
    }

    pub fn cancel_job(&self, request: CancelJobRequest) -> RpcResult<CancelJobResponse> {
        let job_id = parse_job_id(&request.job_id)?;
        let receipt = self.service.cancel_job(&job_id, request.reason)?;

        Ok(CancelJobResponse {
            metadata: self.metadata(),
            job: Job::from(receipt.job),
            event_count: receipt.events.len(),
        })
    }

    fn metadata(&self) -> ProtocolMetadata {
        ProtocolMetadata::from(self.service.health())
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
            ServiceError::Store(error) => store_error_to_rpc(error),
            ServiceError::Runtime(error) => Self {
                code: RpcErrorCode::Internal,
                reason: error.to_string(),
            },
        }
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
            status: daemon_status_label(health.daemon_status).to_string(),
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

impl From<DaemonHealth> for ProtocolMetadata {
    fn from(health: DaemonHealth) -> Self {
        Self {
            protocol_version: health.protocol_version,
            daemon_version: health.daemon_version,
            storage_version: health.storage_version,
            capabilities: health.capabilities,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterRepositoryRequest {
    pub display_name: String,
    pub root_path: String,
    pub trust_state: RepositoryTrustState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegisterRepositoryResponse {
    pub metadata: ProtocolMetadata,
    pub repository: Repository,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListRepositoriesRequest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListRepositoriesResponse {
    pub metadata: ProtocolMetadata,
    pub repositories: Vec<Repository>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repository {
    pub repository_id: String,
    pub display_name: String,
    pub root_path: String,
    pub trust_state: RepositoryTrustState,
    pub created_at_unix_ms: i64,
    pub updated_at_unix_ms: i64,
}

impl From<RepositoryRecord> for Repository {
    fn from(record: RepositoryRecord) -> Self {
        Self {
            repository_id: record.id.as_str().to_string(),
            display_name: record.display_name,
            root_path: record.root_path,
            trust_state: record.trust_state,
            created_at_unix_ms: record.created_at.unix_millis,
            updated_at_unix_ms: record.updated_at.unix_millis,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitJobRequest {
    pub repository_id: String,
    pub requester: Actor,
    pub kind: String,
    pub goal: String,
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
    pub status: Option<JobStatus>,
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
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelJobResponse {
    pub metadata: ProtocolMetadata,
    pub job: Job,
    pub event_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub job_id: String,
    pub repository_id: String,
    pub kind: String,
    pub goal: String,
    pub status: String,
    pub policy_summary: Option<PolicySummary>,
    pub created_at_unix_ms: i64,
    pub started_at_unix_ms: i64,
    pub completed_at_unix_ms: i64,
    pub latest_event_id: String,
    pub cancellation_state: String,
}

impl From<JobRecord> for Job {
    fn from(record: JobRecord) -> Self {
        Self {
            job_id: record.id.as_str().to_string(),
            repository_id: record.repository_id.as_str().to_string(),
            kind: job_kind_label(&record.kind).to_string(),
            goal: record.goal,
            status: job_status_label(record.status).to_string(),
            policy_summary: record.policy_summary.map(PolicySummary::from),
            created_at_unix_ms: record.created_at.unix_millis,
            started_at_unix_ms: record.started_at.map(|ts| ts.unix_millis).unwrap_or(0),
            completed_at_unix_ms: record.completed_at.map(|ts| ts.unix_millis).unwrap_or(0),
            latest_event_id: record
                .latest_event_id
                .map(|id| id.as_str().to_string())
                .unwrap_or_default(),
            cancellation_state: cancellation_state_label(record.cancellation_state).to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySummary {
    pub decision_id: String,
    pub outcome: String,
    pub risk_tier: String,
    pub reason_code: String,
}

impl From<atelia_core::PolicySummary> for PolicySummary {
    fn from(summary: atelia_core::PolicySummary) -> Self {
        Self {
            decision_id: summary
                .decision_id
                .map(|id| id.as_str().to_string())
                .unwrap_or_default(),
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
}

fn parse_repository_id(value: &str) -> RpcResult<RepositoryId> {
    RepositoryId::try_from_string(value.to_string())
        .map_err(|_| RpcError::invalid_argument("repository_id must be a valid repository id"))
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
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn ready_server() -> SecretaryRpcServer {
        let mut service = SecretaryService::new();
        service.set_ready();
        SecretaryRpcServer::new(service)
    }

    fn actor() -> Actor {
        Actor::Agent {
            id: "agent:test".to_string(),
            display_name: Some("Test Agent".to_string()),
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
    }

    #[test]
    fn register_list_submit_and_get_job_round_trip() {
        let server = ready_server();
        let root = test_repo_dir("round-trip");

        let registered = server
            .register_repository(RegisterRepositoryRequest {
                display_name: "rpc-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
            })
            .expect("register should succeed");

        let repositories = server
            .list_repositories(ListRepositoriesRequest)
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
                status: Some(JobStatus::Succeeded),
            })
            .expect("list jobs should succeed");
        assert_eq!(jobs.jobs.len(), 1);
        assert_eq!(jobs.jobs[0].job_id, submitted.job.job_id);

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
                trust_state: RepositoryTrustState::Trusted,
            })
            .expect("register should succeed");
        let submitted = server
            .submit_job(SubmitJobRequest {
                repository_id: registered.repository.repository_id,
                requester: actor(),
                kind: "read".to_string(),
                goal: "finish immediately".to_string(),
            })
            .expect("submit job should succeed");

        let error = server
            .cancel_job(CancelJobRequest {
                job_id: submitted.job.job_id,
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
            })
            .unwrap_err();

        assert_eq!(error.code, RpcErrorCode::InvalidArgument);
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
}
