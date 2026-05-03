//! Daemon service skeleton for Atelia Secretary (Slice 4).
//!
//! Owns daemon health/status metadata, an in-memory job lifecycle runtime, and
//! exposes a synchronous service API for health checks, repository
//! registration/listing, and the first supported job lifecycle calls.

use atelia_core::{
    Actor, CancelJobReceipt, DefaultPolicyEngine, InMemoryStore, JobId, JobKind,
    JobLifecycleService, JobPage, JobQuery, JobRecord, JobStatus, LedgerTimestamp, RepositoryId,
    RepositoryRecord, RepositoryTrustState, ResourceScope, RuntimeError, RuntimeJobReceipt,
    RuntimeJobRequest, SecretaryStore, StoreError,
};
use std::path::PathBuf;

const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "1.0.0";
const STORAGE_VERSION: &str = "0.1.0";

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

// ---------------------------------------------------------------------------
// Service errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ServiceError {
    Conflict { reason: String },
    Store(atelia_core::StoreError),
    Runtime(RuntimeError),
    InvalidArgument { reason: String },
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { reason } => write!(f, "conflict: {reason}"),
            Self::Store(err) => write!(f, "{err}"),
            Self::Runtime(err) => write!(f, "{err}"),
            Self::InvalidArgument { reason } => write!(f, "invalid argument: {reason}"),
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
pub struct ListRepositoriesRequest {
    pub trust_state: Option<RepositoryTrustState>,
    pub page_size: Option<usize>,
    pub page_token: Option<String>,
}

impl Default for ListRepositoriesRequest {
    fn default() -> Self {
        Self {
            trust_state: None,
            page_size: None,
            page_token: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ListRepositoriesPage {
    pub repositories: Vec<RepositoryRecord>,
    pub next_page_token: Option<String>,
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
}

impl SecretaryService {
    /// Create a new service backed by an in-memory store and default policy.
    pub fn new() -> Self {
        Self {
            lifecycle: JobLifecycleService::in_memory(),
            started_at: LedgerTimestamp::now(),
            daemon_status: DaemonStatus::Starting,
        }
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
            capabilities: vec![
                "health.v1".to_string(),
                "repositories.v1".to_string(),
                "jobs.v1".to_string(),
            ],
            repository_count,
            started_at: self.started_at,
        }
    }

    pub fn protocol_metadata(&self) -> ProtocolMetadata {
        ProtocolMetadata {
            protocol_version: PROTOCOL_VERSION.to_string(),
            daemon_version: DAEMON_VERSION.to_string(),
            storage_version: STORAGE_VERSION.to_string(),
            capabilities: vec![
                "health.v1".to_string(),
                "repositories.v1".to_string(),
                "jobs.v1".to_string(),
            ],
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

        let record = RepositoryRecord::new(
            request.display_name,
            root_path,
            request.trust_state,
            LedgerTimestamp::now(),
        );
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

    /// Submit the first supported daemon job, backed by the core echo tool.
    #[allow(dead_code)]
    pub fn submit_job(&self, request: SubmitJobRequest) -> ServiceResult<RuntimeJobReceipt> {
        if request.goal.trim().is_empty() {
            return Err(ServiceError::InvalidArgument {
                reason: "goal must not be empty".to_string(),
            });
        }

        if !request.requested_capabilities.is_empty() {
            return Err(ServiceError::InvalidArgument {
                reason:
                    "requested_capabilities is not yet supported; submit one capability via a future-capable transport".to_string(),
            });
        }

        if request
            .idempotency_key
            .as_ref()
            .is_some_and(|key| !key.trim().is_empty())
        {
            return Err(ServiceError::InvalidArgument {
                reason: "idempotency_key is not yet supported by this daemon runtime".to_string(),
            });
        }

        let runtime_request = RuntimeJobRequest::new(
            request.requester,
            request.repository_id,
            request.kind,
            request.goal,
        )
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

        Ok(self.lifecycle.submit_echo_job(runtime_request)?)
    }

    /// List jobs with optional repository/status filtering.
    #[allow(dead_code)]
    pub fn list_jobs(
        &self,
        repository_id: Option<RepositoryId>,
        status: Option<JobStatus>,
        page_size: Option<usize>,
        page_token: Option<String>,
    ) -> ServiceResult<JobPage> {
        Ok(self.lifecycle.query_jobs(JobQuery {
            repository_id,
            status,
            requester: None,
            page_size,
            page_token,
        })?)
    }

    /// Look up a single job by id.
    #[allow(dead_code)]
    pub fn get_job(&self, id: &JobId) -> ServiceResult<JobRecord> {
        Ok(self.lifecycle.get_job(id)?)
    }

    /// Request cancellation for a queued/running job.
    #[allow(dead_code)]
    pub fn cancel_job(
        &self,
        id: &JobId,
        reason: impl Into<String>,
        _requester: Option<Actor>,
    ) -> ServiceResult<CancelJobReceipt> {
        Ok(self.lifecycle.cancel_job(id, reason)?)
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
    use atelia_core::RepositoryTrustState;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

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
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn register_rejects_duplicate_canonical_root() {
        let svc = ready_service();
        let root = test_repo_dir("duplicate");

        svc.register_repository(RegisterRepositoryRequest {
            display_name: "repo-a".to_string(),
            root_path: root.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::Trusted,
        })
        .expect("first register should succeed");

        let err = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "repo-b".to_string(),
                root_path: root.join(".").to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::ReadOnly,
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
            })
            .expect("register a");
        let r2 = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "repo-b".to_string(),
                root_path: root_b.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::ReadOnly,
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
        })
        .expect("register trusted should succeed");
        svc.register_repository(RegisterRepositoryRequest {
            display_name: "repo-b".to_string(),
            root_path: root_b.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::ReadOnly,
        })
        .expect("register read-only should succeed");
        svc.register_repository(RegisterRepositoryRequest {
            display_name: "repo-c".to_string(),
            root_path: root_c.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::Trusted,
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
    fn health_updates_repository_count() {
        let svc = ready_service();
        let root_a = test_repo_dir("health-a");
        let root_b = test_repo_dir("health-b");

        assert_eq!(svc.health().repository_count, 0);

        svc.register_repository(RegisterRepositoryRequest {
            display_name: "repo-a".to_string(),
            root_path: root_a.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::Trusted,
        })
        .expect("register a");

        assert_eq!(svc.health().repository_count, 1);

        svc.register_repository(RegisterRepositoryRequest {
            display_name: "repo-b".to_string(),
            root_path: root_b.to_string_lossy().to_string(),
            trust_state: RepositoryTrustState::Trusted,
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

    // -- job lifecycle API --------------------------------------------------

    fn actor() -> Actor {
        Actor::Agent {
            id: "agent:test".to_string(),
            display_name: Some("Test Agent".to_string()),
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
            .list_jobs(Some(repository.id), Some(JobStatus::Succeeded), None, None)
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
                Some(1),
                first.next_page_token,
            )
            .expect("list jobs should succeed");
        assert_eq!(second.jobs.len(), 1);
        assert_ne!(second.jobs[0].id, first.jobs[0].id);

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
    fn submit_job_rejects_requested_capabilities() {
        let svc = ready_service();
        let root = test_repo_dir("unsupported-capabilities");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
            })
            .expect("register should succeed");

        let err = svc
            .submit_job(SubmitJobRequest {
                requester: actor(),
                repository_id: repository.id,
                kind: JobKind::Read,
                goal: "summarize".to_string(),
                resource_scope: None,
                requested_capabilities: vec!["capability.discovery".to_string()],
                idempotency_key: None,
            })
            .unwrap_err();

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn submit_job_rejects_idempotency_key() {
        let svc = ready_service();
        let root = test_repo_dir("unsupported-idempotency");
        let repository = svc
            .register_repository(RegisterRepositoryRequest {
                display_name: "job-repo".to_string(),
                root_path: root.to_string_lossy().to_string(),
                trust_state: RepositoryTrustState::Trusted,
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

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
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
