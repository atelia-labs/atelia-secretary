//! Daemon service skeleton for Atelia Secretary (Slice 4).
//!
//! Owns daemon health/status metadata, an in-memory
//! [`SecretaryRuntime`], and exposes a synchronous service API for health
//! checks and repository registration/listing.

use atelia_core::{
    DefaultPolicyEngine, InMemoryStore, LedgerTimestamp, RepositoryId, RepositoryRecord,
    RepositoryTrustState, SecretaryRuntime, SecretaryStore,
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

// ---------------------------------------------------------------------------
// Service errors
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ServiceError {
    Store(atelia_core::StoreError),
    InvalidArgument { reason: String },
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(err) => write!(f, "{err}"),
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

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Thin service façade over an in-memory [`SecretaryRuntime`].
///
/// All methods are synchronous because the underlying store and policy engine
/// are synchronous. The Tokio entrypoint in `main.rs` wraps this in an async
/// runtime for signal handling only.
pub struct SecretaryService {
    runtime: SecretaryRuntime<InMemoryStore, DefaultPolicyEngine>,
    started_at: LedgerTimestamp,
    daemon_status: DaemonStatus,
}

impl SecretaryService {
    /// Create a new service backed by an in-memory store and default policy.
    pub fn new() -> Self {
        Self {
            runtime: SecretaryRuntime::in_memory(),
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
        let (repository_count, storage_status) = match self.runtime.store().list_repositories() {
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
            capabilities: vec!["health.v1".to_string(), "repositories.v1".to_string()],
            repository_count,
            started_at: self.started_at,
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
        self.runtime
            .store()
            .create_repository(record.clone())
            .map_err(|err| match err {
                atelia_core::StoreError::DuplicateId {
                    collection: "repositories",
                    ..
                } => ServiceError::InvalidArgument {
                    reason: "root_path is already registered".to_string(),
                },
                err => ServiceError::Store(err),
            })?;
        Ok(record)
    }

    /// List all registered repositories.
    #[allow(dead_code)]
    pub fn list_repositories(&self) -> ServiceResult<Vec<RepositoryRecord>> {
        Ok(self.runtime.store().list_repositories()?)
    }

    /// Look up a single repository by id.
    #[allow(dead_code)]
    pub fn get_repository(&self, id: &RepositoryId) -> ServiceResult<RepositoryRecord> {
        Ok(self.runtime.store().get_repository(id)?)
    }
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

        assert!(matches!(err, ServiceError::InvalidArgument { .. }));
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
