//! Storage abstractions for Secretary runtime records.

use crate::domain::{
    Actor, AuditRecord, AuditRecordId, CancellationState, EventSeverity, EventSubjectType,
    JobEvent, JobEventId, JobEventKind, JobId, JobRecord, JobStatus, LockDecision, LockDecisionId,
    LockOwner, PolicyDecision, PolicyDecisionId, RepositoryId, RepositoryRecord, ToolInvocation,
    ToolInvocationId, ToolResult, ToolResultId,
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobQuery {
    pub repository_id: Option<RepositoryId>,
    pub status: Option<JobStatus>,
    pub requester: Option<Actor>,
    pub page_size: Option<usize>,
    pub page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobPage {
    pub jobs: Vec<JobRecord>,
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventQuery {
    pub repository_id: Option<RepositoryId>,
    pub cursor: EventCursor,
    pub subject_ids: Vec<String>,
    pub min_severity: Option<EventSeverity>,
    pub page_size: Option<usize>,
    pub page_token: Option<String>,
}

impl Default for EventQuery {
    fn default() -> Self {
        Self {
            repository_id: None,
            cursor: EventCursor::Beginning,
            subject_ids: Vec::new(),
            min_severity: None,
            page_size: None,
            page_token: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPage {
    pub events: Vec<JobEvent>,
    pub next_page_token: Option<String>,
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
    InvalidReference {
        collection: &'static str,
        reason: String,
    },
    InvalidCursor {
        reason: String,
    },
    SequenceOverflow,
    InvalidRecord {
        collection: &'static str,
        reason: String,
    },
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
            StoreError::InvalidReference { collection, reason } => {
                write!(f, "{collection} invalid reference: {reason}")
            }
            StoreError::InvalidCursor { reason } => write!(f, "invalid event cursor: {reason}"),
            StoreError::SequenceOverflow => write!(f, "job event sequence overflowed"),
            StoreError::InvalidRecord { collection, reason } => {
                write!(f, "{collection} invalid record: {reason}")
            }
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
pub trait SecretaryStore: Send + Sync + 'static {
    fn create_repository(&self, record: RepositoryRecord) -> StoreResult<()>;
    fn list_repositories(&self) -> StoreResult<Vec<RepositoryRecord>>;
    fn get_repository(&self, id: &RepositoryId) -> StoreResult<RepositoryRecord>;

    fn create_job_with_initial_event(
        &self,
        record: JobRecord,
        initial_event: JobEvent,
    ) -> StoreResult<JobEvent>;
    fn list_jobs(&self) -> StoreResult<Vec<JobRecord>>;
    fn query_jobs(&self, query: JobQuery) -> StoreResult<JobPage>;
    fn get_job(&self, id: &JobId) -> StoreResult<JobRecord>;

    /// Append a job event and assign the next store-wide sequence number.
    fn append_job_event(&self, event: JobEvent) -> StoreResult<JobEvent>;
    fn append_job_event_and_update_job(
        &self,
        record: JobRecord,
        event: JobEvent,
    ) -> StoreResult<JobEvent>;
    fn append_job_events_and_update_job(
        &self,
        first_record: JobRecord,
        first_event: JobEvent,
        final_record: JobRecord,
        final_event: JobEvent,
    ) -> StoreResult<(JobEvent, JobEvent)>;
    fn replay_job_events(
        &self,
        cursor: EventCursor,
        limit: Option<usize>,
    ) -> StoreResult<Vec<JobEvent>>;
    fn query_job_events(&self, query: EventQuery) -> StoreResult<EventPage>;
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
    fn record_tool_result_and_audit_with_events(
        &self,
        record: JobRecord,
        tool_result: ToolResult,
        tool_result_event: JobEvent,
        audit_record: AuditRecord,
        audit_event: JobEvent,
    ) -> StoreResult<(JobEvent, JobEvent)>;
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
        validate_repository_record(&record)?;
        let mut inner = self.lock()?;
        if inner
            .repositories
            .values()
            .any(|repository| repository.root_path == record.root_path)
        {
            return Err(StoreError::DuplicateId {
                collection: "repositories",
                id: format!("root_path {}", record.root_path),
            });
        }
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

    fn create_job_with_initial_event(
        &self,
        mut record: JobRecord,
        mut initial_event: JobEvent,
    ) -> StoreResult<JobEvent> {
        let mut inner = self.lock()?;
        validate_job_refs(&inner, &record)?;
        if inner.jobs.contains_key(&record.id) {
            return Err(StoreError::DuplicateId {
                collection: "jobs",
                id: id_debug(&record.id),
            });
        }
        validate_initial_event_for_job(&record, &initial_event)?;
        validate_new_job_event(&inner, &initial_event, Some(&record))?;

        let next_sequence = inner
            .next_event_sequence
            .checked_add(1)
            .ok_or(StoreError::SequenceOverflow)?;
        inner.next_event_sequence = next_sequence;
        initial_event.sequence_number = next_sequence;
        record.latest_event_id = Some(initial_event.id.clone());

        inner.jobs.insert(record.id.clone(), record);
        inner
            .job_events_by_sequence
            .insert(initial_event.sequence_number, initial_event.id.clone());
        inner
            .job_events_by_id
            .insert(initial_event.id.clone(), initial_event.clone());
        Ok(initial_event)
    }

    fn list_jobs(&self) -> StoreResult<Vec<JobRecord>> {
        Ok(list_records(&self.lock()?.jobs))
    }

    fn query_jobs(&self, query: JobQuery) -> StoreResult<JobPage> {
        let inner = self.lock()?;
        let start = page_start(query.page_token.as_deref(), "jobs")?;
        let page_size = query.page_size.unwrap_or(usize::MAX);
        let mut filtered = inner
            .jobs
            .values()
            .filter(|job| {
                query
                    .repository_id
                    .as_ref()
                    .map(|repository_id| &job.repository_id == repository_id)
                    .unwrap_or(true)
            })
            .filter(|job| {
                query
                    .status
                    .map(|status| job.status == status)
                    .unwrap_or(true)
            })
            .filter(|job| {
                query
                    .requester
                    .as_ref()
                    .map(|requester| &job.requester == requester)
                    .unwrap_or(true)
            })
            .cloned()
            .collect::<Vec<_>>();
        filtered.sort_by(|left, right| left.id.cmp(&right.id));

        let (jobs, next_page_token) = page_records(filtered.into_iter(), start, page_size);

        Ok(JobPage {
            jobs,
            next_page_token,
        })
    }

    fn get_job(&self, id: &JobId) -> StoreResult<JobRecord> {
        get_record(&self.lock()?.jobs, id, "jobs")
    }

    fn append_job_event(&self, mut event: JobEvent) -> StoreResult<JobEvent> {
        let mut inner = self.lock()?;
        validate_new_job_event(&inner, &event, None)?;

        append_event_locked(&mut inner, &mut event)?;
        Ok(event)
    }

    fn append_job_event_and_update_job(
        &self,
        mut record: JobRecord,
        mut event: JobEvent,
    ) -> StoreResult<JobEvent> {
        let mut inner = self.lock()?;
        validate_job_update(&inner, &record, &event)?;
        validate_new_job_event(&inner, &event, None)?;

        append_event_locked(&mut inner, &mut event)?;
        record.latest_event_id = Some(event.id.clone());
        inner.jobs.insert(record.id.clone(), record);

        Ok(event)
    }

    fn append_job_events_and_update_job(
        &self,
        first_record: JobRecord,
        mut first_event: JobEvent,
        mut final_record: JobRecord,
        mut final_event: JobEvent,
    ) -> StoreResult<(JobEvent, JobEvent)> {
        let mut inner = self.lock()?;

        validate_sibling_event_ids_are_unique(&first_event, &final_event)?;
        validate_two_stage_job_update_identity(
            &first_record,
            &first_event,
            &final_record,
            &final_event,
        )?;
        validate_job_update(&inner, &first_record, &first_event)?;
        validate_new_job_event(&inner, &first_event, None)?;

        let mut intermediate_record = first_record;
        intermediate_record.latest_event_id = Some(first_event.id.clone());
        validate_job_update_against(&intermediate_record, &final_record, &final_event)?;
        validate_new_job_event(&inner, &final_event, None)?;
        ensure_event_sequence_capacity(&inner, 2)?;

        append_event_locked(&mut inner, &mut first_event)?;
        append_event_locked(&mut inner, &mut final_event)?;
        final_record.latest_event_id = Some(final_event.id.clone());
        inner.jobs.insert(final_record.id.clone(), final_record);

        Ok((first_event, final_event))
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

    fn query_job_events(&self, query: EventQuery) -> StoreResult<EventPage> {
        let inner = self.lock()?;
        let start = page_start(query.page_token.as_deref(), "job_events")?;
        let page_size = query.page_size.unwrap_or(usize::MAX);
        let after_sequence = event_cursor_sequence(&inner, query.cursor.clone())?;

        let mut filtered = Vec::new();
        if let Some(start_sequence) = after_sequence.checked_add(1) {
            for (_, id) in inner.job_events_by_sequence.range(start_sequence..) {
                let event = inner
                    .job_events_by_id
                    .get(id)
                    .ok_or_else(|| StoreError::Conflict {
                        collection: "job_events",
                        reason: format!("sequence index references missing id {}", id_debug(id)),
                    })?;

                if event_matches_query(&inner, event, &query)? {
                    filtered.push(event.clone());
                }
            }
        }

        let (events, next_page_token) = page_records(filtered.into_iter(), start, page_size);

        Ok(EventPage {
            events,
            next_page_token,
        })
    }

    fn get_job_event(&self, id: &JobEventId) -> StoreResult<JobEvent> {
        get_record(&self.lock()?.job_events_by_id, id, "job_events")
    }

    fn create_policy_decision(&self, record: PolicyDecision) -> StoreResult<()> {
        let mut inner = self.lock()?;
        ensure_ref_exists(
            inner.repositories.contains_key(&record.repository_id),
            "repositories",
            &record.repository_id,
            "policy_decision.repository_id",
        )?;
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
        validate_lock_decision_timing(&record)?;
        if !inner.repositories.contains_key(&record.repository_id) {
            return Err(StoreError::NotFound {
                collection: "repositories",
                id: id_debug(&record.repository_id),
            });
        }
        let policy_decision = inner
            .policy_decisions
            .get(&record.policy_decision_id)
            .ok_or_else(|| StoreError::NotFound {
                collection: "policy_decisions",
                id: id_debug(&record.policy_decision_id),
            })?;
        ensure_same_repository(
            "lock_decisions",
            "lock_decision.policy_decision_id",
            &record.repository_id,
            &policy_decision.repository_id,
        )?;
        if let LockOwner::Job(job_id) = &record.owner {
            let job = inner.jobs.get(job_id).ok_or_else(|| StoreError::NotFound {
                collection: "jobs",
                id: format!("{} (lock_decision.owner)", id_debug(job_id)),
            })?;
            ensure_same_repository(
                "lock_decisions",
                "lock_decision.owner",
                &record.repository_id,
                &job.repository_id,
            )?;
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

    fn record_tool_result_and_audit_with_events(
        &self,
        mut record: JobRecord,
        tool_result: ToolResult,
        mut tool_result_event: JobEvent,
        audit_record: AuditRecord,
        mut audit_event: JobEvent,
    ) -> StoreResult<(JobEvent, JobEvent)> {
        let mut inner = self.lock()?;

        validate_sibling_event_ids_are_unique(&tool_result_event, &audit_event)?;
        validate_job_update(&inner, &record, &tool_result_event)?;
        validate_job_update(&inner, &record, &audit_event)?;
        validate_tool_result_refs(&inner, &tool_result)?;
        validate_audit_record_refs_with_pending_tool_result(
            &inner,
            &audit_record,
            Some(&tool_result),
        )?;
        validate_new_job_event_with_pending_records(
            &inner,
            &tool_result_event,
            None,
            Some(&tool_result),
            None,
        )?;
        validate_new_job_event_with_pending_records(
            &inner,
            &audit_event,
            None,
            Some(&tool_result),
            Some(&audit_record),
        )?;
        ensure_ref_absent(
            !inner.tool_results.contains_key(&tool_result.id),
            "tool_results",
            &tool_result.id,
        )?;
        ensure_ref_absent(
            !inner.audit_records.contains_key(&audit_record.id),
            "audit_records",
            &audit_record.id,
        )?;
        ensure_event_sequence_capacity(&inner, 2)?;

        insert_record(
            &mut inner.tool_results,
            tool_result.id.clone(),
            tool_result,
            "tool_results",
        )?;
        append_event_locked(&mut inner, &mut tool_result_event)?;
        record.latest_event_id = Some(tool_result_event.id.clone());
        inner.jobs.insert(record.id.clone(), record.clone());

        insert_record(
            &mut inner.audit_records,
            audit_record.id.clone(),
            audit_record,
            "audit_records",
        )?;
        append_event_locked(&mut inner, &mut audit_event)?;
        record.latest_event_id = Some(audit_event.id.clone());
        inner.jobs.insert(record.id.clone(), record);

        Ok((tool_result_event, audit_event))
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

fn validate_repository_record(record: &RepositoryRecord) -> StoreResult<()> {
    if record.display_name.trim().is_empty() {
        return Err(StoreError::InvalidRecord {
            collection: "repositories",
            reason: "display_name must not be empty".to_string(),
        });
    }

    if record.root_path.trim().is_empty() {
        return Err(StoreError::InvalidRecord {
            collection: "repositories",
            reason: "root_path must not be empty".to_string(),
        });
    }

    if record.allowed_path_scope.root_path != record.root_path {
        return Err(StoreError::InvalidRecord {
            collection: "repositories",
            reason: "allowed_path_scope.root_path must match repository root_path".to_string(),
        });
    }

    if record.allowed_path_scope.allowed_paths.is_empty() {
        return Err(StoreError::InvalidRecord {
            collection: "repositories",
            reason: "allowed_path_scope must include at least one path".to_string(),
        });
    }

    Ok(())
}

fn page_start(page_token: Option<&str>, collection: &'static str) -> StoreResult<usize> {
    match page_token {
        Some(token) if !token.is_empty() => {
            token
                .parse::<usize>()
                .map_err(|_| StoreError::InvalidCursor {
                    reason: format!("{collection} page token is not a numeric offset"),
                })
        }
        _ => Ok(0),
    }
}

fn page_records<Record>(
    records: impl Iterator<Item = Record>,
    start: usize,
    page_size: usize,
) -> (Vec<Record>, Option<String>) {
    let mut skipped = 0usize;
    let mut retained = Vec::new();
    let mut has_next = false;

    if page_size == 0 {
        for _ in records.take(start) {}

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

fn event_cursor_sequence(inner: &InMemoryInner, cursor: EventCursor) -> StoreResult<u64> {
    match cursor {
        EventCursor::Beginning => Ok(0),
        EventCursor::AfterSequence(sequence) => Ok(sequence),
        EventCursor::AfterEventId(id) => {
            let event =
                inner
                    .job_events_by_id
                    .get(&id)
                    .ok_or_else(|| StoreError::InvalidCursor {
                        reason: format!("event id is not retained: {}", id_debug(&id)),
                    })?;
            Ok(event.sequence_number)
        }
    }
}

fn event_matches_query(
    inner: &InMemoryInner,
    event: &JobEvent,
    query: &EventQuery,
) -> StoreResult<bool> {
    let repository_matches = query
        .repository_id
        .as_ref()
        .map(|repository_id| {
            event_repository_id(inner, event)
                .map(|event_repository_id| event_repository_id.as_ref() == Some(repository_id))
        })
        .transpose()?
        .unwrap_or(true);
    let subject_matches =
        query.subject_ids.is_empty() || query.subject_ids.contains(&event.subject.subject_id);
    let severity_matches = query
        .min_severity
        .map(|min_severity| severity_rank(event.severity) >= severity_rank(min_severity))
        .unwrap_or(true);

    Ok(repository_matches && subject_matches && severity_matches)
}

fn event_repository_id(
    inner: &InMemoryInner,
    event: &JobEvent,
) -> StoreResult<Option<RepositoryId>> {
    if let Some(repository_id) = &event.refs.repository_id {
        return Ok(Some(repository_id.clone()));
    }

    let subject_repository_id = subject_repository_id(inner, event, None, None, None)?.cloned();
    let mut event_repository_id = subject_repository_id;
    derive_event_repository_from_refs(inner, event, &mut event_repository_id)?;

    Ok(event_repository_id)
}

fn derive_event_repository_from_refs(
    inner: &InMemoryInner,
    event: &JobEvent,
    event_repository_id: &mut Option<RepositoryId>,
) -> StoreResult<()> {
    if let Some(job_id) = &event.refs.job_id {
        let job = inner.jobs.get(job_id).ok_or_else(|| StoreError::NotFound {
            collection: "jobs",
            id: format!("{} (job_event.refs.job_id)", id_debug(job_id)),
        })?;
        ensure_owned_event_repository_consistency(
            event_repository_id,
            &job.repository_id,
            "job_event.refs.job_id",
        )?;
    }

    if let Some(policy_decision_id) = &event.refs.policy_decision_id {
        let policy_decision = inner
            .policy_decisions
            .get(policy_decision_id)
            .ok_or_else(|| StoreError::NotFound {
                collection: "policy_decisions",
                id: format!(
                    "{} (job_event.refs.policy_decision_id)",
                    id_debug(policy_decision_id)
                ),
            })?;
        ensure_owned_event_repository_consistency(
            event_repository_id,
            &policy_decision.repository_id,
            "job_event.refs.policy_decision_id",
        )?;
    }

    if let Some(lock_decision_id) = &event.refs.lock_decision_id {
        let lock_decision =
            inner
                .lock_decisions
                .get(lock_decision_id)
                .ok_or_else(|| StoreError::NotFound {
                    collection: "lock_decisions",
                    id: format!(
                        "{} (job_event.refs.lock_decision_id)",
                        id_debug(lock_decision_id)
                    ),
                })?;
        ensure_owned_event_repository_consistency(
            event_repository_id,
            &lock_decision.repository_id,
            "job_event.refs.lock_decision_id",
        )?;
    }

    if let Some(tool_invocation_id) = &event.refs.tool_invocation_id {
        let tool_invocation = inner
            .tool_invocations
            .get(tool_invocation_id)
            .ok_or_else(|| StoreError::NotFound {
                collection: "tool_invocations",
                id: format!(
                    "{} (job_event.refs.tool_invocation_id)",
                    id_debug(tool_invocation_id)
                ),
            })?;
        ensure_owned_event_repository_consistency(
            event_repository_id,
            &tool_invocation.repository_id,
            "job_event.refs.tool_invocation_id",
        )?;
    }

    if let Some(tool_result_id) = &event.refs.tool_result_id {
        let tool_result =
            inner
                .tool_results
                .get(tool_result_id)
                .ok_or_else(|| StoreError::NotFound {
                    collection: "tool_results",
                    id: format!(
                        "{} (job_event.refs.tool_result_id)",
                        id_debug(tool_result_id)
                    ),
                })?;
        let tool_invocation = inner
            .tool_invocations
            .get(&tool_result.invocation_id)
            .ok_or_else(|| StoreError::Conflict {
                collection: "tool_results",
                reason: format!(
                    "tool_result {} references missing invocation {}",
                    id_debug(tool_result_id),
                    id_debug(&tool_result.invocation_id)
                ),
            })?;
        ensure_owned_event_repository_consistency(
            event_repository_id,
            &tool_invocation.repository_id,
            "job_event.refs.tool_result_id",
        )?;
    }

    if let Some(audit_record_id) = &event.refs.audit_record_id {
        let audit_record =
            inner
                .audit_records
                .get(audit_record_id)
                .ok_or_else(|| StoreError::NotFound {
                    collection: "audit_records",
                    id: format!(
                        "{} (job_event.refs.audit_record_id)",
                        id_debug(audit_record_id)
                    ),
                })?;
        ensure_owned_event_repository_consistency(
            event_repository_id,
            &audit_record.repository_id,
            "job_event.refs.audit_record_id",
        )?;
    }

    Ok(())
}

fn ensure_owned_event_repository_consistency(
    expected: &mut Option<RepositoryId>,
    actual: &RepositoryId,
    context: &str,
) -> StoreResult<()> {
    if let Some(expected) = expected {
        ensure_same_repository("job_events", context, expected, actual)?;
    } else {
        *expected = Some(actual.clone());
    }

    Ok(())
}

fn append_event_locked(inner: &mut InMemoryInner, event: &mut JobEvent) -> StoreResult<()> {
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

    Ok(())
}

fn ensure_event_sequence_capacity(inner: &InMemoryInner, event_count: u64) -> StoreResult<()> {
    inner
        .next_event_sequence
        .checked_add(event_count)
        .map(|_| ())
        .ok_or(StoreError::SequenceOverflow)
}

const fn severity_rank(severity: EventSeverity) -> u8 {
    match severity {
        EventSeverity::Debug => 0,
        EventSeverity::Info => 1,
        EventSeverity::Warning => 2,
        EventSeverity::Error => 3,
    }
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

fn validate_job_refs(inner: &InMemoryInner, record: &JobRecord) -> StoreResult<()> {
    ensure_ref_exists(
        inner.repositories.contains_key(&record.repository_id),
        "repositories",
        &record.repository_id,
        "job.repository_id",
    )
}

fn validate_new_job_event(
    inner: &InMemoryInner,
    event: &JobEvent,
    pending_job: Option<&JobRecord>,
) -> StoreResult<()> {
    validate_new_job_event_with_pending_records(inner, event, pending_job, None, None)
}

fn validate_new_job_event_with_pending_records(
    inner: &InMemoryInner,
    event: &JobEvent,
    pending_job: Option<&JobRecord>,
    pending_tool_result: Option<&ToolResult>,
    pending_audit_record: Option<&AuditRecord>,
) -> StoreResult<()> {
    if inner.job_events_by_id.contains_key(&event.id) {
        return Err(StoreError::DuplicateId {
            collection: "job_events",
            id: id_debug(&event.id),
        });
    }
    if !event.subject.has_valid_subject_id() {
        return Err(StoreError::InvalidReference {
            collection: "job_events",
            reason: format!(
                "event.subject_id {} does not match subject_type {:?}",
                event.subject.subject_id, event.subject.subject_type
            ),
        });
    }

    let subject_repository_id = validate_event_subject(
        inner,
        event,
        pending_job,
        pending_tool_result,
        pending_audit_record,
    )?;
    validate_event_refs(
        inner,
        event,
        pending_job,
        pending_tool_result,
        pending_audit_record,
        subject_repository_id,
    )
}

fn validate_initial_event_for_job(record: &JobRecord, event: &JobEvent) -> StoreResult<()> {
    if event.subject.subject_type != EventSubjectType::Job
        || event.subject.subject_id != record.id.as_str()
    {
        return Err(StoreError::InvalidReference {
            collection: "job_events",
            reason: format!(
                "initial event subject must identify job {}",
                id_debug(&record.id)
            ),
        });
    }
    if event.refs.job_id.as_ref() != Some(&record.id) {
        return Err(StoreError::InvalidReference {
            collection: "job_events",
            reason: format!(
                "initial event refs.job_id must identify job {}",
                id_debug(&record.id)
            ),
        });
    }
    if event.refs.repository_id.as_ref() != Some(&record.repository_id) {
        return Err(StoreError::InvalidReference {
            collection: "job_events",
            reason: format!(
                "initial event refs.repository_id must identify repository {}",
                id_debug(&record.repository_id)
            ),
        });
    }

    Ok(())
}

fn validate_job_update(
    inner: &InMemoryInner,
    record: &JobRecord,
    event: &JobEvent,
) -> StoreResult<()> {
    let existing = inner
        .jobs
        .get(&record.id)
        .ok_or_else(|| StoreError::NotFound {
            collection: "jobs",
            id: id_debug(&record.id),
        })?;
    if record.latest_event_id != existing.latest_event_id {
        return Err(StoreError::Conflict {
            collection: "jobs",
            reason: format!(
                "job update for {} is stale: expected latest_event_id {:?}, got {:?}",
                id_debug(&record.id),
                existing.latest_event_id,
                record.latest_event_id
            ),
        });
    }
    validate_job_update_against(existing, record, event)
}

fn validate_job_update_against(
    existing: &JobRecord,
    record: &JobRecord,
    event: &JobEvent,
) -> StoreResult<()> {
    ensure_same_repository(
        "jobs",
        "job.repository_id",
        &existing.repository_id,
        &record.repository_id,
    )?;
    ensure_immutable_job_field(
        &record.id,
        "schema_version",
        existing.schema_version,
        record.schema_version,
    )?;
    ensure_immutable_job_field(
        &record.id,
        "created_at",
        existing.created_at,
        record.created_at,
    )?;
    ensure_immutable_job_field(
        &record.id,
        "requester",
        &existing.requester,
        &record.requester,
    )?;
    ensure_immutable_job_field(&record.id, "kind", &existing.kind, &record.kind)?;
    ensure_immutable_job_field(&record.id, "goal", &existing.goal, &record.goal)?;
    validate_job_lifecycle_update(existing, record, event)?;
    validate_job_cancellation_update(existing, record, event)?;
    validate_job_policy_summary_update(existing, record, event)?;
    if event.refs.job_id.as_ref() != Some(&record.id) {
        return Err(StoreError::InvalidReference {
            collection: "job_events",
            reason: format!(
                "job update event refs.job_id must identify job {}",
                id_debug(&record.id)
            ),
        });
    }
    if event.refs.repository_id.as_ref() != Some(&record.repository_id) {
        return Err(StoreError::InvalidReference {
            collection: "job_events",
            reason: format!(
                "job update event refs.repository_id must identify repository {}",
                id_debug(&record.repository_id)
            ),
        });
    }

    Ok(())
}

fn validate_job_cancellation_update(
    existing: &JobRecord,
    record: &JobRecord,
    event: &JobEvent,
) -> StoreResult<()> {
    if record.cancellation_state == existing.cancellation_state {
        if matches!(event.kind, JobEventKind::CancellationRequested) {
            return job_update_conflict(
                &record.id,
                "CancellationRequested event did not change materialized cancellation state",
            );
        }
        return Ok(());
    }

    match (
        existing.cancellation_state,
        record.cancellation_state,
        &event.kind,
    ) {
        (
            CancellationState::NotRequested,
            CancellationState::Requested,
            JobEventKind::CancellationRequested,
        ) => Ok(()),
        _ => job_update_conflict(
            &record.id,
            format!(
                "cancellation_state changed from {:?} to {:?} without matching CancellationRequested event",
                existing.cancellation_state, record.cancellation_state
            ),
        ),
    }
}

fn validate_two_stage_job_update_identity(
    first_record: &JobRecord,
    first_event: &JobEvent,
    final_record: &JobRecord,
    final_event: &JobEvent,
) -> StoreResult<()> {
    if first_record.id != final_record.id {
        return Err(StoreError::InvalidReference {
            collection: "jobs",
            reason: format!(
                "two-stage job update first_record {} does not match final_record {}",
                id_debug(&first_record.id),
                id_debug(&final_record.id)
            ),
        });
    }

    for (context, event) in [
        ("first_event.refs.job_id", first_event),
        ("final_event.refs.job_id", final_event),
    ] {
        if event.refs.job_id.as_ref() != Some(&first_record.id) {
            return Err(StoreError::InvalidReference {
                collection: "job_events",
                reason: format!("{context} must identify job {}", id_debug(&first_record.id)),
            });
        }
    }

    Ok(())
}

fn validate_job_lifecycle_update(
    existing: &JobRecord,
    record: &JobRecord,
    event: &JobEvent,
) -> StoreResult<()> {
    if record.updated_at < existing.updated_at {
        return job_update_conflict(
            &record.id,
            format!(
                "updated_at moved backwards from {:?} to {:?}",
                existing.updated_at, record.updated_at
            ),
        );
    }

    if record.status != existing.status {
        match &event.kind {
            JobEventKind::JobStatusChanged { from, to }
                if *from == existing.status && *to == record.status => {}
            _ => {
                return job_update_conflict(
                    &record.id,
                    format!(
                        "status changed from {:?} to {:?} without matching JobStatusChanged event",
                        existing.status, record.status
                    ),
                );
            }
        }

        let mut expected = existing.clone();
        expected
            .transition_status(record.status, record.updated_at)
            .map_err(|error| StoreError::Conflict {
                collection: "jobs",
                reason: format!(
                    "job update for {} has invalid lifecycle transition: {error:?}",
                    id_debug(&record.id)
                ),
            })?;
        if record.started_at != expected.started_at || record.completed_at != expected.completed_at
        {
            return job_update_conflict(
                &record.id,
                format!(
                    "lifecycle timestamps do not match transition from {:?} to {:?}",
                    existing.status, record.status
                ),
            );
        }
    } else {
        if matches!(event.kind, JobEventKind::JobStatusChanged { .. }) {
            return job_update_conflict(
                &record.id,
                "JobStatusChanged event did not change materialized job status",
            );
        }
        if record.started_at != existing.started_at || record.completed_at != existing.completed_at
        {
            return job_update_conflict(
                &record.id,
                "lifecycle timestamps changed without a status transition",
            );
        }
    }

    if record.status == JobStatus::Running && record.started_at.is_none() {
        return job_update_conflict(&record.id, "running job must have started_at");
    }
    if record.status.is_terminal() && record.completed_at.is_none() {
        return job_update_conflict(&record.id, "terminal job must have completed_at");
    }
    if !record.status.is_terminal() && record.completed_at.is_some() {
        return job_update_conflict(&record.id, "non-terminal job cannot have completed_at");
    }

    Ok(())
}

fn validate_job_policy_summary_update(
    existing: &JobRecord,
    record: &JobRecord,
    event: &JobEvent,
) -> StoreResult<()> {
    if record.policy_summary == existing.policy_summary {
        return Ok(());
    }

    let JobEventKind::PolicyDecided { .. } = &event.kind else {
        return job_update_conflict(
            &record.id,
            "policy_summary changed without a PolicyDecided event",
        );
    };
    let summary_policy_id = record
        .policy_summary
        .as_ref()
        .and_then(|summary| summary.decision_id.as_ref());
    if summary_policy_id != event.refs.policy_decision_id.as_ref() {
        return job_update_conflict(
            &record.id,
            "policy_summary decision_id must match event refs.policy_decision_id",
        );
    }

    Ok(())
}

fn job_update_conflict(job_id: &JobId, reason: impl Into<String>) -> StoreResult<()> {
    Err(StoreError::Conflict {
        collection: "jobs",
        reason: format!("job update for {} {}", id_debug(job_id), reason.into()),
    })
}

fn validate_sibling_event_ids_are_unique(left: &JobEvent, right: &JobEvent) -> StoreResult<()> {
    if left.id != right.id {
        return Ok(());
    }

    Err(StoreError::DuplicateId {
        collection: "job_events",
        id: id_debug(&left.id),
    })
}

fn ensure_immutable_job_field<T>(
    job_id: &JobId,
    field_name: &'static str,
    existing: T,
    incoming: T,
) -> StoreResult<()>
where
    T: PartialEq + fmt::Debug,
{
    if existing == incoming {
        return Ok(());
    }

    Err(StoreError::Conflict {
        collection: "jobs",
        reason: format!(
            "job update for {} attempted to change immutable field {field_name}: existing {:?}, incoming {:?}",
            id_debug(job_id),
            existing,
            incoming
        ),
    })
}

fn validate_event_subject<'a>(
    inner: &'a InMemoryInner,
    event: &JobEvent,
    pending_job: Option<&'a JobRecord>,
    pending_tool_result: Option<&'a ToolResult>,
    pending_audit_record: Option<&'a AuditRecord>,
) -> StoreResult<Option<&'a RepositoryId>> {
    let subject_repository_id = subject_repository_id(
        inner,
        event,
        pending_job,
        pending_tool_result,
        pending_audit_record,
    )?;
    if let (Some(subject_repository_id), Some(ref_repository_id)) =
        (subject_repository_id, event.refs.repository_id.as_ref())
    {
        ensure_same_repository(
            "job_events",
            "job_event.subject_id",
            ref_repository_id,
            subject_repository_id,
        )?;
    }

    Ok(subject_repository_id)
}

fn subject_repository_id<'a>(
    inner: &'a InMemoryInner,
    event: &JobEvent,
    pending_job: Option<&'a JobRecord>,
    pending_tool_result: Option<&'a ToolResult>,
    pending_audit_record: Option<&'a AuditRecord>,
) -> StoreResult<Option<&'a RepositoryId>> {
    match event.subject.subject_type {
        EventSubjectType::Repository => {
            let repository = inner
                .repositories
                .keys()
                .find(|id| id.as_str() == event.subject.subject_id)
                .ok_or_else(|| subject_not_found("repositories", event))?;
            Ok(Some(repository))
        }
        EventSubjectType::Job => {
            let job = inner
                .jobs
                .values()
                .find(|job| job.id.as_str() == event.subject.subject_id)
                .or_else(|| pending_job.filter(|job| job.id.as_str() == event.subject.subject_id))
                .ok_or_else(|| subject_not_found("jobs", event))?;
            Ok(Some(&job.repository_id))
        }
        EventSubjectType::PolicyDecision => {
            let policy_decision = inner
                .policy_decisions
                .values()
                .find(|policy_decision| policy_decision.id.as_str() == event.subject.subject_id)
                .ok_or_else(|| subject_not_found("policy_decisions", event))?;
            Ok(Some(&policy_decision.repository_id))
        }
        EventSubjectType::LockDecision => {
            let lock_decision = inner
                .lock_decisions
                .values()
                .find(|lock_decision| lock_decision.id.as_str() == event.subject.subject_id)
                .ok_or_else(|| subject_not_found("lock_decisions", event))?;
            Ok(Some(&lock_decision.repository_id))
        }
        EventSubjectType::ToolInvocation => {
            let tool_invocation = inner
                .tool_invocations
                .values()
                .find(|tool_invocation| tool_invocation.id.as_str() == event.subject.subject_id)
                .ok_or_else(|| subject_not_found("tool_invocations", event))?;
            Ok(Some(&tool_invocation.repository_id))
        }
        EventSubjectType::ToolResult => {
            let tool_result = inner
                .tool_results
                .values()
                .find(|tool_result| tool_result.id.as_str() == event.subject.subject_id)
                .or_else(|| {
                    pending_tool_result
                        .filter(|tool_result| tool_result.id.as_str() == event.subject.subject_id)
                })
                .ok_or_else(|| subject_not_found("tool_results", event))?;
            let tool_invocation = inner
                .tool_invocations
                .get(&tool_result.invocation_id)
                .ok_or_else(|| StoreError::Conflict {
                    collection: "tool_results",
                    reason: format!(
                        "tool_result {} references missing invocation {}",
                        event.subject.subject_id,
                        id_debug(&tool_result.invocation_id)
                    ),
                })?;
            Ok(Some(&tool_invocation.repository_id))
        }
        EventSubjectType::AuditRecord => {
            let audit_record = inner
                .audit_records
                .values()
                .find(|audit_record| audit_record.id.as_str() == event.subject.subject_id)
                .or_else(|| {
                    pending_audit_record
                        .filter(|audit_record| audit_record.id.as_str() == event.subject.subject_id)
                })
                .ok_or_else(|| subject_not_found("audit_records", event))?;
            Ok(Some(&audit_record.repository_id))
        }
    }
}

fn subject_not_found(collection: &'static str, event: &JobEvent) -> StoreError {
    StoreError::NotFound {
        collection,
        id: format!("{} (job_event.subject_id)", event.subject.subject_id),
    }
}

fn validate_event_refs(
    inner: &InMemoryInner,
    event: &JobEvent,
    pending_job: Option<&JobRecord>,
    pending_tool_result: Option<&ToolResult>,
    pending_audit_record: Option<&AuditRecord>,
    subject_repository_id: Option<&RepositoryId>,
) -> StoreResult<()> {
    let mut event_repository_id = event.refs.repository_id.as_ref().or(subject_repository_id);
    let mut event_job_id = match event.subject.subject_type {
        EventSubjectType::Job => Some(event.subject.subject_id.as_str()),
        _ => event.refs.job_id.as_ref().map(JobId::as_str),
    };
    let mut event_policy_decision_id = match event.subject.subject_type {
        EventSubjectType::PolicyDecision => Some(event.subject.subject_id.as_str()),
        _ => event
            .refs
            .policy_decision_id
            .as_ref()
            .map(PolicyDecisionId::as_str),
    };

    if let Some(repository_id) = &event.refs.repository_id {
        ensure_subject_ref_matches(
            event,
            EventSubjectType::Repository,
            repository_id.as_str(),
            "job_event.refs.repository_id",
        )?;
        ensure_ref_exists(
            inner.repositories.contains_key(repository_id),
            "repositories",
            repository_id,
            "job_event.refs.repository_id",
        )?;
    }

    if let Some(job_id) = &event.refs.job_id {
        ensure_subject_ref_matches(
            event,
            EventSubjectType::Job,
            job_id.as_str(),
            "job_event.refs.job_id",
        )?;
        let job = inner
            .jobs
            .get(job_id)
            .or_else(|| pending_job.filter(|job| &job.id == job_id))
            .ok_or_else(|| StoreError::NotFound {
                collection: "jobs",
                id: format!("{} (job_event.refs.job_id)", id_debug(job_id)),
            })?;
        ensure_event_repository_consistency(
            &mut event_repository_id,
            &job.repository_id,
            "job_event.refs.job_id",
        )?;
        ensure_event_job_consistency(&mut event_job_id, &job.id, "job_event.refs.job_id")?;
    }

    if let Some(policy_decision_id) = &event.refs.policy_decision_id {
        ensure_subject_ref_matches(
            event,
            EventSubjectType::PolicyDecision,
            policy_decision_id.as_str(),
            "job_event.refs.policy_decision_id",
        )?;
        let policy_decision = inner
            .policy_decisions
            .get(policy_decision_id)
            .ok_or_else(|| StoreError::NotFound {
                collection: "policy_decisions",
                id: format!(
                    "{} (job_event.refs.policy_decision_id)",
                    id_debug(policy_decision_id)
                ),
            })?;
        ensure_event_repository_consistency(
            &mut event_repository_id,
            &policy_decision.repository_id,
            "job_event.refs.policy_decision_id",
        )?;
    }

    if let Some(lock_decision_id) = &event.refs.lock_decision_id {
        ensure_subject_ref_matches(
            event,
            EventSubjectType::LockDecision,
            lock_decision_id.as_str(),
            "job_event.refs.lock_decision_id",
        )?;
        let lock_decision =
            inner
                .lock_decisions
                .get(lock_decision_id)
                .ok_or_else(|| StoreError::NotFound {
                    collection: "lock_decisions",
                    id: format!(
                        "{} (job_event.refs.lock_decision_id)",
                        id_debug(lock_decision_id)
                    ),
                })?;
        ensure_event_repository_consistency(
            &mut event_repository_id,
            &lock_decision.repository_id,
            "job_event.refs.lock_decision_id",
        )?;
        ensure_event_policy_consistency(
            &mut event_policy_decision_id,
            &lock_decision.policy_decision_id,
            "job_event.refs.lock_decision_id",
        )?;
    }

    if let Some(tool_invocation_id) = &event.refs.tool_invocation_id {
        ensure_subject_ref_matches(
            event,
            EventSubjectType::ToolInvocation,
            tool_invocation_id.as_str(),
            "job_event.refs.tool_invocation_id",
        )?;
        let tool_invocation = inner
            .tool_invocations
            .get(tool_invocation_id)
            .ok_or_else(|| StoreError::NotFound {
                collection: "tool_invocations",
                id: format!(
                    "{} (job_event.refs.tool_invocation_id)",
                    id_debug(tool_invocation_id)
                ),
            })?;
        ensure_event_repository_consistency(
            &mut event_repository_id,
            &tool_invocation.repository_id,
            "job_event.refs.tool_invocation_id",
        )?;
        ensure_event_job_consistency(
            &mut event_job_id,
            &tool_invocation.job_id,
            "job_event.refs.tool_invocation_id",
        )?;
        ensure_event_policy_consistency(
            &mut event_policy_decision_id,
            &tool_invocation.policy_decision_id,
            "job_event.refs.tool_invocation_id",
        )?;
    }

    if let Some(tool_result_id) = &event.refs.tool_result_id {
        ensure_subject_ref_matches(
            event,
            EventSubjectType::ToolResult,
            tool_result_id.as_str(),
            "job_event.refs.tool_result_id",
        )?;
        let tool_result = inner
            .tool_results
            .get(tool_result_id)
            .or_else(|| pending_tool_result.filter(|result| &result.id == tool_result_id))
            .ok_or_else(|| StoreError::NotFound {
                collection: "tool_results",
                id: format!(
                    "{} (job_event.refs.tool_result_id)",
                    id_debug(tool_result_id)
                ),
            })?;
        let tool_invocation = inner
            .tool_invocations
            .get(&tool_result.invocation_id)
            .ok_or_else(|| StoreError::Conflict {
                collection: "tool_results",
                reason: format!(
                    "tool_result {} references missing invocation {}",
                    id_debug(tool_result_id),
                    id_debug(&tool_result.invocation_id)
                ),
            })?;
        ensure_event_repository_consistency(
            &mut event_repository_id,
            &tool_invocation.repository_id,
            "job_event.refs.tool_result_id",
        )?;
        if event
            .refs
            .tool_invocation_id
            .as_ref()
            .is_some_and(|id| id != &tool_result.invocation_id)
        {
            return Err(StoreError::InvalidReference {
                collection: "job_events",
                reason: format!(
                    "job_event.refs.tool_result_id {} belongs to invocation_id {}, not {:?}",
                    id_debug(tool_result_id),
                    id_debug(&tool_result.invocation_id),
                    event.refs.tool_invocation_id
                ),
            });
        }
        ensure_event_job_consistency(
            &mut event_job_id,
            &tool_invocation.job_id,
            "job_event.refs.tool_result_id",
        )?;
        ensure_event_policy_consistency(
            &mut event_policy_decision_id,
            &tool_invocation.policy_decision_id,
            "job_event.refs.tool_result_id",
        )?;
    }

    if let Some(audit_record_id) = &event.refs.audit_record_id {
        ensure_subject_ref_matches(
            event,
            EventSubjectType::AuditRecord,
            audit_record_id.as_str(),
            "job_event.refs.audit_record_id",
        )?;
        let audit_record = inner
            .audit_records
            .get(audit_record_id)
            .or_else(|| pending_audit_record.filter(|record| &record.id == audit_record_id))
            .ok_or_else(|| StoreError::NotFound {
                collection: "audit_records",
                id: format!(
                    "{} (job_event.refs.audit_record_id)",
                    id_debug(audit_record_id)
                ),
            })?;
        ensure_event_repository_consistency(
            &mut event_repository_id,
            &audit_record.repository_id,
            "job_event.refs.audit_record_id",
        )?;
        ensure_event_policy_consistency(
            &mut event_policy_decision_id,
            &audit_record.policy_decision_id,
            "job_event.refs.audit_record_id",
        )?;
        if let Some(tool_invocation_id) = &audit_record.tool_invocation_id {
            let canonical_invocation =
                inner
                    .tool_invocations
                    .get(tool_invocation_id)
                    .ok_or_else(|| StoreError::Conflict {
                        collection: "audit_records",
                        reason: format!(
                            "audit_record {} references missing invocation {}",
                            id_debug(audit_record_id),
                            id_debug(tool_invocation_id)
                        ),
                    })?;
            if event
                .refs
                .tool_invocation_id
                .as_ref()
                .is_some_and(|id| id != tool_invocation_id)
            {
                return Err(StoreError::InvalidReference {
                    collection: "job_events",
                    reason: format!(
                        "job_event.refs.audit_record_id {} belongs to invocation_id {}, not {:?}",
                        id_debug(audit_record_id),
                        id_debug(tool_invocation_id),
                        event.refs.tool_invocation_id
                    ),
                });
            }
            ensure_event_job_consistency(
                &mut event_job_id,
                &canonical_invocation.job_id,
                "job_event.refs.audit_record_id",
            )?;
            if let Some(tool_result_id) = &event.refs.tool_result_id {
                let tool_result = inner
                    .tool_results
                    .get(tool_result_id)
                    .or_else(|| pending_tool_result.filter(|result| &result.id == tool_result_id))
                    .ok_or_else(|| StoreError::NotFound {
                        collection: "tool_results",
                        id: format!(
                            "{} (job_event.refs.tool_result_id)",
                            id_debug(tool_result_id)
                        ),
                    })?;
                if tool_result.invocation_id != *tool_invocation_id {
                    return Err(StoreError::InvalidReference {
                        collection: "job_events",
                        reason: format!(
                            "job_event.refs.audit_record_id {} belongs to invocation_id {}, but tool_result_id {} belongs to invocation_id {}",
                            id_debug(audit_record_id),
                            id_debug(tool_invocation_id),
                            id_debug(tool_result_id),
                            id_debug(&tool_result.invocation_id)
                        ),
                    });
                }
            }
        }
    }

    Ok(())
}

fn ensure_subject_ref_matches(
    event: &JobEvent,
    expected_subject_type: EventSubjectType,
    ref_id: &str,
    context: &str,
) -> StoreResult<()> {
    if event.subject.subject_type != expected_subject_type || event.subject.subject_id == ref_id {
        return Ok(());
    }

    Err(StoreError::InvalidReference {
        collection: "job_events",
        reason: format!(
            "{context} {} does not match event.subject_id {} for subject_type {:?}",
            ref_id, event.subject.subject_id, event.subject.subject_type
        ),
    })
}

fn ensure_event_job_consistency<'a>(
    expected: &mut Option<&'a str>,
    implied_job_id: &'a JobId,
    context: &str,
) -> StoreResult<()> {
    ensure_event_identity_consistency(expected, implied_job_id.as_str(), context, "job_id")
}

fn ensure_event_policy_consistency<'a>(
    expected: &mut Option<&'a str>,
    implied_policy_decision_id: &'a PolicyDecisionId,
    context: &str,
) -> StoreResult<()> {
    ensure_event_identity_consistency(
        expected,
        implied_policy_decision_id.as_str(),
        context,
        "policy_decision_id",
    )
}

fn ensure_event_identity_consistency<'a>(
    expected: &mut Option<&'a str>,
    actual: &'a str,
    context: &str,
    field_name: &str,
) -> StoreResult<()> {
    if let Some(expected) = *expected {
        if expected == actual {
            return Ok(());
        }

        return Err(StoreError::InvalidReference {
            collection: "job_events",
            reason: format!("{context} implies {field_name} {actual}, not {expected}",),
        });
    }

    *expected = Some(actual);
    Ok(())
}

fn ensure_event_repository_consistency<'a>(
    expected: &mut Option<&'a RepositoryId>,
    actual: &'a RepositoryId,
    context: &str,
) -> StoreResult<()> {
    if let Some(expected) = expected {
        ensure_same_repository("job_events", context, expected, actual)?;
    } else {
        *expected = Some(actual);
    }

    Ok(())
}

fn validate_lock_decision_timing(record: &LockDecision) -> StoreResult<()> {
    if record.expires_at <= record.locked_at {
        return Err(StoreError::InvalidRecord {
            collection: "lock_decisions",
            reason: "expires_at must be later than locked_at".to_string(),
        });
    }
    if record.created_at != record.locked_at {
        return Err(StoreError::InvalidRecord {
            collection: "lock_decisions",
            reason: "created_at must equal locked_at".to_string(),
        });
    }
    if record.updated_at < record.locked_at {
        return Err(StoreError::InvalidRecord {
            collection: "lock_decisions",
            reason: "updated_at must not be earlier than locked_at".to_string(),
        });
    }
    if record.released_at.is_some() && record.reclaimed_at.is_some() {
        return Err(StoreError::InvalidRecord {
            collection: "lock_decisions",
            reason: "released_at and reclaimed_at are mutually exclusive".to_string(),
        });
    }
    match record.status {
        crate::domain::LockStatus::Held | crate::domain::LockStatus::Expired => {
            if record.released_at.is_some() || record.reclaimed_at.is_some() {
                return Err(StoreError::InvalidRecord {
                    collection: "lock_decisions",
                    reason: "active or expired locks must not have terminal timestamps".to_string(),
                });
            }
        }
        crate::domain::LockStatus::Released => {
            let released_at = record
                .released_at
                .ok_or_else(|| StoreError::InvalidRecord {
                    collection: "lock_decisions",
                    reason: "released locks require released_at".to_string(),
                })?;
            if released_at < record.locked_at || record.updated_at < released_at {
                return Err(StoreError::InvalidRecord {
                    collection: "lock_decisions",
                    reason: "released_at must be monotonic with lock timestamps".to_string(),
                });
            }
        }
        crate::domain::LockStatus::Reclaimed => {
            let reclaimed_at = record
                .reclaimed_at
                .ok_or_else(|| StoreError::InvalidRecord {
                    collection: "lock_decisions",
                    reason: "reclaimed locks require reclaimed_at".to_string(),
                })?;
            if reclaimed_at < record.expires_at || record.updated_at < reclaimed_at {
                return Err(StoreError::InvalidRecord {
                    collection: "lock_decisions",
                    reason: "reclaimed_at must be at or after expires_at and updated_at"
                        .to_string(),
                });
            }
        }
    }

    Ok(())
}

fn validate_tool_invocation_refs(
    inner: &InMemoryInner,
    record: &ToolInvocation,
) -> StoreResult<()> {
    let job = inner
        .jobs
        .get(&record.job_id)
        .ok_or_else(|| StoreError::NotFound {
            collection: "jobs",
            id: format!("{} (tool_invocation.job_id)", id_debug(&record.job_id)),
        })?;
    ensure_ref_exists(
        inner.repositories.contains_key(&record.repository_id),
        "repositories",
        &record.repository_id,
        "tool_invocation.repository_id",
    )?;
    ensure_same_repository(
        "tool_invocations",
        "tool_invocation.job_id",
        &record.repository_id,
        &job.repository_id,
    )?;

    let policy_decision = inner
        .policy_decisions
        .get(&record.policy_decision_id)
        .ok_or_else(|| StoreError::NotFound {
            collection: "policy_decisions",
            id: format!(
                "{} (tool_invocation.policy_decision_id)",
                id_debug(&record.policy_decision_id)
            ),
        })?;
    ensure_same_repository(
        "tool_invocations",
        "tool_invocation.policy_decision_id",
        &record.repository_id,
        &policy_decision.repository_id,
    )
}

fn validate_tool_result_refs(inner: &InMemoryInner, record: &ToolResult) -> StoreResult<()> {
    let invocation = inner
        .tool_invocations
        .get(&record.invocation_id)
        .ok_or_else(|| StoreError::NotFound {
            collection: "tool_invocations",
            id: format!(
                "{} (tool_result.invocation_id)",
                id_debug(&record.invocation_id)
            ),
        })?;
    if invocation.tool_id != record.tool_id {
        return Err(StoreError::InvalidReference {
            collection: "tool_results",
            reason: format!(
                "tool_result.tool_id {} does not match invocation tool_id {}",
                record.tool_id, invocation.tool_id
            ),
        });
    }
    if inner
        .tool_results
        .values()
        .any(|existing| existing.invocation_id == record.invocation_id)
    {
        return Err(StoreError::Conflict {
            collection: "tool_results",
            reason: format!(
                "tool_result already exists for invocation {}",
                id_debug(&record.invocation_id)
            ),
        });
    }

    Ok(())
}

fn validate_audit_record_refs(inner: &InMemoryInner, record: &AuditRecord) -> StoreResult<()> {
    validate_audit_record_refs_with_pending_tool_result(inner, record, None)
}

fn validate_audit_record_refs_with_pending_tool_result(
    inner: &InMemoryInner,
    record: &AuditRecord,
    pending_tool_result: Option<&ToolResult>,
) -> StoreResult<()> {
    ensure_ref_exists(
        inner.repositories.contains_key(&record.repository_id),
        "repositories",
        &record.repository_id,
        "audit_record.repository_id",
    )?;
    let policy_decision = inner
        .policy_decisions
        .get(&record.policy_decision_id)
        .ok_or_else(|| StoreError::NotFound {
            collection: "policy_decisions",
            id: format!(
                "{} (audit_record.policy_decision_id)",
                id_debug(&record.policy_decision_id)
            ),
        })?;
    ensure_same_repository(
        "audit_records",
        "audit_record.policy_decision_id",
        &record.repository_id,
        &policy_decision.repository_id,
    )?;

    if let Some(tool_invocation_id) = &record.tool_invocation_id {
        let tool_invocation = inner
            .tool_invocations
            .get(tool_invocation_id)
            .ok_or_else(|| StoreError::NotFound {
                collection: "tool_invocations",
                id: format!(
                    "{} (audit_record.tool_invocation_id)",
                    id_debug(tool_invocation_id)
                ),
            })?;
        ensure_same_repository(
            "audit_records",
            "audit_record.tool_invocation_id",
            &record.repository_id,
            &tool_invocation.repository_id,
        )?;
        if tool_invocation.policy_decision_id != record.policy_decision_id {
            return Err(StoreError::InvalidReference {
                collection: "audit_records",
                reason: format!(
                    "audit_record.tool_invocation_id {} belongs to policy_decision_id {}, not {}",
                    id_debug(tool_invocation_id),
                    id_debug(&tool_invocation.policy_decision_id),
                    id_debug(&record.policy_decision_id)
                ),
            });
        }
        if let Some(tool_result) = pending_tool_result {
            if tool_result.invocation_id != *tool_invocation_id {
                return Err(StoreError::InvalidReference {
                    collection: "audit_records",
                    reason: format!(
                        "pending tool_result {} belongs to invocation_id {}, not {}",
                        id_debug(&tool_result.id),
                        id_debug(&tool_result.invocation_id),
                        id_debug(tool_invocation_id)
                    ),
                });
            }
        }
    }

    Ok(())
}

fn ensure_same_repository(
    collection: &'static str,
    context: &str,
    expected: &RepositoryId,
    actual: &RepositoryId,
) -> StoreResult<()> {
    if actual == expected {
        return Ok(());
    }

    Err(StoreError::InvalidReference {
        collection,
        reason: format!(
            "{context} belongs to repository {}, not {}",
            id_debug(actual),
            id_debug(expected)
        ),
    })
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

fn ensure_ref_absent<Id: fmt::Debug>(
    absent: bool,
    collection: &'static str,
    id: &Id,
) -> StoreResult<()> {
    if absent {
        return Ok(());
    }

    Err(StoreError::DuplicateId {
        collection,
        id: id_debug(id),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{
        Actor, CancellationState, EventRefs, EventSeverity, EventSubject, EventSubjectType,
        JobEventKind, JobKind, LedgerTimestamp, LockOwner, LockedScope, PolicyOutcome,
        PolicySummary, RepositoryTrustState, ResourceScope, RiskTier, StructuredValue,
        ToolResultField, ToolResultStatus,
    };
    use std::sync::atomic::{AtomicU64, Ordering};

    static REPOSITORY_ROOT_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn timestamp(value: i64) -> LedgerTimestamp {
        LedgerTimestamp::from_unix_millis(value)
    }

    fn actor() -> Actor {
        Actor::System {
            id: "store-test".to_string(),
        }
    }

    fn repository_record() -> RepositoryRecord {
        let counter = REPOSITORY_ROOT_COUNTER.fetch_add(1, Ordering::Relaxed);
        RepositoryRecord::new(
            "store test repository",
            format!("/tmp/atelia-store-test-{counter}"),
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

    fn job_event(repository_id: RepositoryId) -> JobEvent {
        JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(3),
            subject: EventSubject::repository(&repository_id),
            kind: JobEventKind::Message,
            severity: EventSeverity::Info,
            public_message: "store test event".to_string(),
            refs: EventRefs {
                repository_id: Some(repository_id),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        }
    }

    fn initial_job_event(repository_id: RepositoryId, job_id: &JobId) -> JobEvent {
        let mut event = job_event(repository_id);
        event.subject = EventSubject::job(job_id);
        event.refs.job_id = Some(job_id.clone());
        event.kind = JobEventKind::JobSubmitted;
        event
    }

    fn persist_job(store: &InMemoryStore, job: JobRecord) -> JobRecord {
        let event = initial_job_event(job.repository_id.clone(), &job.id);
        store
            .create_job_with_initial_event(job.clone(), event.clone())
            .unwrap();

        let mut stored_job = job;
        stored_job.latest_event_id = Some(event.id);
        stored_job
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
        .unwrap()
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
        let job = persist_job(&store, job);

        assert_eq!(store.get_job(&job.id).unwrap(), job);
        assert_eq!(store.list_jobs().unwrap().len(), 1);
    }

    #[test]
    fn append_events_assigns_monotonic_sequences() {
        let store = InMemoryStore::new();
        let repository = repository_record();

        store.create_repository(repository.clone()).unwrap();

        let first = store
            .append_job_event(job_event(repository.id.clone()))
            .unwrap();
        let second = store.append_job_event(job_event(repository.id)).unwrap();

        assert_eq!(first.sequence_number, 1);
        assert_eq!(second.sequence_number, 2);
        assert_eq!(store.get_job_event(&first.id).unwrap(), first);
    }

    #[test]
    fn append_event_reports_sequence_overflow() {
        let store = InMemoryStore::new();
        let repository = repository_record();

        store.create_repository(repository.clone()).unwrap();
        store.lock().unwrap().next_event_sequence = u64::MAX;

        assert_eq!(
            Err(StoreError::SequenceOverflow),
            store.append_job_event(job_event(repository.id))
        );
    }

    #[test]
    fn append_job_event_and_update_job_rejects_stale_job_snapshot() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let stale_job = persist_job(&store, job);
        let mut fresh_job = stale_job.clone();
        fresh_job
            .transition_status(JobStatus::Running, timestamp(5))
            .unwrap();

        let mut first_update = job_event(repository.id.clone());
        first_update.subject = EventSubject::job(&fresh_job.id);
        first_update.kind = JobEventKind::JobStatusChanged {
            from: JobStatus::Queued,
            to: JobStatus::Running,
        };
        first_update.refs.job_id = Some(fresh_job.id.clone());
        store
            .append_job_event_and_update_job(fresh_job, first_update)
            .unwrap();

        let mut stale_update = stale_job.clone();
        stale_update
            .transition_status(JobStatus::Canceled, timestamp(6))
            .unwrap();
        let mut stale_event = job_event(repository.id);
        stale_event.subject = EventSubject::job(&stale_update.id);
        stale_event.kind = JobEventKind::JobStatusChanged {
            from: JobStatus::Queued,
            to: JobStatus::Canceled,
        };
        stale_event.refs.job_id = Some(stale_update.id.clone());

        assert!(matches!(
            store.append_job_event_and_update_job(stale_update, stale_event),
            Err(StoreError::Conflict {
                collection: "jobs",
                ..
            })
        ));
        assert_eq!(
            JobStatus::Running,
            store.get_job(&stale_job.id).unwrap().status
        );
    }

    #[test]
    fn append_job_events_and_update_job_rejects_mixed_job_identity_without_partial_update() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let first_job = job_record(repository.id.clone());
        let second_job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let mut first_job = persist_job(&store, first_job);
        let second_job = persist_job(&store, second_job);

        first_job.cancellation_state = CancellationState::Requested;
        let mut first_event = job_event(repository.id.clone());
        first_event.subject = EventSubject::job(&first_job.id);
        first_event.kind = JobEventKind::CancellationRequested;
        first_event.refs.job_id = Some(first_job.id.clone());

        let mut final_job = second_job.clone();
        final_job.latest_event_id = Some(first_event.id.clone());
        final_job
            .transition_status(JobStatus::Canceled, timestamp(6))
            .unwrap();
        let mut final_event = job_event(repository.id);
        final_event.subject = EventSubject::job(&second_job.id);
        final_event.kind = JobEventKind::JobStatusChanged {
            from: JobStatus::Queued,
            to: JobStatus::Canceled,
        };
        final_event.refs.job_id = Some(second_job.id.clone());

        assert!(matches!(
            store.append_job_events_and_update_job(
                first_job.clone(),
                first_event,
                final_job,
                final_event
            ),
            Err(StoreError::InvalidReference { .. })
        ));

        let stored_first = store.get_job(&first_job.id).unwrap();
        assert_eq!(JobStatus::Queued, stored_first.status);
        assert_eq!(
            CancellationState::NotRequested,
            stored_first.cancellation_state
        );
        assert_eq!(first_job.latest_event_id, stored_first.latest_event_id);
    }

    #[test]
    fn append_job_event_and_update_job_rejects_immutable_job_field_changes() {
        fn assert_rejects_change(
            mutate: impl FnOnce(&mut JobRecord),
            expected_field_name: &'static str,
        ) {
            let store = InMemoryStore::new();
            let repository = repository_record();
            let job = job_record(repository.id.clone());

            store.create_repository(repository.clone()).unwrap();
            let mut job = persist_job(&store, job);
            mutate(&mut job);

            let mut event = job_event(repository.id);
            event.subject = EventSubject::job(&job.id);
            event.refs.job_id = Some(job.id.clone());

            let error = store
                .append_job_event_and_update_job(job, event)
                .unwrap_err();

            match error {
                StoreError::Conflict { collection, reason } => {
                    assert_eq!("jobs", collection);
                    assert!(
                        reason.contains(expected_field_name),
                        "reason should name {expected_field_name}: {reason}"
                    );
                }
                error => panic!("expected immutable field conflict, got {error:?}"),
            }
        }

        assert_rejects_change(|job| job.schema_version += 1, "schema_version");
        assert_rejects_change(|job| job.created_at = timestamp(99), "created_at");
        assert_rejects_change(
            |job| {
                job.requester = Actor::User {
                    id: "different-user".to_string(),
                    display_name: None,
                };
            },
            "requester",
        );
        assert_rejects_change(|job| job.kind = JobKind::Maintenance, "kind");
        assert_rejects_change(|job| job.goal = "different goal".to_string(), "goal");
    }

    #[test]
    fn append_job_event_and_update_job_rejects_invalid_lifecycle_updates() {
        fn assert_rejects_lifecycle_update(mutate: impl FnOnce(&mut JobRecord)) {
            let store = InMemoryStore::new();
            let repository = repository_record();
            let job = job_record(repository.id.clone());

            store.create_repository(repository.clone()).unwrap();
            let mut job = persist_job(&store, job);
            mutate(&mut job);

            let mut event = job_event(repository.id);
            event.subject = EventSubject::job(&job.id);
            event.refs.job_id = Some(job.id.clone());

            assert!(matches!(
                store.append_job_event_and_update_job(job, event),
                Err(StoreError::Conflict {
                    collection: "jobs",
                    ..
                })
            ));
        }

        assert_rejects_lifecycle_update(|job| {
            job.status = JobStatus::Succeeded;
            job.updated_at = timestamp(10);
            job.completed_at = Some(timestamp(10));
        });
        assert_rejects_lifecycle_update(|job| {
            job.updated_at = timestamp(1);
        });
        assert_rejects_lifecycle_update(|job| {
            job.status = JobStatus::Running;
            job.updated_at = timestamp(10);
        });
    }

    #[test]
    fn append_job_event_and_update_job_rejects_invalid_cancellation_state_updates() {
        fn assert_rejects_cancellation_update(
            mutate_job: impl FnOnce(&mut JobRecord),
            mutate_event: impl FnOnce(&mut JobEvent),
        ) {
            let store = InMemoryStore::new();
            let repository = repository_record();
            let job = job_record(repository.id.clone());

            store.create_repository(repository.clone()).unwrap();
            let mut job = persist_job(&store, job);
            mutate_job(&mut job);

            let mut event = job_event(repository.id);
            event.subject = EventSubject::job(&job.id);
            event.refs.job_id = Some(job.id.clone());
            mutate_event(&mut event);

            assert!(matches!(
                store.append_job_event_and_update_job(job, event),
                Err(StoreError::Conflict {
                    collection: "jobs",
                    ..
                })
            ));
        }

        assert_rejects_cancellation_update(
            |job| job.cancellation_state = CancellationState::Requested,
            |_| {},
        );
        assert_rejects_cancellation_update(
            |_| {},
            |event| event.kind = JobEventKind::CancellationRequested,
        );
        assert_rejects_cancellation_update(
            |job| job.cancellation_state = CancellationState::ForceStop,
            |event| event.kind = JobEventKind::CancellationRequested,
        );
    }

    #[test]
    fn append_job_event_and_update_job_rejects_policy_summary_without_policy_event() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let mut job = persist_job(&store, job);
        let policy = policy_decision(repository.id.clone());
        store.create_policy_decision(policy.clone()).unwrap();
        job.policy_summary = Some(PolicySummary {
            decision_id: Some(policy.id),
            outcome: policy.outcome,
            risk_tier: policy.risk_tier,
            reason_code: policy.reason_code,
        });

        let mut event = job_event(repository.id);
        event.subject = EventSubject::job(&job.id);
        event.refs.job_id = Some(job.id.clone());

        assert!(matches!(
            store.append_job_event_and_update_job(job, event),
            Err(StoreError::Conflict {
                collection: "jobs",
                ..
            })
        ));
    }

    #[test]
    fn record_tool_result_and_audit_with_events_rejects_without_partial_writes() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let job = persist_job(&store, job);
        let policy = policy_decision(repository.id.clone());
        let invocation = tool_invocation(repository.id.clone(), job.id.clone(), policy.id.clone());
        let result = tool_result(invocation.id.clone());
        let existing_audit = audit_record(
            repository.id.clone(),
            policy.id.clone(),
            Some(invocation.id.clone()),
        );
        let duplicate_audit = existing_audit.clone();

        store.create_policy_decision(policy.clone()).unwrap();
        store.create_tool_invocation(invocation.clone()).unwrap();
        store.create_audit_record(existing_audit).unwrap();

        let result_event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(10),
            subject: EventSubject::tool_result(&result.id),
            kind: JobEventKind::ToolResultRecorded {
                status: result.status,
            },
            severity: EventSeverity::Info,
            public_message: "tool result recorded".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id.clone()),
                job_id: Some(job.id.clone()),
                policy_decision_id: Some(policy.id.clone()),
                tool_invocation_id: Some(invocation.id.clone()),
                tool_result_id: Some(result.id.clone()),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };
        let audit_event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(11),
            subject: EventSubject::audit_record(&duplicate_audit.id),
            kind: JobEventKind::AuditRecorded,
            severity: EventSeverity::Info,
            public_message: "audit record created".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id),
                job_id: Some(job.id.clone()),
                policy_decision_id: Some(policy.id),
                tool_invocation_id: Some(invocation.id),
                tool_result_id: Some(result.id.clone()),
                audit_record_id: Some(duplicate_audit.id.clone()),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };

        assert!(matches!(
            store.record_tool_result_and_audit_with_events(
                job.clone(),
                result,
                result_event,
                duplicate_audit,
                audit_event,
            ),
            Err(StoreError::DuplicateId {
                collection: "audit_records",
                ..
            })
        ));
        assert!(store.list_tool_results().unwrap().is_empty());
        assert_eq!(
            1,
            store
                .replay_job_events(EventCursor::Beginning, None)
                .unwrap()
                .len()
        );
        assert_eq!(job, store.get_job(&job.id).unwrap());
    }

    #[test]
    fn record_tool_result_and_audit_with_events_rejects_duplicate_sibling_event_ids() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let job = persist_job(&store, job);
        let policy = policy_decision(repository.id.clone());
        let invocation = tool_invocation(repository.id.clone(), job.id.clone(), policy.id.clone());
        let result = tool_result(invocation.id.clone());
        let audit = audit_record(
            repository.id.clone(),
            policy.id.clone(),
            Some(invocation.id.clone()),
        );

        store.create_policy_decision(policy.clone()).unwrap();
        store.create_tool_invocation(invocation.clone()).unwrap();

        let shared_event_id = JobEventId::new();
        let result_event = JobEvent {
            id: shared_event_id.clone(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(10),
            subject: EventSubject::tool_result(&result.id),
            kind: JobEventKind::ToolResultRecorded {
                status: result.status,
            },
            severity: EventSeverity::Info,
            public_message: "tool result recorded".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id.clone()),
                job_id: Some(job.id.clone()),
                policy_decision_id: Some(policy.id.clone()),
                tool_invocation_id: Some(invocation.id.clone()),
                tool_result_id: Some(result.id.clone()),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };
        let audit_event = JobEvent {
            id: shared_event_id,
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(11),
            subject: EventSubject::audit_record(&audit.id),
            kind: JobEventKind::AuditRecorded,
            severity: EventSeverity::Info,
            public_message: "audit record created".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id),
                job_id: Some(job.id.clone()),
                policy_decision_id: Some(policy.id),
                tool_invocation_id: Some(invocation.id),
                tool_result_id: Some(result.id.clone()),
                audit_record_id: Some(audit.id.clone()),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };

        assert!(matches!(
            store.record_tool_result_and_audit_with_events(
                job.clone(),
                result,
                result_event,
                audit,
                audit_event,
            ),
            Err(StoreError::DuplicateId {
                collection: "job_events",
                ..
            })
        ));
        assert!(store.list_tool_results().unwrap().is_empty());
        assert!(store.list_audit_records().unwrap().is_empty());
        assert_eq!(job, store.get_job(&job.id).unwrap());
    }

    #[test]
    fn record_tool_result_and_audit_with_events_rejects_wrong_audit_event_job_refs() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());
        let other_job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let job = persist_job(&store, job);
        let other_job = persist_job(&store, other_job);
        let policy = policy_decision(repository.id.clone());
        let invocation = tool_invocation(repository.id.clone(), job.id.clone(), policy.id.clone());
        let result = tool_result(invocation.id.clone());
        let audit = audit_record(
            repository.id.clone(),
            policy.id.clone(),
            Some(invocation.id.clone()),
        );

        store.create_policy_decision(policy.clone()).unwrap();
        store.create_tool_invocation(invocation.clone()).unwrap();

        let result_event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(10),
            subject: EventSubject::tool_result(&result.id),
            kind: JobEventKind::ToolResultRecorded {
                status: result.status,
            },
            severity: EventSeverity::Info,
            public_message: "tool result recorded".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id.clone()),
                job_id: Some(job.id.clone()),
                policy_decision_id: Some(policy.id.clone()),
                tool_invocation_id: Some(invocation.id.clone()),
                tool_result_id: Some(result.id.clone()),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };
        let audit_event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(11),
            subject: EventSubject::audit_record(&audit.id),
            kind: JobEventKind::AuditRecorded,
            severity: EventSeverity::Info,
            public_message: "audit record created".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id),
                job_id: Some(other_job.id),
                policy_decision_id: Some(policy.id),
                tool_invocation_id: Some(invocation.id),
                tool_result_id: Some(result.id.clone()),
                audit_record_id: Some(audit.id.clone()),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };

        assert!(matches!(
            store.record_tool_result_and_audit_with_events(
                job.clone(),
                result,
                result_event,
                audit,
                audit_event,
            ),
            Err(StoreError::InvalidReference {
                collection: "job_events",
                ..
            })
        ));
        assert!(store.list_tool_results().unwrap().is_empty());
        assert!(store.list_audit_records().unwrap().is_empty());
        assert_eq!(job, store.get_job(&job.id).unwrap());
    }

    #[test]
    fn job_events_reject_subject_and_ref_identity_mismatches() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let job = persist_job(&store, job);
        let policy = policy_decision(repository.id.clone());
        let invocation = tool_invocation(repository.id.clone(), job.id.clone(), policy.id.clone());
        let result = tool_result(invocation.id.clone());
        let audit = audit_record(
            repository.id.clone(),
            policy.id.clone(),
            Some(invocation.id.clone()),
        );

        store.create_policy_decision(policy.clone()).unwrap();
        store.create_tool_invocation(invocation.clone()).unwrap();

        let result_event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(10),
            subject: EventSubject::tool_result(&result.id),
            kind: JobEventKind::ToolResultRecorded {
                status: result.status,
            },
            severity: EventSeverity::Info,
            public_message: "tool result recorded".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id.clone()),
                job_id: Some(job.id.clone()),
                policy_decision_id: Some(policy.id.clone()),
                tool_invocation_id: Some(invocation.id.clone()),
                tool_result_id: Some(ToolResultId::new()),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };
        let audit_event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(11),
            subject: EventSubject::audit_record(&audit.id),
            kind: JobEventKind::AuditRecorded,
            severity: EventSeverity::Info,
            public_message: "audit record created".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id),
                job_id: Some(job.id.clone()),
                policy_decision_id: Some(policy.id),
                tool_invocation_id: Some(invocation.id),
                tool_result_id: Some(result.id.clone()),
                audit_record_id: Some(audit.id.clone()),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };

        assert!(matches!(
            store.record_tool_result_and_audit_with_events(
                job.clone(),
                result,
                result_event,
                audit,
                audit_event,
            ),
            Err(StoreError::InvalidReference {
                collection: "job_events",
                ..
            })
        ));
        assert!(store.list_tool_results().unwrap().is_empty());
        assert!(store.list_audit_records().unwrap().is_empty());
        assert_eq!(job, store.get_job(&job.id).unwrap());
    }

    #[test]
    fn job_events_reject_policy_refs_that_contradict_referenced_records() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let job = persist_job(&store, job);
        let first_policy = policy_decision(repository.id.clone());
        let second_policy = policy_decision(repository.id.clone());
        let invocation = tool_invocation(
            repository.id.clone(),
            job.id.clone(),
            first_policy.id.clone(),
        );
        let result = tool_result(invocation.id.clone());
        let audit = audit_record(
            repository.id.clone(),
            first_policy.id.clone(),
            Some(invocation.id.clone()),
        );

        store.create_policy_decision(first_policy.clone()).unwrap();
        store.create_policy_decision(second_policy.clone()).unwrap();
        store.create_tool_invocation(invocation.clone()).unwrap();

        let invocation_event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(10),
            subject: EventSubject::tool_invocation(&invocation.id),
            kind: JobEventKind::ToolInvoked {
                tool_id: invocation.tool_id.clone(),
            },
            severity: EventSeverity::Info,
            public_message: "tool invoked".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id.clone()),
                job_id: Some(job.id.clone()),
                policy_decision_id: Some(second_policy.id.clone()),
                tool_invocation_id: Some(invocation.id.clone()),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };
        assert!(matches!(
            store.append_job_event(invocation_event),
            Err(StoreError::InvalidReference {
                collection: "job_events",
                ..
            })
        ));

        store.create_tool_result(result.clone()).unwrap();
        let result_event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(11),
            subject: EventSubject::tool_result(&result.id),
            kind: JobEventKind::ToolResultRecorded {
                status: result.status,
            },
            severity: EventSeverity::Info,
            public_message: "tool result recorded".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id.clone()),
                job_id: Some(job.id.clone()),
                policy_decision_id: Some(second_policy.id.clone()),
                tool_invocation_id: Some(invocation.id.clone()),
                tool_result_id: Some(result.id.clone()),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };
        assert!(matches!(
            store.append_job_event(result_event),
            Err(StoreError::InvalidReference {
                collection: "job_events",
                ..
            })
        ));

        store.create_audit_record(audit.clone()).unwrap();
        let audit_event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(12),
            subject: EventSubject::audit_record(&audit.id),
            kind: JobEventKind::AuditRecorded,
            severity: EventSeverity::Info,
            public_message: "audit record created".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id),
                job_id: Some(job.id),
                policy_decision_id: Some(second_policy.id),
                tool_invocation_id: Some(invocation.id),
                audit_record_id: Some(audit.id),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };
        assert!(matches!(
            store.append_job_event(audit_event),
            Err(StoreError::InvalidReference {
                collection: "job_events",
                ..
            })
        ));
    }

    #[test]
    fn job_events_reject_mixed_implied_policy_refs_without_explicit_policy() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let job = persist_job(&store, job);
        let first_policy = policy_decision(repository.id.clone());
        let second_policy = policy_decision(repository.id.clone());
        let lock_decision = lock_decision(repository.id.clone(), first_policy.id.clone());
        let invocation = tool_invocation(
            repository.id.clone(),
            job.id.clone(),
            second_policy.id.clone(),
        );

        store.create_policy_decision(first_policy).unwrap();
        store.create_policy_decision(second_policy).unwrap();
        store.create_lock_decision(lock_decision.clone()).unwrap();
        store.create_tool_invocation(invocation.clone()).unwrap();

        let event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(10),
            subject: EventSubject::tool_invocation(&invocation.id),
            kind: JobEventKind::ToolInvoked {
                tool_id: invocation.tool_id.clone(),
            },
            severity: EventSeverity::Info,
            public_message: "tool invoked".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id),
                job_id: Some(job.id),
                lock_decision_id: Some(lock_decision.id),
                tool_invocation_id: Some(invocation.id),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };

        assert!(matches!(
            store.append_job_event(event),
            Err(StoreError::InvalidReference {
                collection: "job_events",
                ..
            })
        ));
    }

    #[test]
    fn job_events_reject_subject_job_conflicts_when_job_ref_is_omitted() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let subject_job = job_record(repository.id.clone());
        let invocation_job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let subject_job = persist_job(&store, subject_job);
        let invocation_job = persist_job(&store, invocation_job);
        let policy = policy_decision(repository.id.clone());
        let invocation = tool_invocation(
            repository.id.clone(),
            invocation_job.id.clone(),
            policy.id.clone(),
        );

        store.create_policy_decision(policy.clone()).unwrap();
        store.create_tool_invocation(invocation.clone()).unwrap();

        let event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(10),
            subject: EventSubject::job(&subject_job.id),
            kind: JobEventKind::ToolInvoked {
                tool_id: invocation.tool_id.clone(),
            },
            severity: EventSeverity::Info,
            public_message: "tool invoked".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id),
                policy_decision_id: Some(policy.id),
                tool_invocation_id: Some(invocation.id),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };

        assert!(matches!(
            store.append_job_event(event),
            Err(StoreError::InvalidReference {
                collection: "job_events",
                ..
            })
        ));
    }

    #[test]
    fn job_events_reject_subject_policy_conflicts_when_policy_ref_is_omitted() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let job = persist_job(&store, job);
        let subject_policy = policy_decision(repository.id.clone());
        let invocation_policy = policy_decision(repository.id.clone());
        let invocation = tool_invocation(
            repository.id.clone(),
            job.id.clone(),
            invocation_policy.id.clone(),
        );

        store
            .create_policy_decision(subject_policy.clone())
            .unwrap();
        store.create_policy_decision(invocation_policy).unwrap();
        store.create_tool_invocation(invocation.clone()).unwrap();

        let event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(10),
            subject: EventSubject::policy_decision(&subject_policy.id),
            kind: JobEventKind::ToolInvoked {
                tool_id: invocation.tool_id.clone(),
            },
            severity: EventSeverity::Info,
            public_message: "tool invoked".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id),
                job_id: Some(job.id),
                tool_invocation_id: Some(invocation.id),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };

        assert!(matches!(
            store.append_job_event(event),
            Err(StoreError::InvalidReference {
                collection: "job_events",
                ..
            })
        ));
    }

    #[test]
    fn job_events_reject_audit_record_refs_that_contradict_canonical_invocation() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let job = persist_job(&store, job);
        let policy = policy_decision(repository.id.clone());
        let first_invocation =
            tool_invocation(repository.id.clone(), job.id.clone(), policy.id.clone());
        let second_invocation =
            tool_invocation(repository.id.clone(), job.id.clone(), policy.id.clone());
        let result_for_second_invocation = tool_result(second_invocation.id.clone());
        let audit_for_first_invocation = audit_record(
            repository.id.clone(),
            policy.id.clone(),
            Some(first_invocation.id.clone()),
        );

        store.create_policy_decision(policy.clone()).unwrap();
        store.create_tool_invocation(first_invocation).unwrap();
        store
            .create_tool_invocation(second_invocation.clone())
            .unwrap();
        store
            .create_tool_result(result_for_second_invocation.clone())
            .unwrap();
        store
            .create_audit_record(audit_for_first_invocation.clone())
            .unwrap();

        let audit_event = JobEvent {
            id: JobEventId::new(),
            schema_version: 1,
            sequence_number: 0,
            created_at: timestamp(13),
            subject: EventSubject::audit_record(&audit_for_first_invocation.id),
            kind: JobEventKind::AuditRecorded,
            severity: EventSeverity::Info,
            public_message: "audit record created".to_string(),
            refs: EventRefs {
                repository_id: Some(repository.id),
                job_id: Some(job.id),
                policy_decision_id: Some(policy.id),
                tool_result_id: Some(result_for_second_invocation.id),
                audit_record_id: Some(audit_for_first_invocation.id),
                ..EventRefs::default()
            },
            redactions: Vec::new(),
        };

        assert!(matches!(
            store.append_job_event(audit_event),
            Err(StoreError::InvalidReference {
                collection: "job_events",
                ..
            })
        ));
    }

    #[test]
    fn replay_from_cursor_returns_events_after_cursor() {
        let store = InMemoryStore::new();
        let repository = repository_record();

        store.create_repository(repository.clone()).unwrap();

        let first = store
            .append_job_event(job_event(repository.id.clone()))
            .unwrap();
        let second = store
            .append_job_event(job_event(repository.id.clone()))
            .unwrap();
        let third = store.append_job_event(job_event(repository.id)).unwrap();

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
        let repository = repository_record();

        store.create_repository(repository.clone()).unwrap();

        store.append_job_event(job_event(repository.id)).unwrap();

        assert_eq!(
            store
                .replay_job_events(EventCursor::AfterSequence(u64::MAX), None)
                .unwrap(),
            Vec::<JobEvent>::new()
        );
    }

    #[test]
    fn query_jobs_filters_and_paginates_without_full_scan_contract() {
        let store = InMemoryStore::new();
        let first_repository = repository_record();
        let second_repository = repository_record();
        let first_job = job_record(first_repository.id.clone());
        let second_job = job_record(second_repository.id.clone());

        store.create_repository(first_repository.clone()).unwrap();
        store.create_repository(second_repository).unwrap();
        let first_job = persist_job(&store, first_job);
        persist_job(&store, second_job);

        let page = store
            .query_jobs(JobQuery {
                repository_id: Some(first_repository.id),
                status: Some(JobStatus::Queued),
                page_size: Some(1),
                page_token: None,
                requester: None,
            })
            .unwrap();

        assert_eq!(page.jobs, vec![first_job]);
        assert_eq!(page.next_page_token, None);
    }

    #[test]
    fn query_jobs_uses_stable_order_and_zero_size_does_not_loop() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let first_job = job_record(repository.id.clone());
        let second_job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let second_job = persist_job(&store, second_job);
        let first_job = persist_job(&store, first_job);

        let page = store
            .query_jobs(JobQuery {
                repository_id: Some(repository.id.clone()),
                page_size: Some(10),
                ..JobQuery::default()
            })
            .unwrap();
        let mut expected = vec![first_job, second_job];
        expected.sort_by(|left, right| left.id.cmp(&right.id));

        assert_eq!(page.jobs, expected);

        let empty_page = store
            .query_jobs(JobQuery {
                repository_id: Some(repository.id),
                page_size: Some(0),
                ..JobQuery::default()
            })
            .unwrap();

        assert!(empty_page.jobs.is_empty());
        assert_eq!(empty_page.next_page_token, None);
    }

    #[test]
    fn query_job_events_filters_by_repository_subject_and_severity() {
        let store = InMemoryStore::new();
        let first_repository = repository_record();
        let second_repository = repository_record();
        let mut first_event = job_event(first_repository.id.clone());
        let second_event = job_event(second_repository.id.clone());
        first_event.severity = EventSeverity::Warning;

        store.create_repository(first_repository.clone()).unwrap();
        store.create_repository(second_repository).unwrap();
        let first_event = store.append_job_event(first_event).unwrap();
        store.append_job_event(second_event).unwrap();

        let page = store
            .query_job_events(EventQuery {
                repository_id: Some(first_repository.id),
                cursor: EventCursor::Beginning,
                subject_ids: vec![first_event.subject.subject_id.clone()],
                min_severity: Some(EventSeverity::Warning),
                page_size: Some(1),
                page_token: None,
            })
            .unwrap();

        assert_eq!(page.events, vec![first_event]);
        assert_eq!(page.next_page_token, None);
    }

    #[test]
    fn query_job_events_infers_repository_from_subject_when_ref_is_omitted() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());

        store.create_repository(repository.clone()).unwrap();
        let job = persist_job(&store, job);

        let mut event = job_event(repository.id.clone());
        event.subject = EventSubject::job(&job.id);
        event.refs.repository_id = None;
        event.refs.job_id = None;
        let event = store.append_job_event(event).unwrap();

        let page = store
            .query_job_events(EventQuery {
                repository_id: Some(repository.id),
                cursor: EventCursor::Beginning,
                page_size: Some(10),
                ..EventQuery::default()
            })
            .unwrap();

        assert!(page.events.contains(&event));
    }

    #[test]
    fn query_job_events_zero_size_does_not_loop() {
        let store = InMemoryStore::new();
        let repository = repository_record();

        store.create_repository(repository.clone()).unwrap();
        store
            .append_job_event(job_event(repository.id.clone()))
            .unwrap();

        let page = store
            .query_job_events(EventQuery {
                repository_id: Some(repository.id),
                page_size: Some(0),
                ..EventQuery::default()
            })
            .unwrap();

        assert!(page.events.is_empty());
        assert_eq!(page.next_page_token, None);
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
    fn duplicate_repository_roots_are_rejected() {
        let store = InMemoryStore::new();
        let first = repository_record();
        let mut second = repository_record();
        second.id = RepositoryId::new();
        second.root_path = first.root_path.clone();
        second.allowed_path_scope = first.allowed_path_scope.clone();

        store.create_repository(first).unwrap();

        assert!(matches!(
            store.create_repository(second),
            Err(StoreError::DuplicateId {
                collection: "repositories",
                ..
            })
        ));
    }

    #[test]
    fn repository_records_reject_mismatched_allowed_scope_root() {
        let store = InMemoryStore::new();
        let mut repository = repository_record();
        repository.allowed_path_scope.root_path = format!("{}/scope", repository.root_path);

        assert!(matches!(
            store.create_repository(repository),
            Err(StoreError::InvalidRecord {
                collection: "repositories",
                ..
            })
        ));
    }

    #[test]
    fn duplicate_event_ids_are_rejected() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let event = job_event(repository.id.clone());

        store.create_repository(repository).unwrap();

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
    fn create_job_with_initial_event_persists_atomically() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());
        let mut event = job_event(repository.id.clone());
        event.subject = EventSubject::job(&job.id);
        event.refs.job_id = Some(job.id.clone());

        assert!(matches!(
            store.create_job_with_initial_event(job.clone(), event.clone()),
            Err(StoreError::NotFound {
                collection: "repositories",
                ..
            })
        ));
        assert!(matches!(
            store.get_job(&job.id),
            Err(StoreError::NotFound { .. })
        ));

        store.create_repository(repository).unwrap();
        let stored_event = store
            .create_job_with_initial_event(job.clone(), event.clone())
            .unwrap();
        let mut stored_job = job;
        stored_job.latest_event_id = Some(event.id.clone());

        assert_eq!(store.get_job(&stored_job.id).unwrap(), stored_job);
        assert_eq!(stored_event.sequence_number, 1);
        assert_eq!(store.get_job_event(&event.id).unwrap(), stored_event);
    }

    #[test]
    fn create_job_with_initial_event_rejects_events_for_other_subjects() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());
        let event = job_event(repository.id.clone());

        store.create_repository(repository).unwrap();

        assert!(matches!(
            store.create_job_with_initial_event(job.clone(), event),
            Err(StoreError::InvalidReference {
                collection: "job_events",
                ..
            })
        ));
        assert!(matches!(
            store.get_job(&job.id),
            Err(StoreError::NotFound { .. })
        ));
    }

    #[test]
    fn job_events_reject_invalid_subjects_and_missing_refs() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let mut event = job_event(repository.id.clone());
        event.subject = EventSubject {
            subject_type: EventSubjectType::Job,
            subject_id: repository.id.as_str().to_string(),
        };

        store.create_repository(repository.clone()).unwrap();

        assert!(matches!(
            store.append_job_event(event),
            Err(StoreError::InvalidReference {
                collection: "job_events",
                ..
            })
        ));

        let mut event = job_event(repository.id);
        event.refs.policy_decision_id = Some(PolicyDecisionId::new());

        assert!(matches!(
            store.append_job_event(event),
            Err(StoreError::NotFound {
                collection: "policy_decisions",
                ..
            })
        ));
    }

    #[test]
    fn job_events_reject_cross_repository_refs() {
        let store = InMemoryStore::new();
        let first_repository = repository_record();
        let second_repository = repository_record();
        let second_job = job_record(second_repository.id.clone());
        let mut event = job_event(first_repository.id.clone());
        event.refs.job_id = Some(second_job.id.clone());
        let mut event_without_repository_ref = event.clone();
        event_without_repository_ref.id = JobEventId::new();
        event_without_repository_ref.refs.repository_id = None;

        store.create_repository(first_repository).unwrap();
        store.create_repository(second_repository).unwrap();
        persist_job(&store, second_job);

        assert!(matches!(
            store.append_job_event(event),
            Err(StoreError::InvalidReference {
                collection: "job_events",
                ..
            })
        ));
        assert!(matches!(
            store.append_job_event(event_without_repository_ref),
            Err(StoreError::InvalidReference {
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
    fn lock_decisions_reject_cross_repository_policy_decisions() {
        let store = InMemoryStore::new();
        let first_repository = repository_record();
        let second_repository = repository_record();
        let policy = policy_decision(second_repository.id.clone());
        let lock = lock_decision(first_repository.id.clone(), policy.id.clone());

        store.create_repository(first_repository).unwrap();
        store.create_repository(second_repository).unwrap();
        store.create_policy_decision(policy).unwrap();

        assert!(matches!(
            store.create_lock_decision(lock),
            Err(StoreError::InvalidReference {
                collection: "lock_decisions",
                ..
            })
        ));
    }

    #[test]
    fn policy_decisions_require_existing_repository() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let policy = policy_decision(repository.id.clone());

        assert!(matches!(
            store.create_policy_decision(policy.clone()),
            Err(StoreError::NotFound {
                collection: "repositories",
                ..
            })
        ));

        store.create_repository(repository).unwrap();
        store.create_policy_decision(policy).unwrap();
    }

    #[test]
    fn lock_decisions_reject_missing_or_cross_repository_job_owner() {
        let store = InMemoryStore::new();
        let first_repository = repository_record();
        let second_repository = repository_record();
        let first_policy = policy_decision(first_repository.id.clone());
        let second_job = job_record(second_repository.id.clone());
        let mut missing_owner_lock =
            lock_decision(first_repository.id.clone(), first_policy.id.clone());
        missing_owner_lock.owner = LockOwner::Job(JobId::new());
        let mut cross_owner_lock =
            lock_decision(first_repository.id.clone(), first_policy.id.clone());
        cross_owner_lock.owner = LockOwner::Job(second_job.id.clone());

        store.create_repository(first_repository).unwrap();
        store.create_repository(second_repository).unwrap();
        store.create_policy_decision(first_policy).unwrap();

        assert!(matches!(
            store.create_lock_decision(missing_owner_lock),
            Err(StoreError::NotFound {
                collection: "jobs",
                ..
            })
        ));

        persist_job(&store, second_job);

        assert!(matches!(
            store.create_lock_decision(cross_owner_lock),
            Err(StoreError::InvalidReference {
                collection: "lock_decisions",
                ..
            })
        ));
    }

    #[test]
    fn lock_decisions_reject_invalid_timing_even_from_struct_literal() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let policy = policy_decision(repository.id.clone());
        let mut lock = lock_decision(repository.id.clone(), policy.id.clone());
        lock.expires_at = lock.locked_at;

        store.create_repository(repository).unwrap();
        store.create_policy_decision(policy).unwrap();

        assert!(matches!(
            store.create_lock_decision(lock),
            Err(StoreError::InvalidRecord {
                collection: "lock_decisions",
                ..
            })
        ));
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
        persist_job(&store, job);

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
    fn tool_invocations_reject_cross_repository_parents() {
        let store = InMemoryStore::new();
        let first_repository = repository_record();
        let second_repository = repository_record();
        let first_job = job_record(first_repository.id.clone());
        let second_job = job_record(second_repository.id.clone());
        let first_policy = policy_decision(first_repository.id.clone());
        let second_policy = policy_decision(second_repository.id.clone());

        store.create_repository(first_repository.clone()).unwrap();
        store.create_repository(second_repository.clone()).unwrap();
        persist_job(&store, first_job.clone());
        persist_job(&store, second_job.clone());
        store.create_policy_decision(first_policy.clone()).unwrap();
        store.create_policy_decision(second_policy.clone()).unwrap();

        assert!(matches!(
            store.create_tool_invocation(tool_invocation(
                first_repository.id.clone(),
                second_job.id,
                first_policy.id.clone()
            )),
            Err(StoreError::InvalidReference {
                collection: "tool_invocations",
                ..
            })
        ));
        assert!(matches!(
            store.create_tool_invocation(tool_invocation(
                first_repository.id,
                first_job.id,
                second_policy.id
            )),
            Err(StoreError::InvalidReference {
                collection: "tool_invocations",
                ..
            })
        ));
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

    #[test]
    fn tool_results_reject_mismatched_tool_id_and_duplicate_invocation() {
        let store = InMemoryStore::new();
        let repository = repository_record();
        let job = job_record(repository.id.clone());
        let policy = policy_decision(repository.id.clone());
        let invocation = tool_invocation(repository.id.clone(), job.id.clone(), policy.id.clone());
        let mut mismatched_result = tool_result(invocation.id.clone());
        mismatched_result.tool_id = "fs.write".to_string();

        store.create_repository(repository).unwrap();
        persist_job(&store, job);
        store.create_policy_decision(policy).unwrap();
        store.create_tool_invocation(invocation.clone()).unwrap();

        assert!(matches!(
            store.create_tool_result(mismatched_result),
            Err(StoreError::InvalidReference {
                collection: "tool_results",
                ..
            })
        ));

        store
            .create_tool_result(tool_result(invocation.id.clone()))
            .unwrap();

        assert!(matches!(
            store.create_tool_result(tool_result(invocation.id)),
            Err(StoreError::Conflict {
                collection: "tool_results",
                ..
            })
        ));
    }

    #[test]
    fn audit_records_reject_cross_repository_policy_and_invocation() {
        let store = InMemoryStore::new();
        let first_repository = repository_record();
        let second_repository = repository_record();
        let first_job = job_record(first_repository.id.clone());
        let second_job = job_record(second_repository.id.clone());
        let first_policy = policy_decision(first_repository.id.clone());
        let second_policy = policy_decision(second_repository.id.clone());
        let second_invocation = tool_invocation(
            second_repository.id.clone(),
            second_job.id.clone(),
            second_policy.id.clone(),
        );

        store.create_repository(first_repository.clone()).unwrap();
        store.create_repository(second_repository.clone()).unwrap();
        persist_job(&store, first_job);
        persist_job(&store, second_job);
        store.create_policy_decision(first_policy.clone()).unwrap();
        store.create_policy_decision(second_policy.clone()).unwrap();
        store
            .create_tool_invocation(second_invocation.clone())
            .unwrap();

        assert!(matches!(
            store.create_audit_record(audit_record(
                first_repository.id.clone(),
                second_policy.id,
                None
            )),
            Err(StoreError::InvalidReference {
                collection: "audit_records",
                ..
            })
        ));
        assert!(matches!(
            store.create_audit_record(audit_record(
                first_repository.id,
                first_policy.id,
                Some(second_invocation.id)
            )),
            Err(StoreError::InvalidReference {
                collection: "audit_records",
                ..
            })
        ));
    }
}
