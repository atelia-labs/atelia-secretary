//! Storage abstractions for Secretary runtime records.

use crate::domain::{
    AuditRecord, AuditRecordId, JobEvent, JobEventId, JobId, JobRecord, LockDecision,
    LockDecisionId, PolicyDecision, PolicyDecisionId, RepositoryId, RepositoryRecord,
    ToolInvocation, ToolInvocationId, ToolResult, ToolResultId,
};
use std::collections::{BTreeMap, HashMap};
use std::error::Error;
use std::fmt;
use std::hash::Hash;
use std::sync::{Arc, Mutex, MutexGuard};

pub type StoreResult<T> = Result<T, StoreError>;

/// Cursor used by clients to replay the ordered job-event ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum EventCursor {
    /// Replay from the first retained event.
    #[default]
    Beginning,
    /// Replay events with sequence numbers greater than this value.
    AfterSequence(u64),
    /// Replay events after the event with this id.
    AfterEventId(JobEventId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    NotFound {
        collection: &'static str,
        id: String,
    },
    DuplicateId {
        collection: &'static str,
        id: String,
    },
    Conflict {
        collection: &'static str,
        reason: String,
    },
    InvalidCursor {
        reason: String,
    },
    SequenceOverflow,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StoreError::NotFound { collection, id } => {
                write!(f, "{collection} record was not found: {id}")
            }
            StoreError::DuplicateId { collection, id } => {
                write!(f, "{collection} record already exists: {id}")
            }
            StoreError::Conflict { collection, reason } => {
                write!(f, "{collection} conflict: {reason}")
            }
            StoreError::InvalidCursor { reason } => write!(f, "invalid event cursor: {reason}"),
            StoreError::SequenceOverflow => write!(f, "job event sequence overflowed"),
        }
    }
}

impl Error for StoreError {}

/// Synchronous logical store for the Secretary ledger.
///
/// The first backend is intentionally small: callers create immutable records,
/// append ordered events, and replay events from stable cursors. Mutation-heavy
/// lifecycle behavior should be represented by appending records/events rather
/// than by broad in-place updates.
pub trait SecretaryStore: Clone + Send + Sync + 'static {
    fn create_repository(&self, record: RepositoryRecord) -> StoreResult<()>;
    fn list_repositories(&self) -> StoreResult<Vec<RepositoryRecord>>;
    fn get_repository(&self, id: &RepositoryId) -> StoreResult<RepositoryRecord>;

    fn create_job(&self, record: JobRecord) -> StoreResult<()>;
    fn list_jobs(&self) -> StoreResult<Vec<JobRecord>>;
    fn get_job(&self, id: &JobId) -> StoreResult<JobRecord>;

    /// Append a job event and assign the next store-wide sequence number.
    fn append_job_event(&self, event: JobEvent) -> StoreResult<JobEvent>;
    fn replay_job_events(
        &self,
        cursor: EventCursor,
        limit: Option<usize>,
    ) -> StoreResult<Vec<JobEvent>>;
    fn get_job_event(&self, id: &JobEventId) -> StoreResult<JobEvent>;

    fn create_policy_decision(&self, record: PolicyDecision) -> StoreResult<()>;
    fn list_policy_decisions(&self) -> StoreResult<Vec<PolicyDecision>>;
    fn get_policy_decision(&self, id: &PolicyDecisionId) -> StoreResult<PolicyDecision>;

    fn create_lock_decision(&self, record: LockDecision) -> StoreResult<()>;
    fn list_lock_decisions(&self) -> StoreResult<Vec<LockDecision>>;
    fn get_lock_decision(&self, id: &LockDecisionId) -> StoreResult<LockDecision>;

    fn create_tool_invocation(&self, record: ToolInvocation) -> StoreResult<()>;
    fn list_tool_invocations(&self) -> StoreResult<Vec<ToolInvocation>>;
    fn get_tool_invocation(&self, id: &ToolInvocationId) -> StoreResult<ToolInvocation>;

    fn create_tool_result(&self, record: ToolResult) -> StoreResult<()>;
    fn list_tool_results(&self) -> StoreResult<Vec<ToolResult>>;
    fn get_tool_result(&self, id: &ToolResultId) -> StoreResult<ToolResult>;

    fn create_audit_record(&self, record: AuditRecord) -> StoreResult<()>;
    fn list_audit_records(&self) -> StoreResult<Vec<AuditRecord>>;
    fn get_audit_record(&self, id: &AuditRecordId) -> StoreResult<AuditRecord>;
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryStore {
    inner: Arc<Mutex<InMemoryInner>>,
}

#[derive(Debug, Default)]
struct InMemoryInner {
    repositories: HashMap<RepositoryId, RepositoryRecord>,
    jobs: HashMap<JobId, JobRecord>,
    job_events_by_id: HashMap<JobEventId, JobEvent>,
    job_events_by_sequence: BTreeMap<u64, JobEventId>,
    next_event_sequence: u64,
    policy_decisions: HashMap<PolicyDecisionId, PolicyDecision>,
    lock_decisions: HashMap<LockDecisionId, LockDecision>,
    tool_invocations: HashMap<ToolInvocationId, ToolInvocation>,
    tool_results: HashMap<ToolResultId, ToolResult>,
    audit_records: HashMap<AuditRecordId, AuditRecord>,
}

impl InMemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> StoreResult<MutexGuard<'_, InMemoryInner>> {
        self.inner.lock().map_err(|_| StoreError::Conflict {
            collection: "store",
            reason: "in-memory store lock was poisoned".to_string(),
        })
    }
}

impl SecretaryStore for InMemoryStore {
    fn create_repository(&self, record: RepositoryRecord) -> StoreResult<()> {
        let mut inner = self.lock()?;
        insert_record(
            &mut inner.repositories,
            record.id.clone(),
            record,
            "repositories",
        )
    }

    fn list_repositories(&self) -> StoreResult<Vec<RepositoryRecord>> {
        Ok(list_records(&self.lock()?.repositories))
    }

    fn get_repository(&self, id: &RepositoryId) -> StoreResult<RepositoryRecord> {
        get_record(&self.lock()?.repositories, id, "repositories")
    }

    fn create_job(&self, record: JobRecord) -> StoreResult<()> {
        let mut inner = self.lock()?;
        if !inner.repositories.contains_key(&record.repository_id) {
            return Err(StoreError::NotFound {
                collection: "repositories",
                id: id_debug(&record.repository_id),
            });
        }
        insert_record(&mut inner.jobs, record.id.clone(), record, "jobs")
    }

    fn list_jobs(&self) -> StoreResult<Vec<JobRecord>> {
        Ok(list_records(&self.lock()?.jobs))
    }

    fn get_job(&self, id: &JobId) -> StoreResult<JobRecord> {
        get_record(&self.lock()?.jobs, id, "jobs")
    }

    fn append_job_event(&self, mut event: JobEvent) -> StoreResult<JobEvent> {
        let mut inner = self.lock()?;
        if inner.job_events_by_id.contains_key(&event.id) {
            return Err(StoreError::DuplicateId {
                collection: "job_events",
                id: id_debug(&event.id),
            });
        }

        let next_sequence = inner
            .next_event_sequence
            .checked_add(1)
            .ok_or(StoreError::SequenceOverflow)?;
        inner.next_event_sequence = next_sequence;
        event.sequence_number = next_sequence;

        inner
            .job_events_by_sequence
            .insert(event.sequence_number, event.id.clone());
        inner
            .job_events_by_id
            .insert(event.id.clone(), event.clone());
        Ok(event)
    }

    fn replay_job_events(
        &self,
        cursor: EventCursor,
        limit: Option<usize>,
    ) -> StoreResult<Vec<JobEvent>> {
        let inner = self.lock()?;
        let after_sequence =
            match cursor {
                EventCursor::Beginning => 0,
                EventCursor::AfterSequence(sequence) => sequence,
                EventCursor::AfterEventId(id) => {
                    let event = inner.job_events_by_id.get(&id).ok_or_else(|| {
                        StoreError::InvalidCursor {
                            reason: format!("event id is not retained: {}", id_debug(&id)),
                        }
                    })?;
                    event.sequence_number
                }
            };

        let mut events = Vec::new();
        if let Some(start_sequence) = after_sequence.checked_add(1) {
            for (_, id) in inner
                .job_events_by_sequence
                .range(start_sequence..)
                .take(limit.unwrap_or(usize::MAX))
            {
                let event = inner
                    .job_events_by_id
                    .get(id)
                    .ok_or_else(|| StoreError::Conflict {
                        collection: "job_events",
                        reason: format!("sequence index references missing id {}", id_debug(id)),
                    })?;
                events.push(event.clone());
            }
        }
        Ok(events)
    }

    fn get_job_event(&self, id: &JobEventId) -> StoreResult<JobEvent> {
        get_record(&self.lock()?.job_events_by_id, id, "job_events")
    }

    fn create_policy_decision(&self, record: PolicyDecision) -> StoreResult<()> {
        let mut inner = self.lock()?;
        insert_record(
            &mut inner.policy_decisions,
            record.id.clone(),
            record,
            "policy_decisions",
        )
    }

    fn list_policy_decisions(&self) -> StoreResult<Vec<PolicyDecision>> {
        Ok(list_records(&self.lock()?.policy_decisions))
    }

    fn get_policy_decision(&self, id: &PolicyDecisionId) -> StoreResult<PolicyDecision> {
        get_record(&self.lock()?.policy_decisions, id, "policy_decisions")
    }

    fn create_lock_decision(&self, record: LockDecision) -> StoreResult<()> {
        let mut inner = self.lock()?;
        if inner.lock_decisions.contains_key(&record.id) {
            return Err(StoreError::DuplicateId {
                collection: "lock_decisions",
                id: id_debug(&record.id),
            });
        }
        if !inner.repositories.contains_key(&record.repository_id) {
            return Err(StoreError::NotFound {
                collection: "repositories",
                id: id_debug(&record.repository_id),
            });
        }
        if !inner
            .policy_decisions
            .contains_key(&record.policy_decision_id)
        {
            return Err(StoreError::NotFound {
                collection: "policy_decisions",
                id: id_debug(&record.policy_decision_id),
            });
        }
        if is_active_lock_status(&record.status) {
            let conflicting_lock = inner.lock_decisions.values().find(|existing| {
                is_active_lock_status(&existing.status)
                    && existing.repository_id == record.repository_id
                    && existing.locked_scope == record.locked_scope
            });
            if let Some(existing) = conflicting_lock {
                return Err(StoreError::Conflict {
                    collection: "lock_decisions",
                    reason: format!(
                        "active lock {} already covers repository/scope",
                        id_debug(&existing.id)
                    ),
                });
            }
        }
        inner.lock_decisions.insert(record.id.clone(), record);
        Ok(())
    }

    fn list_lock_decisions(&self) -> StoreResult<Vec<LockDecision>> {
        Ok(list_records(&self.lock()?.lock_decisions))
    }

    fn get_lock_decision(&self, id: &LockDecisionId) -> StoreResult<LockDecision> {
        get_record(&self.lock()?.lock_decisions, id, "lock_decisions")
    }

    fn create_tool_invocation(&self, record: ToolInvocation) -> StoreResult<()> {
        let mut inner = self.lock()?;
        validate_tool_invocation_refs(&inner, &record)?;
        insert_record(
            &mut inner.tool_invocations,
            record.id.clone(),
            record,
            "tool_invocations",
        )
    }

    fn list_tool_invocations(&self) -> StoreResult<Vec<ToolInvocation>> {
        Ok(list_records(&self.lock()?.tool_invocations))
    }

    fn get_tool_invocation(&self, id: &ToolInvocationId) -> StoreResult<ToolInvocation> {
        get_record(&self.lock()?.tool_invocations, id, "tool_invocations")
    }

    fn create_tool_result(&self, record: ToolResult) -> StoreResult<()> {
        let mut inner = self.lock()?;
        validate_tool_result_refs(&inner, &record)?;
        insert_record(
            &mut inner.tool_results,
            record.id.clone(),
            record,
            "tool_results",
        )
    }

    fn list_tool_results(&self) -> StoreResult<Vec<ToolResult>> {
        Ok(list_records(&self.lock()?.tool_results))
    }

    fn get_tool_result(&self, id: &ToolResultId) -> StoreResult<ToolResult> {
        get_record(&self.lock()?.tool_results, id, "tool_results")
    }

    fn create_audit_record(&self, record: AuditRecord) -> StoreResult<()> {
        let mut inner = self.lock()?;
        validate_audit_record_refs(&inner, &record)?;
        insert_record(
            &mut inner.audit_records,
            record.id.clone(),
            record,
            "audit_records",
        )
    }

    fn list_audit_records(&self) -> StoreResult<Vec<AuditRecord>> {
        Ok(list_records(&self.lock()?.audit_records))
    }

    fn get_audit_record(&self, id: &AuditRecordId) -> StoreResult<AuditRecord> {
        get_record(&self.lock()?.audit_records, id, "audit_records")
    }
}

fn insert_record<Id, Record>(
    collection: &mut HashMap<Id, Record>,
    id: Id,
    record: Record,
    collection_name: &'static str,
) -> StoreResult<()>
where
    Id: Clone + Eq + Hash + fmt::Debug,
{
    if collection.contains_key(&id) {
        return Err(StoreError::DuplicateId {
            collection: collection_name,
            id: id_debug(&id),
        });
    }
    collection.insert(id, record);
    Ok(())
}

fn get_record<Id, Record>(
    collection: &HashMap<Id, Record>,
    id: &Id,
    collection_name: &'static str,
) -> StoreResult<Record>
where
    Id: Eq + Hash + fmt::Debug,
    Record: Clone,
{
    collection
        .get(id)
        .cloned()
        .ok_or_else(|| StoreError::NotFound {
            collection: collection_name,
            id: id_debug(id),
        })
}

fn list_records<Id, Record>(collection: &HashMap<Id, Record>) -> Vec<Record>
where
    Record: Clone,
{
    collection.values().cloned().collect()
}

fn id_debug<Id: fmt::Debug>(id: &Id) -> String {
    format!("{id:?}")
}

fn is_active_lock_status<Status: fmt::Debug>(status: &Status) -> bool {
    matches!(
        format!("{status:?}").as_str(),
        "Held" | "Active" | "held" | "active"
    )
}

fn validate_tool_invocation_refs(
    inner: &InMemoryInner,
    record: &ToolInvocation,
) -> StoreResult<()> {
    ensure_ref_exists(
        inner.jobs.contains_key(&record.job_id),
        "jobs",
        &record.job_id,
        "tool_invocation.job_id",
    )?;
    ensure_ref_exists(
        inner.repositories.contains_key(&record.repository_id),
        "repositories",
        &record.repository_id,
        "tool_invocation.repository_id",
    )?;
    ensure_ref_exists(
        inner
            .policy_decisions
            .contains_key(&record.policy_decision_id),
        "policy_decisions",
        &record.policy_decision_id,
        "tool_invocation.policy_decision_id",
    )
}

fn validate_tool_result_refs(inner: &InMemoryInner, record: &ToolResult) -> StoreResult<()> {
    ensure_ref_exists(
        inner.tool_invocations.contains_key(&record.invocation_id),
        "tool_invocations",
        &record.invocation_id,
        "tool_result.invocation_id",
    )
}

fn validate_audit_record_refs(inner: &InMemoryInner, record: &AuditRecord) -> StoreResult<()> {
    ensure_ref_exists(
        inner.repositories.contains_key(&record.repository_id),
        "repositories",
        &record.repository_id,
        "audit_record.repository_id",
    )?;
    ensure_ref_exists(
        inner
            .policy_decisions
            .contains_key(&record.policy_decision_id),
        "policy_decisions",
        &record.policy_decision_id,
        "audit_record.policy_decision_id",
    )?;

    if let Some(tool_invocation_id) = &record.tool_invocation_id {
        ensure_ref_exists(
            inner.tool_invocations.contains_key(tool_invocation_id),
            "tool_invocations",
            tool_invocation_id,
            "audit_record.tool_invocation_id",
        )?;
    }

    Ok(())
}

fn ensure_ref_exists<Id: fmt::Debug>(
    exists: bool,
    collection: &'static str,
    id: &Id,
    context: &str,
) -> StoreResult<()> {
    if exists {
        return Ok(());
    }

    Err(StoreError::NotFound {
        collection,
        id: format!("{} ({context})", id_debug(id)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Actor, EventRefs, EventSeverity, EventSubject, EventSubjectType, JobEventKind, JobKind,
        LedgerTimestamp, LockOwner, LockedScope, PolicyOutcome, RepositoryTrustState,
        ResourceScope, RiskTier, StructuredValue, ToolResultField, ToolResultStatus,
    };

    fn timestamp(value: i64) -> LedgerTimestamp {
        LedgerTimestamp::from_unix_millis(value)
    }

    fn actor() -> Actor {
        Actor::System {
            id: "store-test".to_string(),
        }
    }

    fn repository_record() -> RepositoryRecord {
        RepositoryRecord::new(
            "store test repository",
            "/tmp/atelia-store-test",
            RepositoryTrustState::Trusted,
            timestamp(1),
        )
    }

    fn job_record(repository_id: RepositoryId) -> JobRecord {
        JobRecord::new(
            actor(),
            repository_id,
            JobKind::Read,
            "store test job",
            timestamp(2),
        )
    }

    fn job_event() -> JobEvent {
        JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(3),
            subject: EventSubject {
                subject_type: EventSubjectType::Job,
                subject_id: "job_test".to_string(),
            },
            kind: JobEventKind::Message,
            severity: EventSeverity::Info,
            public_message: "store test event".to_string(),
            refs: EventRefs::default(),
            redactions: Vec::new(),
        }
    }

    fn policy_decision(repository_id: RepositoryId) -> PolicyDecision {
        PolicyDecision {
            id: PolicyDecisionId::new(),
            schema_version: 1,
            created_at: timestamp(4),
            requester: actor(),
            repository_id,
            requested_capability: "filesystem.read".to_string(),
            resource_scope: ResourceScope {
                kind: "repository".to_string(),
                value: "store-test".to_string(),
            },
            tool_id: None,
            provider_id: None,
            declared_effect: "read repository metadata".to_string(),
            current_trust_state: RepositoryTrustState::Trusted,
            approval_available: false,
            policy_version: "test-policy-v1".to_string(),
            outcome: PolicyOutcome::Allowed,
            risk_tier: RiskTier::R1,
            reason_code: "test_allowed".to_string(),
            user_reason: "test policy decision".to_string(),
            approval_request_ref: None,
            audit_ref: None,
            redactions: Vec::new(),
        }
    }

    fn lock_decision(
        repository_id: RepositoryId,
        policy_decision_id: PolicyDecisionId,
    ) -> LockDecision {
        LockDecision::new(
            repository_id,
            policy_decision_id,
            LockOwner::System {
                id: "store-test".to_string(),
            },
            LockedScope::Repository,
            timestamp(5),
            timestamp(6),
        )
    }

    fn tool_invocation(
        repository_id: RepositoryId,
        job_id: JobId,
        policy_decision_id: PolicyDecisionId,
    ) -> ToolInvocation {
        ToolInvocation {
            id: ToolInvocationId::new(),
            schema_version: 1,
            created_at: timestamp(7),
            job_id,
            repository_id,
            policy_decision_id,
            actor: actor(),
            tool_id: "fs.search".to_string(),
            requested_capability: "filesystem.search".to_string(),
            args_summary: "search docs".to_string(),
            resolved_paths: Vec::new(),
            timeout_millis: Some(1000),
            redactions: Vec::new(),
        }
    }

    fn tool_result(invocation_id: ToolInvocationId) -> ToolResult {
        ToolResult {
            id: ToolResultId::new(),
            schema_version: 1,
            created_at: timestamp(8),
            invocation_id,
            tool_id: "fs.search".to_string(),
            status: ToolResultStatus::Succeeded,
            schema_ref: Some("tool_result.v1".to_string()),
            fields: vec![ToolResultField {
                key: "summary".to_string(),
                value: StructuredValue::String("ok".to_string()),
            }],
            evidence_refs: Vec::new(),
            output_refs: Vec::new(),
            truncation: None,
            redactions: Vec::new(),
        }
    }

    fn audit_record(
        repository_id: RepositoryId,
        policy_decision_id: PolicyDecisionId,
        tool_invocation_id: Option<ToolInvocationId>,
    ) -> AuditRecord {
        AuditRecord {
            id: AuditRecordId::new(),
            schema_version: 1,
            created_at: timestamp(9),
            actor: actor(),
            repository_id,
            requested_capability: "filesystem.search".to_string(),
            policy_decision_id,
            tool_invocation_id,
            effect_summary: "searched docs".to_string(),
            output_refs: Vec::new(),
            redactions: Vec::new(),
        }
    }

    #[test]
    fn create_list_get_repository_records() {
        let store = InMemoryStore::new();
        let repository = repository_record();

        store.create_repository(repository.clone()).unwrap();

        assert_eq!(store.get_repository(&repository.id).unwrap(), repository);
        assert_eq!(store.list_repositories().unwrap().len(), 1);
    }

    #[test]
    fn create_list_get_jobs() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());

        store.create_repository(repository).unwrap();
        store.create_job(job.clone()).unwrap();

        assert_eq!(store.get_job(&job.id).unwrap(), job);
        assert_eq!(store.list_jobs().unwrap().len(), 1);
    }

    #[test]
    fn append_events_assigns_monotonic_sequences() {
        let store = InMemoryStore::new();

        let first = store.append_job_event(job_event()).unwrap();
        let second = store.append_job_event(job_event()).unwrap();

        assert_eq!(first.sequence_number, 1);
        assert_eq!(second.sequence_number, 2);
        assert_eq!(store.get_job_event(&first.id).unwrap(), first);
    }

    #[test]
    fn append_event_reports_sequence_overflow() {
        let store = InMemoryStore::new();
        store.lock().unwrap().next_event_sequence = u64::MAX;

        assert_eq!(
            Err(StoreError::SequenceOverflow),
            store.append_job_event(job_event())
        );
    }

    #[test]
    fn replay_from_cursor_returns_events_after_cursor() {
        let store = InMemoryStore::new();

        let first = store.append_job_event(job_event()).unwrap();
        let second = store.append_job_event(job_event()).unwrap();
        let third = store.append_job_event(job_event()).unwrap();

        assert_eq!(
            store
                .replay_job_events(EventCursor::AfterSequence(first.sequence_number), None)
                .unwrap(),
            vec![second.clone(), third.clone()]
        );
        assert_eq!(
            store
                .replay_job_events(EventCursor::AfterEventId(second.id), Some(1))
                .unwrap(),
            vec![third]
        );
    }

    #[test]
    fn replay_after_max_sequence_returns_empty_events() {
        let store = InMemoryStore::new();

        store.append_job_event(job_event()).unwrap();

        assert_eq!(
            store
                .replay_job_events(EventCursor::AfterSequence(u64::MAX), None)
                .unwrap(),
            Vec::<JobEvent>::new()
        );
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        let store = InMemoryStore::new();
        let repository = repository_record();

        store.create_repository(repository.clone()).unwrap();

        assert!(matches!(
            store.create_repository(repository),
            Err(StoreError::DuplicateId {
                collection: "repositories",
                ..
            })
        ));
    }

    #[test]
    fn duplicate_event_ids_are_rejected() {
        let store = InMemoryStore::new();
        let event = job_event();

        store.append_job_event(event.clone()).unwrap();

        assert!(matches!(
            store.append_job_event(event),
            Err(StoreError::DuplicateId {
                collection: "job_events",
                ..
            })
        ));
    }

    #[test]
    fn lock_decisions_are_persisted() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let policy = policy_decision(repository.id.clone());
        let lock = lock_decision(repository.id.clone(), policy.id.clone());

        store.create_repository(repository).unwrap();
        store.create_policy_decision(policy).unwrap();
        store.create_lock_decision(lock.clone()).unwrap();

        assert_eq!(store.get_lock_decision(&lock.id).unwrap(), lock);
        assert_eq!(store.list_lock_decisions().unwrap().len(), 1);
    }

    #[test]
    fn lock_decisions_require_existing_repository_and_policy_decision() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let policy = policy_decision(repository.id.clone());
        let lock = lock_decision(repository.id.clone(), policy.id.clone());

        assert!(matches!(
            store.create_lock_decision(lock.clone()),
            Err(StoreError::NotFound {
                collection: "repositories",
                ..
            })
        ));

        store.create_repository(repository).unwrap();

        assert!(matches!(
            store.create_lock_decision(lock.clone()),
            Err(StoreError::NotFound {
                collection: "policy_decisions",
                ..
            })
        ));

        store.create_policy_decision(policy).unwrap();
        store.create_lock_decision(lock).unwrap();
    }

    #[test]
    fn tool_records_require_existing_parents() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());
        let policy = policy_decision(repository.id.clone());
        let invocation = tool_invocation(repository.id.clone(), job.id.clone(), policy.id.clone());
        let result = tool_result(invocation.id.clone());
        let audit = audit_record(
            repository.id.clone(),
            policy.id.clone(),
            Some(invocation.id.clone()),
        );

        assert!(matches!(
            store.create_tool_invocation(invocation.clone()),
            Err(StoreError::NotFound {
                collection: "jobs",
                ..
            })
        ));

        store.create_repository(repository).unwrap();
        store.create_job(job).unwrap();

        assert!(matches!(
            store.create_tool_invocation(invocation.clone()),
            Err(StoreError::NotFound {
                collection: "policy_decisions",
                ..
            })
        ));

        store.create_policy_decision(policy).unwrap();
        store.create_tool_invocation(invocation.clone()).unwrap();
        store.create_tool_result(result.clone()).unwrap();
        store.create_audit_record(audit.clone()).unwrap();

        assert_eq!(store.get_tool_result(&result.id).unwrap(), result);
        assert_eq!(store.get_audit_record(&audit.id).unwrap(), audit);
    }

    #[test]
    fn tool_result_and_audit_reject_missing_invocation() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let policy = policy_decision(repository.id.clone());
        let invocation = tool_invocation(repository.id.clone(), JobId::new(), policy.id.clone());

        store.create_repository(repository.clone()).unwrap();
        store.create_policy_decision(policy.clone()).unwrap();

        assert!(matches!(
            store.create_tool_result(tool_result(invocation.id.clone())),
            Err(StoreError::NotFound {
                collection: "tool_invocations",
                ..
            })
        ));
        assert!(matches!(
            store.create_audit_record(audit_record(repository.id, policy.id, Some(invocation.id))),
            Err(StoreError::NotFound {
                collection: "tool_invocations",
                ..
            })
        ));
    }
}
