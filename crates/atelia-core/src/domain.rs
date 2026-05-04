//! Runtime domain records for the Secretary ledger.

use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

const DOMAIN_SCHEMA_VERSION: u32 = 1;

macro_rules! opaque_id {
    ($name:ident, $prefix:literal) => {
        #[derive(Debug, Clone, Serialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(format!("{}{}", $prefix, Uuid::new_v4()))
            }

            pub fn try_from_string(value: impl Into<String>) -> Result<Self, InvalidIdError> {
                let value = value.into();
                let uuid_part = value.strip_prefix($prefix).ok_or_else(|| {
                    InvalidIdError::new(stringify!($name), $prefix, value.clone())
                })?;

                if uuid_part.is_empty() || Uuid::parse_str(uuid_part).is_err() {
                    return Err(InvalidIdError::new(stringify!($name), $prefix, value));
                }

                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }

            pub fn has_valid_prefix(&self) -> bool {
                self.0.starts_with($prefix)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_from_string(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InvalidIdError {
    pub type_name: &'static str,
    pub expected_prefix: &'static str,
    pub value: String,
}

impl InvalidIdError {
    fn new(type_name: &'static str, expected_prefix: &'static str, value: String) -> Self {
        Self {
            type_name,
            expected_prefix,
            value,
        }
    }
}

impl fmt::Display for InvalidIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} must be a prefixed opaque id starting with {} followed by a UUID",
            self.type_name, self.expected_prefix
        )
    }
}

impl Error for InvalidIdError {}

opaque_id!(RepositoryId, "repo_");
opaque_id!(JobId, "job_");
opaque_id!(JobEventId, "evt_");
opaque_id!(PolicyDecisionId, "pol_");
opaque_id!(LockDecisionId, "lock_");
opaque_id!(ToolInvocationId, "tool_");
opaque_id!(ToolResultId, "res_");
opaque_id!(AuditRecordId, "aud_");
opaque_id!(SchemaMigrationId, "mig_");
opaque_id!(OutputRefId, "out_");
opaque_id!(ArtifactRefId, "art_");

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct LedgerTimestamp {
    pub unix_millis: i64,
}

impl LedgerTimestamp {
    pub fn now() -> Self {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
            .unwrap_or(0);

        Self {
            unix_millis: millis,
        }
    }

    pub fn from_unix_millis(unix_millis: i64) -> Self {
        Self { unix_millis }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RedactionMarker {
    pub field_path: String,
    pub reason: String,
    pub redacted_at: LedgerTimestamp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TruncationMetadata {
    pub original_bytes: u64,
    pub retained_bytes: u64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OutputRef {
    pub id: OutputRefId,
    #[serde(default)]
    pub uri: String,
    pub media_type: String,
    pub label: Option<String>,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRef {
    pub id: ArtifactRefId,
    #[serde(default)]
    pub uri: String,
    pub media_type: String,
    pub label: Option<String>,
    pub digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PathScope {
    pub root_path: String,
    pub allowed_paths: Vec<String>,
}

impl PathScope {
    pub fn repository(root_path: impl Into<String>) -> Self {
        let root_path = root_path.into();

        Self {
            allowed_paths: vec![root_path.clone()],
            root_path,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryTrustState {
    Trusted,
    ReadOnly,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryMetadata {
    pub observed_at: LedgerTimestamp,
    pub vcs_kind: Option<String>,
    pub branch: Option<String>,
    pub head_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepositoryRecord {
    pub id: RepositoryId,
    pub schema_version: u32,
    pub created_at: LedgerTimestamp,
    pub updated_at: LedgerTimestamp,
    pub display_name: String,
    pub root_path: String,
    pub allowed_path_scope: PathScope,
    pub trust_state: RepositoryTrustState,
    pub owner_hint: Option<String>,
    pub last_observed_metadata: Option<RepositoryMetadata>,
    pub redactions: Vec<RedactionMarker>,
}

impl RepositoryRecord {
    pub fn new(
        display_name: impl Into<String>,
        root_path: impl Into<String>,
        trust_state: RepositoryTrustState,
        created_at: LedgerTimestamp,
    ) -> Self {
        let root_path = root_path.into();

        Self {
            id: RepositoryId::new(),
            schema_version: DOMAIN_SCHEMA_VERSION,
            created_at,
            updated_at: created_at,
            display_name: display_name.into(),
            allowed_path_scope: PathScope::repository(root_path.clone()),
            root_path,
            trust_state,
            owner_hint: None,
            last_observed_metadata: None,
            redactions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Read,
    Mutate,
    Process,
    Maintenance,
    Other { name: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Blocked,
    Canceled,
}

impl JobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Blocked | Self::Canceled
        )
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        if self == next {
            return true;
        }

        match self {
            Self::Queued => matches!(
                next,
                Self::Running | Self::Failed | Self::Blocked | Self::Canceled
            ),
            Self::Running => matches!(
                next,
                Self::Succeeded | Self::Failed | Self::Blocked | Self::Canceled
            ),
            Self::Succeeded | Self::Failed | Self::Blocked | Self::Canceled => false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CancellationState {
    NotRequested,
    Requested,
    CooperativeStop,
    ForceStop,
    Completed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicySummary {
    pub decision_id: Option<PolicyDecisionId>,
    pub outcome: PolicyOutcome,
    pub risk_tier: RiskTier,
    pub reason_code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobRecord {
    pub id: JobId,
    pub schema_version: u32,
    pub created_at: LedgerTimestamp,
    pub updated_at: LedgerTimestamp,
    pub started_at: Option<LedgerTimestamp>,
    pub completed_at: Option<LedgerTimestamp>,
    pub requester: Actor,
    pub repository_id: RepositoryId,
    pub kind: JobKind,
    pub goal: String,
    pub status: JobStatus,
    pub policy_summary: Option<PolicySummary>,
    pub cancellation_state: CancellationState,
    pub latest_event_id: Option<JobEventId>,
    pub redactions: Vec<RedactionMarker>,
}

impl JobRecord {
    pub fn new(
        requester: Actor,
        repository_id: RepositoryId,
        kind: JobKind,
        goal: impl Into<String>,
        created_at: LedgerTimestamp,
    ) -> Self {
        Self {
            id: JobId::new(),
            schema_version: DOMAIN_SCHEMA_VERSION,
            created_at,
            updated_at: created_at,
            started_at: None,
            completed_at: None,
            requester,
            repository_id,
            kind,
            goal: goal.into(),
            status: JobStatus::Queued,
            policy_summary: None,
            cancellation_state: CancellationState::NotRequested,
            latest_event_id: None,
            redactions: Vec::new(),
        }
    }

    pub fn transition_status(
        &mut self,
        next: JobStatus,
        at: LedgerTimestamp,
    ) -> Result<(), JobStatusTransitionError> {
        if !self.status.can_transition_to(next) {
            return Err(JobStatusTransitionError::InvalidTransition {
                from: self.status,
                to: next,
            });
        }

        self.ensure_monotonic_transition_timestamp(at)?;

        self.status = next;
        self.updated_at = at;

        if next == JobStatus::Running && self.started_at.is_none() {
            self.started_at = Some(at);
        }

        if next.is_terminal() && self.completed_at.is_none() {
            self.completed_at = Some(at);
        }

        Ok(())
    }

    fn ensure_monotonic_transition_timestamp(
        &self,
        at: LedgerTimestamp,
    ) -> Result<(), JobStatusTransitionError> {
        let latest = [Some(self.updated_at), self.started_at, self.completed_at]
            .into_iter()
            .flatten()
            .max()
            .unwrap_or(self.updated_at);

        if at < latest {
            return Err(JobStatusTransitionError::NonMonotonicTimestamp { at, latest });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatusTransitionError {
    InvalidTransition {
        from: JobStatus,
        to: JobStatus,
    },
    NonMonotonicTimestamp {
        at: LedgerTimestamp,
        latest: LedgerTimestamp,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventSubjectType {
    Repository,
    Job,
    PolicyDecision,
    LockDecision,
    ToolInvocation,
    ToolResult,
    AuditRecord,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EventSubject {
    pub subject_type: EventSubjectType,
    pub subject_id: String,
}

impl EventSubject {
    pub fn repository(id: &RepositoryId) -> Self {
        Self::new(EventSubjectType::Repository, id.as_str())
    }

    pub fn job(id: &JobId) -> Self {
        Self::new(EventSubjectType::Job, id.as_str())
    }

    pub fn policy_decision(id: &PolicyDecisionId) -> Self {
        Self::new(EventSubjectType::PolicyDecision, id.as_str())
    }

    pub fn lock_decision(id: &LockDecisionId) -> Self {
        Self::new(EventSubjectType::LockDecision, id.as_str())
    }

    pub fn tool_invocation(id: &ToolInvocationId) -> Self {
        Self::new(EventSubjectType::ToolInvocation, id.as_str())
    }

    pub fn tool_result(id: &ToolResultId) -> Self {
        Self::new(EventSubjectType::ToolResult, id.as_str())
    }

    pub fn audit_record(id: &AuditRecordId) -> Self {
        Self::new(EventSubjectType::AuditRecord, id.as_str())
    }

    fn new(subject_type: EventSubjectType, subject_id: &str) -> Self {
        Self {
            subject_type,
            subject_id: subject_id.to_string(),
        }
    }

    pub fn has_valid_subject_id(&self) -> bool {
        match self.subject_type {
            EventSubjectType::Repository => RepositoryId::try_from_string(&self.subject_id).is_ok(),
            EventSubjectType::Job => JobId::try_from_string(&self.subject_id).is_ok(),
            EventSubjectType::PolicyDecision => {
                PolicyDecisionId::try_from_string(&self.subject_id).is_ok()
            }
            EventSubjectType::LockDecision => {
                LockDecisionId::try_from_string(&self.subject_id).is_ok()
            }
            EventSubjectType::ToolInvocation => {
                ToolInvocationId::try_from_string(&self.subject_id).is_ok()
            }
            EventSubjectType::ToolResult => ToolResultId::try_from_string(&self.subject_id).is_ok(),
            EventSubjectType::AuditRecord => {
                AuditRecordId::try_from_string(&self.subject_id).is_ok()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventSeverity {
    Debug,
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobEventKind {
    JobSubmitted,
    JobStatusChanged { from: JobStatus, to: JobStatus },
    PolicyDecided { outcome: PolicyOutcome },
    LockHeld,
    LockReleased,
    LockReclaimed,
    ToolInvoked { tool_id: String },
    ToolResultRecorded { status: ToolResultStatus },
    AuditRecorded,
    CancellationRequested,
    RecoveryActionRecorded,
    Message,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EventRefs {
    pub repository_id: Option<RepositoryId>,
    pub job_id: Option<JobId>,
    pub policy_decision_id: Option<PolicyDecisionId>,
    pub lock_decision_id: Option<LockDecisionId>,
    pub tool_invocation_id: Option<ToolInvocationId>,
    pub tool_result_id: Option<ToolResultId>,
    pub audit_record_id: Option<AuditRecordId>,
    pub output_refs: Vec<OutputRef>,
    pub artifact_refs: Vec<ArtifactRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobEvent {
    pub id: JobEventId,
    pub schema_version: u32,
    pub sequence_number: u64,
    pub created_at: LedgerTimestamp,
    pub subject: EventSubject,
    pub kind: JobEventKind,
    pub severity: EventSeverity,
    pub public_message: String,
    pub refs: EventRefs,
    pub redactions: Vec<RedactionMarker>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RiskTier {
    R0,
    R1,
    R2,
    R3,
    R4,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PolicyOutcome {
    Allowed,
    Audited,
    NeedsApproval,
    Blocked,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceScope {
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PolicyDecision {
    pub id: PolicyDecisionId,
    pub schema_version: u32,
    pub created_at: LedgerTimestamp,
    pub requester: Actor,
    pub repository_id: RepositoryId,
    pub requested_capability: String,
    pub resource_scope: ResourceScope,
    pub tool_id: Option<String>,
    pub provider_id: Option<String>,
    pub declared_effect: String,
    pub current_trust_state: RepositoryTrustState,
    pub approval_available: bool,
    pub policy_version: String,
    pub outcome: PolicyOutcome,
    pub risk_tier: RiskTier,
    pub reason_code: String,
    pub user_reason: String,
    pub approval_request_ref: Option<OutputRef>,
    pub audit_ref: Option<AuditRecordId>,
    pub redactions: Vec<RedactionMarker>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LockOwner {
    Job(JobId),
    Process { id: String },
    System { id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum LockedScope {
    Repository,
    Path { path: String },
    PathPattern { pattern: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LockStatus {
    Held,
    Released,
    Expired,
    Reclaimed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LockDecision {
    pub id: LockDecisionId,
    pub schema_version: u32,
    pub created_at: LedgerTimestamp,
    pub updated_at: LedgerTimestamp,
    pub repository_id: RepositoryId,
    pub policy_decision_id: PolicyDecisionId,
    pub owner: LockOwner,
    pub locked_scope: LockedScope,
    pub locked_at: LedgerTimestamp,
    pub expires_at: LedgerTimestamp,
    pub released_at: Option<LedgerTimestamp>,
    pub reclaimed_at: Option<LedgerTimestamp>,
    pub status: LockStatus,
    pub redactions: Vec<RedactionMarker>,
}

impl LockDecision {
    pub fn new(
        repository_id: RepositoryId,
        policy_decision_id: PolicyDecisionId,
        owner: LockOwner,
        locked_scope: LockedScope,
        locked_at: LedgerTimestamp,
        expires_at: LedgerTimestamp,
    ) -> Result<Self, LockDecisionCreateError> {
        if expires_at <= locked_at {
            return Err(LockDecisionCreateError::NonPositiveDuration {
                locked_at,
                expires_at,
            });
        }

        Ok(Self {
            id: LockDecisionId::new(),
            schema_version: DOMAIN_SCHEMA_VERSION,
            created_at: locked_at,
            updated_at: locked_at,
            repository_id,
            policy_decision_id,
            owner,
            locked_scope,
            locked_at,
            expires_at,
            released_at: None,
            reclaimed_at: None,
            status: LockStatus::Held,
            redactions: Vec::new(),
        })
    }

    pub fn reclaim(
        &mut self,
        owner: &LockOwner,
        reclaimed_at: LedgerTimestamp,
    ) -> Result<bool, LockReclaimError> {
        if owner != &self.owner {
            return Err(LockReclaimError::OwnerMismatch);
        }

        match self.status {
            LockStatus::Reclaimed => Ok(false),
            LockStatus::Held | LockStatus::Expired => {
                self.ensure_monotonic_reclaim_timestamp(reclaimed_at)?;
                if reclaimed_at < self.expires_at {
                    return Err(LockReclaimError::NotExpired {
                        reclaimed_at,
                        expires_at: self.expires_at,
                    });
                }
                self.status = LockStatus::Reclaimed;
                self.reclaimed_at = Some(reclaimed_at);
                self.updated_at = reclaimed_at;
                Ok(true)
            }
            LockStatus::Released => Err(LockReclaimError::AlreadyReleased),
        }
    }

    fn ensure_monotonic_reclaim_timestamp(
        &self,
        reclaimed_at: LedgerTimestamp,
    ) -> Result<(), LockReclaimError> {
        if reclaimed_at < self.updated_at {
            return Err(LockReclaimError::NonMonotonicTimestamp {
                at: reclaimed_at,
                latest: self.updated_at,
            });
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LockDecisionCreateError {
    NonPositiveDuration {
        locked_at: LedgerTimestamp,
        expires_at: LedgerTimestamp,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LockReclaimError {
    OwnerMismatch,
    AlreadyReleased,
    NotExpired {
        reclaimed_at: LedgerTimestamp,
        expires_at: LedgerTimestamp,
    },
    NonMonotonicTimestamp {
        at: LedgerTimestamp,
        latest: LedgerTimestamp,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedPath {
    pub requested_path: String,
    pub resolved_path: String,
    pub display_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolInvocation {
    pub id: ToolInvocationId,
    pub schema_version: u32,
    pub created_at: LedgerTimestamp,
    pub job_id: JobId,
    pub repository_id: RepositoryId,
    pub policy_decision_id: PolicyDecisionId,
    pub actor: Actor,
    pub tool_id: String,
    pub requested_capability: String,
    pub args_summary: String,
    pub resolved_paths: Vec<ResolvedPath>,
    pub timeout_millis: Option<u64>,
    pub redactions: Vec<RedactionMarker>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolResultStatus {
    Succeeded,
    Failed,
    Canceled,
    TimedOut,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StructuredValue {
    Null,
    Bool(bool),
    Integer(i64),
    String(String),
    StringList(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResultField {
    pub key: String,
    pub value: StructuredValue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolResult {
    pub id: ToolResultId,
    pub schema_version: u32,
    pub created_at: LedgerTimestamp,
    pub invocation_id: ToolInvocationId,
    pub tool_id: String,
    pub status: ToolResultStatus,
    pub schema_ref: Option<String>,
    pub fields: Vec<ToolResultField>,
    pub evidence_refs: Vec<ArtifactRef>,
    pub output_refs: Vec<OutputRef>,
    pub truncation: Option<TruncationMetadata>,
    pub redactions: Vec<RedactionMarker>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditRecord {
    pub id: AuditRecordId,
    pub schema_version: u32,
    pub created_at: LedgerTimestamp,
    pub actor: Actor,
    pub repository_id: RepositoryId,
    pub requested_capability: String,
    pub policy_decision_id: PolicyDecisionId,
    pub tool_invocation_id: Option<ToolInvocationId>,
    pub effect_summary: String,
    pub output_refs: Vec<OutputRef>,
    pub redactions: Vec<RedactionMarker>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SchemaMigrationStatus {
    Applied,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SchemaMigrationRecord {
    pub id: SchemaMigrationId,
    pub schema_version: u32,
    pub created_at: LedgerTimestamp,
    pub updated_at: LedgerTimestamp,
    pub migration_name: String,
    pub migration_version: u32,
    pub status: SchemaMigrationStatus,
    pub leader_id: Option<String>,
    pub notes: Option<String>,
    pub redactions: Vec<RedactionMarker>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::de::{value::StrDeserializer, IntoDeserializer};
    use serde::ser::{Error as SerError, Impossible, Serialize, Serializer};
    use serde::Deserialize;
    use serde_json::{from_str, to_string};
    use std::fmt;

    #[derive(Debug)]
    struct TestSerdeError(String);

    impl fmt::Display for TestSerdeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for TestSerdeError {}

    impl SerError for TestSerdeError {
        fn custom<T: fmt::Display>(msg: T) -> Self {
            Self(msg.to_string())
        }
    }

    struct StringRoundTripSerializer;

    impl Serializer for StringRoundTripSerializer {
        type Ok = String;
        type Error = TestSerdeError;
        type SerializeSeq = Impossible<String, TestSerdeError>;
        type SerializeTuple = Impossible<String, TestSerdeError>;
        type SerializeTupleStruct = Impossible<String, TestSerdeError>;
        type SerializeTupleVariant = Impossible<String, TestSerdeError>;
        type SerializeMap = Impossible<String, TestSerdeError>;
        type SerializeStruct = Impossible<String, TestSerdeError>;
        type SerializeStructVariant = Impossible<String, TestSerdeError>;

        fn serialize_str(self, value: &str) -> Result<Self::Ok, Self::Error> {
            Ok(value.to_owned())
        }

        fn serialize_unit_variant(
            self,
            _name: &'static str,
            _variant_index: u32,
            variant: &'static str,
        ) -> Result<Self::Ok, Self::Error> {
            Ok(variant.to_owned())
        }

        fn serialize_bool(self, _v: bool) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported bool"))
        }

        fn serialize_i8(self, _v: i8) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported i8"))
        }

        fn serialize_i16(self, _v: i16) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported i16"))
        }

        fn serialize_i32(self, _v: i32) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported i32"))
        }

        fn serialize_i64(self, _v: i64) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported i64"))
        }

        fn serialize_u8(self, _v: u8) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported u8"))
        }

        fn serialize_u16(self, _v: u16) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported u16"))
        }

        fn serialize_u32(self, _v: u32) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported u32"))
        }

        fn serialize_u64(self, _v: u64) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported u64"))
        }

        fn serialize_f32(self, _v: f32) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported f32"))
        }

        fn serialize_f64(self, _v: f64) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported f64"))
        }

        fn serialize_char(self, _v: char) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported char"))
        }

        fn serialize_bytes(self, _v: &[u8]) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported bytes"))
        }

        fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported none"))
        }

        fn serialize_some<T: ?Sized + Serialize>(
            self,
            _value: &T,
        ) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported some"))
        }

        fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported unit"))
        }

        fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported unit struct"))
        }

        fn serialize_newtype_struct<T: ?Sized + Serialize>(
            self,
            _name: &'static str,
            value: &T,
        ) -> Result<Self::Ok, Self::Error> {
            value.serialize(self)
        }

        fn serialize_newtype_variant<T: ?Sized + Serialize>(
            self,
            _name: &'static str,
            _variant_index: u32,
            _variant: &'static str,
            _value: &T,
        ) -> Result<Self::Ok, Self::Error> {
            Err(TestSerdeError::custom("unsupported newtype variant"))
        }

        fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
            Err(TestSerdeError::custom("unsupported seq"))
        }

        fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Self::Error> {
            Err(TestSerdeError::custom("unsupported tuple"))
        }

        fn serialize_tuple_struct(
            self,
            _name: &'static str,
            _len: usize,
        ) -> Result<Self::SerializeTupleStruct, Self::Error> {
            Err(TestSerdeError::custom("unsupported tuple struct"))
        }

        fn serialize_tuple_variant(
            self,
            _name: &'static str,
            _variant_index: u32,
            _variant: &'static str,
            _len: usize,
        ) -> Result<Self::SerializeTupleVariant, Self::Error> {
            Err(TestSerdeError::custom("unsupported tuple variant"))
        }

        fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
            Err(TestSerdeError::custom("unsupported map"))
        }

        fn serialize_struct(
            self,
            _name: &'static str,
            _len: usize,
        ) -> Result<Self::SerializeStruct, Self::Error> {
            Err(TestSerdeError::custom("unsupported struct"))
        }

        fn serialize_struct_variant(
            self,
            _name: &'static str,
            _variant_index: u32,
            _variant: &'static str,
            _len: usize,
        ) -> Result<Self::SerializeStructVariant, Self::Error> {
            Err(TestSerdeError::custom("unsupported struct variant"))
        }
    }

    fn string_round_trip<T>(value: &T) -> T
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
        let serialized = value.serialize(StringRoundTripSerializer).unwrap();
        let deserializer: StrDeserializer<'_, serde::de::value::Error> =
            serialized.as_str().into_deserializer();

        T::deserialize(deserializer).unwrap()
    }

    fn assert_serde_record<T>()
    where
        T: Serialize + for<'de> Deserialize<'de>,
    {
    }

    #[test]
    fn opaque_ids_have_expected_prefixes_and_round_trip_through_serde() {
        let repo_id = RepositoryId::new();
        let job_id = JobId::new();
        let event_id = JobEventId::new();
        let policy_id = PolicyDecisionId::new();
        let lock_id = LockDecisionId::new();
        let invocation_id = ToolInvocationId::new();
        let result_id = ToolResultId::new();
        let audit_id = AuditRecordId::new();
        let output_id = OutputRefId::new();
        let artifact_id = ArtifactRefId::new();

        assert!(repo_id.has_valid_prefix());
        assert!(job_id.has_valid_prefix());
        assert!(event_id.has_valid_prefix());
        assert!(policy_id.has_valid_prefix());
        assert!(lock_id.has_valid_prefix());
        assert!(invocation_id.has_valid_prefix());
        assert!(result_id.has_valid_prefix());
        assert!(audit_id.has_valid_prefix());
        assert!(output_id.has_valid_prefix());
        assert!(artifact_id.has_valid_prefix());

        assert_eq!(repo_id, string_round_trip(&repo_id));
        assert_eq!(job_id, string_round_trip(&job_id));
        assert_eq!(
            repo_id,
            RepositoryId::try_from_string(repo_id.as_str()).unwrap()
        );
        assert!(RepositoryId::try_from_string(job_id.as_str()).is_err());
        assert!(RepositoryId::try_from_string("repo_not-a-uuid").is_err());

        let invalid_deserializer: StrDeserializer<'_, serde::de::value::Error> =
            "repo_not-a-uuid".into_deserializer();
        assert!(RepositoryId::deserialize(invalid_deserializer).is_err());
    }

    #[test]
    fn unit_enums_round_trip_through_serde() {
        assert_eq!(JobStatus::Queued, string_round_trip(&JobStatus::Queued));
        assert_eq!(
            PolicyOutcome::NeedsApproval,
            string_round_trip(&PolicyOutcome::NeedsApproval)
        );
        assert_eq!(RiskTier::R3, string_round_trip(&RiskTier::R3));
        assert_eq!(
            ToolResultStatus::TimedOut,
            string_round_trip(&ToolResultStatus::TimedOut)
        );
        assert_eq!(
            LockStatus::Reclaimed,
            string_round_trip(&LockStatus::Reclaimed)
        );

        assert_eq!(
            "read_only",
            RepositoryTrustState::ReadOnly
                .serialize(StringRoundTripSerializer)
                .unwrap()
        );
        assert_eq!(
            "needs_approval",
            PolicyOutcome::NeedsApproval
                .serialize(StringRoundTripSerializer)
                .unwrap()
        );
        assert_eq!(
            "job_submitted",
            JobEventKind::JobSubmitted
                .serialize(StringRoundTripSerializer)
                .unwrap()
        );
        assert_eq!(
            "R3",
            RiskTier::R3.serialize(StringRoundTripSerializer).unwrap()
        );
    }

    #[test]
    fn domain_records_are_serde_serializable() {
        assert_serde_record::<RepositoryRecord>();
        assert_serde_record::<JobRecord>();
        assert_serde_record::<JobEvent>();
        assert_serde_record::<PolicyDecision>();
        assert_serde_record::<LockDecision>();
        assert_serde_record::<ToolInvocation>();
        assert_serde_record::<ToolResult>();
        assert_serde_record::<AuditRecord>();
        assert_serde_record::<SchemaMigrationRecord>();
    }

    #[test]
    fn schema_migration_record_round_trips_through_serde() {
        let record = SchemaMigrationRecord {
            id: SchemaMigrationId::new(),
            schema_version: DOMAIN_SCHEMA_VERSION,
            created_at: LedgerTimestamp::from_unix_millis(10),
            updated_at: LedgerTimestamp::from_unix_millis(10),
            migration_name: "create_schema_migrations".to_string(),
            migration_version: 2024050401,
            status: SchemaMigrationStatus::Applied,
            leader_id: Some("daemon-1".to_string()),
            notes: Some("bootstrapped schema migrations".to_string()),
            redactions: Vec::new(),
        };

        let json = to_string(&record).unwrap();
        let round_tripped: SchemaMigrationRecord = from_str(&json).unwrap();

        assert_eq!(record, round_tripped);
    }

    #[test]
    fn output_ref_deserializes_without_uri() {
        let id = OutputRefId::new();
        let json = format!(
            r#"{{"id":"{}","media_type":"text/plain","label":null}}"#,
            id.as_str()
        );
        let output_ref: OutputRef = from_str(&json).unwrap();

        assert_eq!(id, output_ref.id);
        assert_eq!("", output_ref.uri);
        assert_eq!("text/plain", output_ref.media_type);
    }

    #[test]
    fn artifact_ref_deserializes_without_uri() {
        let id = ArtifactRefId::new();
        let json = format!(
            r#"{{"id":"{}","media_type":"application/json","digest":null}}"#,
            id.as_str()
        );
        let artifact_ref: ArtifactRef = from_str(&json).unwrap();

        assert_eq!(id, artifact_ref.id);
        assert_eq!("", artifact_ref.uri);
        assert_eq!("application/json", artifact_ref.media_type);
    }

    #[test]
    fn job_status_transition_rules_match_lifecycle_boundaries() {
        assert!(JobStatus::Queued.can_transition_to(JobStatus::Running));
        assert!(JobStatus::Queued.can_transition_to(JobStatus::Blocked));
        assert!(JobStatus::Running.can_transition_to(JobStatus::Succeeded));
        assert!(JobStatus::Running.can_transition_to(JobStatus::Canceled));

        assert!(!JobStatus::Queued.can_transition_to(JobStatus::Succeeded));
        assert!(!JobStatus::Succeeded.can_transition_to(JobStatus::Running));
        assert!(!JobStatus::Failed.can_transition_to(JobStatus::Canceled));
    }

    #[test]
    fn job_transition_helper_sets_lifecycle_timestamps() {
        let created_at = LedgerTimestamp::from_unix_millis(1000);
        let started_at = LedgerTimestamp::from_unix_millis(2000);
        let completed_at = LedgerTimestamp::from_unix_millis(3000);
        let mut job = JobRecord::new(
            Actor::User {
                id: "user-1".to_owned(),
                display_name: None,
            },
            RepositoryId::new(),
            JobKind::Read,
            "inspect repository",
            created_at,
        );

        job.transition_status(JobStatus::Running, started_at)
            .unwrap();
        job.transition_status(JobStatus::Succeeded, completed_at)
            .unwrap();

        assert_eq!(Some(started_at), job.started_at);
        assert_eq!(Some(completed_at), job.completed_at);
        assert_eq!(JobStatus::Succeeded, job.status);
    }

    #[test]
    fn job_transition_rejects_non_monotonic_timestamp_without_mutating() {
        let created_at = LedgerTimestamp::from_unix_millis(1000);
        let started_at = LedgerTimestamp::from_unix_millis(2000);
        let mut job = JobRecord::new(
            Actor::User {
                id: "user-1".to_owned(),
                display_name: None,
            },
            RepositoryId::new(),
            JobKind::Read,
            "inspect repository",
            created_at,
        );

        job.transition_status(JobStatus::Running, started_at)
            .unwrap();

        assert_eq!(
            Err(JobStatusTransitionError::NonMonotonicTimestamp {
                at: LedgerTimestamp::from_unix_millis(1500),
                latest: started_at,
            }),
            job.transition_status(
                JobStatus::Succeeded,
                LedgerTimestamp::from_unix_millis(1500)
            )
        );
        assert_eq!(JobStatus::Running, job.status);
        assert_eq!(started_at, job.updated_at);
        assert_eq!(None, job.completed_at);
    }

    #[test]
    fn event_subject_constructors_validate_expected_id_prefixes() {
        let repository_id = RepositoryId::new();
        let job_id = JobId::new();

        assert!(EventSubject::repository(&repository_id).has_valid_subject_id());
        assert!(EventSubject::job(&job_id).has_valid_subject_id());
        assert!(!EventSubject {
            subject_type: EventSubjectType::Job,
            subject_id: repository_id.as_str().to_string(),
        }
        .has_valid_subject_id());
    }

    #[test]
    fn lock_decision_rejects_non_positive_duration() {
        let locked_at = LedgerTimestamp::from_unix_millis(1000);

        assert_eq!(
            Err(LockDecisionCreateError::NonPositiveDuration {
                locked_at,
                expires_at: locked_at,
            }),
            LockDecision::new(
                RepositoryId::new(),
                PolicyDecisionId::new(),
                LockOwner::Job(JobId::new()),
                LockedScope::Repository,
                locked_at,
                locked_at,
            )
        );
    }

    #[test]
    fn lock_reclaim_is_idempotent_for_same_owner() {
        let owner = LockOwner::Job(JobId::new());
        let mut lock = LockDecision::new(
            RepositoryId::new(),
            PolicyDecisionId::new(),
            owner.clone(),
            LockedScope::Repository,
            LedgerTimestamp::from_unix_millis(1000),
            LedgerTimestamp::from_unix_millis(2000),
        )
        .unwrap();

        assert_eq!(
            Ok(true),
            lock.reclaim(&owner, LedgerTimestamp::from_unix_millis(3000))
        );
        assert_eq!(
            Ok(false),
            lock.reclaim(&owner, LedgerTimestamp::from_unix_millis(4000))
        );
        assert_eq!(LockStatus::Reclaimed, lock.status);
        assert_eq!(
            Some(LedgerTimestamp::from_unix_millis(3000)),
            lock.reclaimed_at
        );
    }

    #[test]
    fn lock_reclaim_rejects_non_monotonic_timestamp_without_mutating() {
        let owner = LockOwner::Job(JobId::new());
        let mut lock = LockDecision::new(
            RepositoryId::new(),
            PolicyDecisionId::new(),
            owner.clone(),
            LockedScope::Repository,
            LedgerTimestamp::from_unix_millis(1000),
            LedgerTimestamp::from_unix_millis(2000),
        )
        .unwrap();

        assert_eq!(
            Err(LockReclaimError::NonMonotonicTimestamp {
                at: LedgerTimestamp::from_unix_millis(500),
                latest: LedgerTimestamp::from_unix_millis(1000),
            }),
            lock.reclaim(&owner, LedgerTimestamp::from_unix_millis(500))
        );
        assert_eq!(LockStatus::Held, lock.status);
        assert_eq!(LedgerTimestamp::from_unix_millis(1000), lock.updated_at);
        assert_eq!(None, lock.reclaimed_at);
    }

    #[test]
    fn lock_reclaim_rejects_unexpired_lock_without_mutating() {
        let owner = LockOwner::Job(JobId::new());
        let mut lock = LockDecision::new(
            RepositoryId::new(),
            PolicyDecisionId::new(),
            owner.clone(),
            LockedScope::Repository,
            LedgerTimestamp::from_unix_millis(1000),
            LedgerTimestamp::from_unix_millis(3000),
        )
        .unwrap();

        assert_eq!(
            Err(LockReclaimError::NotExpired {
                reclaimed_at: LedgerTimestamp::from_unix_millis(2000),
                expires_at: LedgerTimestamp::from_unix_millis(3000),
            }),
            lock.reclaim(&owner, LedgerTimestamp::from_unix_millis(2000))
        );
        assert_eq!(LockStatus::Held, lock.status);
        assert_eq!(LedgerTimestamp::from_unix_millis(1000), lock.updated_at);
        assert_eq!(None, lock.reclaimed_at);
    }
}
