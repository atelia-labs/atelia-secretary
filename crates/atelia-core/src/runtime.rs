//! Synchronous Secretary runtime loop for bounded tool jobs.

use crate::domain::{
    Actor, AuditRecord, AuditRecordId, CancellationState, EventRefs, EventSeverity, EventSubject,
    JobEvent, JobEventId, JobEventKind, JobId, JobKind, JobRecord, JobStatus,
    JobStatusTransitionError, LedgerTimestamp, PolicyDecision, PolicyOutcome, PolicySummary,
    RepositoryId, ResourceScope, StructuredValue, ToolInvocation, ToolInvocationId, ToolResult,
    ToolResultField, ToolResultId, ToolResultStatus,
};
use crate::policy::{DefaultPolicyEngine, PolicyEngine, PolicyInput, DEFAULT_POLICY_VERSION};
use crate::store::{EventCursor, InMemoryStore, JobPage, JobQuery, SecretaryStore, StoreError};
use crate::tool_output::{
    render_tool_result, OutputFormat, RenderOptions, RenderedToolOutput, ToolOutputRenderError,
};
use std::error::Error;
use std::fmt;

const RUNTIME_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeJobRequest {
    pub requester: Actor,
    pub repository_id: RepositoryId,
    pub kind: JobKind,
    pub goal: String,
    pub resource_scope: ResourceScope,
    pub approval_available: bool,
    pub render_options: RenderOptions,
}

impl RuntimeJobRequest {
    pub fn new(
        requester: Actor,
        repository_id: RepositoryId,
        kind: JobKind,
        goal: impl Into<String>,
    ) -> Self {
        Self {
            requester,
            repository_id,
            kind,
            goal: goal.into(),
            resource_scope: ResourceScope {
                kind: "repository".to_string(),
                value: ".".to_string(),
            },
            approval_available: true,
            render_options: RenderOptions::new(OutputFormat::Toon),
        }
    }

    pub fn with_resource_scope(
        mut self,
        kind: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.resource_scope = ResourceScope {
            kind: kind.into(),
            value: value.into(),
        };
        self
    }

    pub fn without_approval_path(mut self) -> Self {
        self.approval_available = false;
        self
    }

    pub fn with_render_options(mut self, render_options: RenderOptions) -> Self {
        self.render_options = render_options;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeJobReceipt {
    pub job: JobRecord,
    pub policy_decision: PolicyDecision,
    pub tool_invocation: Option<ToolInvocation>,
    pub tool_result: Option<ToolResult>,
    pub rendered_output: Option<RenderedToolOutput>,
    pub audit_record: Option<AuditRecord>,
    pub events: Vec<JobEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeError {
    Store(StoreError),
    JobStatusTransition(JobStatusTransitionError),
    ToolOutputRender(ToolOutputRenderError),
    InvalidToolResult { reason: String },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(f, "{error}"),
            Self::JobStatusTransition(error) => {
                write!(f, "job status transition failed: {error:?}")
            }
            Self::ToolOutputRender(error) => write!(f, "{error}"),
            Self::InvalidToolResult { reason } => write!(f, "invalid tool result: {reason}"),
        }
    }
}

impl Error for RuntimeError {}

impl From<StoreError> for RuntimeError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl From<JobStatusTransitionError> for RuntimeError {
    fn from(error: JobStatusTransitionError) -> Self {
        Self::JobStatusTransition(error)
    }
}

impl From<ToolOutputRenderError> for RuntimeError {
    fn from(error: ToolOutputRenderError) -> Self {
        Self::ToolOutputRender(error)
    }
}

pub type RuntimeResult<T> = Result<T, RuntimeError>;

pub trait RuntimeTool {
    fn tool_id(&self) -> &'static str;
    fn requested_capability(&self) -> &'static str;
    fn declared_effect(&self) -> &'static str;
    fn args_summary(&self, request: &RuntimeJobRequest) -> String;
    fn execute(&self, invocation: &ToolInvocation, request: &RuntimeJobRequest) -> ToolResult;
}

#[derive(Debug, Clone, Default)]
pub struct EchoTool;

impl RuntimeTool for EchoTool {
    fn tool_id(&self) -> &'static str {
        "secretary.echo"
    }

    fn requested_capability(&self) -> &'static str {
        "capability.discovery"
    }

    fn declared_effect(&self) -> &'static str {
        "produce a deterministic contract result without external effects"
    }

    fn args_summary(&self, request: &RuntimeJobRequest) -> String {
        format!("goal={}", request.goal)
    }

    fn execute(&self, invocation: &ToolInvocation, request: &RuntimeJobRequest) -> ToolResult {
        ToolResult {
            id: ToolResultId::new(),
            schema_version: RUNTIME_SCHEMA_VERSION,
            created_at: LedgerTimestamp::now(),
            invocation_id: invocation.id.clone(),
            tool_id: invocation.tool_id.clone(),
            status: ToolResultStatus::Succeeded,
            schema_ref: Some("tool_result.secretary.echo.v1".to_string()),
            fields: vec![
                ToolResultField {
                    key: "summary".to_string(),
                    value: StructuredValue::String(format!("echoed goal: {}", request.goal)),
                },
                ToolResultField {
                    key: "goal".to_string(),
                    value: StructuredValue::String(request.goal.clone()),
                },
                ToolResultField {
                    key: "policy.state".to_string(),
                    value: StructuredValue::String("recorded_before_execution".to_string()),
                },
            ],
            evidence_refs: Vec::new(),
            output_refs: Vec::new(),
            truncation: None,
            redactions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SecretaryRuntime<S = InMemoryStore, P = DefaultPolicyEngine> {
    store: S,
    policy_engine: P,
}

impl SecretaryRuntime<InMemoryStore, DefaultPolicyEngine> {
    pub fn in_memory() -> Self {
        Self::new(InMemoryStore::new(), DefaultPolicyEngine::new())
    }
}

impl<S, P> SecretaryRuntime<S, P>
where
    S: SecretaryStore,
    P: PolicyEngine,
{
    pub fn new(store: S, policy_engine: P) -> Self {
        Self {
            store,
            policy_engine,
        }
    }

    pub fn store(&self) -> &S {
        &self.store
    }

    pub fn run_tool_job<T>(
        &self,
        request: RuntimeJobRequest,
        tool: &T,
    ) -> RuntimeResult<RuntimeJobReceipt>
    where
        T: RuntimeTool,
    {
        let repository = self.store.get_repository(&request.repository_id)?;
        let mut events = Vec::new();
        let mut job = JobRecord::new(
            request.requester.clone(),
            request.repository_id.clone(),
            request.kind.clone(),
            request.goal.clone(),
            LedgerTimestamp::now(),
        );

        let initial_event = job_event(
            EventSubject::job(&job.id),
            JobEventKind::JobSubmitted,
            EventSeverity::Info,
            "job submitted",
            refs_for_job(&job),
        );
        let initial_event = self
            .store
            .create_job_with_initial_event(job.clone(), initial_event)?;
        job.latest_event_id = Some(initial_event.id.clone());
        events.push(initial_event);

        let policy_decision = self.policy_engine.evaluate(
            PolicyInput::new(
                request.requester.clone(),
                request.repository_id.clone(),
                tool.requested_capability(),
                request.resource_scope.clone(),
                tool.declared_effect(),
                repository.trust_state,
                request.approval_available,
                DEFAULT_POLICY_VERSION,
            )
            .with_tool_id(tool.tool_id()),
        );
        self.store.create_policy_decision(policy_decision.clone())?;
        job.policy_summary = Some(policy_summary(&policy_decision));
        let policy_event = job_event(
            EventSubject::policy_decision(&policy_decision.id),
            JobEventKind::PolicyDecided {
                outcome: policy_decision.outcome,
            },
            policy_event_severity(policy_decision.outcome),
            policy_decision.user_reason.clone(),
            refs_for_policy(&job, &policy_decision),
        );
        events.push(
            self.store
                .append_job_event_and_update_job(job.clone(), policy_event)?,
        );
        job.latest_event_id = events.last().map(|event| event.id.clone());

        if policy_decision.outcome == PolicyOutcome::Blocked
            || policy_decision.outcome == PolicyOutcome::NeedsApproval
        {
            let terminal_status = JobStatus::Blocked;
            let previous = job.status;
            job.transition_status(terminal_status, LedgerTimestamp::now())?;
            let status_event = job_event(
                EventSubject::job(&job.id),
                JobEventKind::JobStatusChanged {
                    from: previous,
                    to: terminal_status,
                },
                EventSeverity::Warning,
                "job stopped before tool execution",
                refs_for_policy(&job, &policy_decision),
            );
            events.push(
                self.store
                    .append_job_event_and_update_job(job.clone(), status_event)?,
            );
            job.latest_event_id = events.last().map(|event| event.id.clone());
            return Ok(RuntimeJobReceipt {
                job,
                policy_decision,
                tool_invocation: None,
                tool_result: None,
                rendered_output: None,
                audit_record: None,
                events,
            });
        }

        let previous = job.status;
        job.transition_status(JobStatus::Running, LedgerTimestamp::now())?;
        let running_event = job_event(
            EventSubject::job(&job.id),
            JobEventKind::JobStatusChanged {
                from: previous,
                to: JobStatus::Running,
            },
            EventSeverity::Info,
            "job running",
            refs_for_policy(&job, &policy_decision),
        );
        events.push(
            self.store
                .append_job_event_and_update_job(job.clone(), running_event)?,
        );
        job.latest_event_id = events.last().map(|event| event.id.clone());

        let invocation = ToolInvocation {
            id: ToolInvocationId::new(),
            schema_version: RUNTIME_SCHEMA_VERSION,
            created_at: LedgerTimestamp::now(),
            job_id: job.id.clone(),
            repository_id: request.repository_id.clone(),
            policy_decision_id: policy_decision.id.clone(),
            actor: request.requester.clone(),
            tool_id: tool.tool_id().to_string(),
            requested_capability: tool.requested_capability().to_string(),
            args_summary: tool.args_summary(&request),
            resolved_paths: Vec::new(),
            timeout_millis: None,
            redactions: Vec::new(),
        };
        self.store.create_tool_invocation(invocation.clone())?;
        let invocation_event = job_event(
            EventSubject::tool_invocation(&invocation.id),
            JobEventKind::ToolInvoked {
                tool_id: invocation.tool_id.clone(),
            },
            EventSeverity::Info,
            "tool invoked",
            refs_for_invocation(&job, &policy_decision, &invocation),
        );
        events.push(
            self.store
                .append_job_event_and_update_job(job.clone(), invocation_event)?,
        );
        job.latest_event_id = events.last().map(|event| event.id.clone());

        let result = tool.execute(&invocation, &request);
        if let Err(error) = validate_runtime_tool_result(&invocation, &result) {
            let previous = job.status;
            job.transition_status(JobStatus::Failed, LedgerTimestamp::now())?;
            let failure_event = job_event(
                EventSubject::job(&job.id),
                JobEventKind::JobStatusChanged {
                    from: previous,
                    to: JobStatus::Failed,
                },
                EventSeverity::Error,
                "tool result rejected",
                refs_for_invocation(&job, &policy_decision, &invocation),
            );
            self.store
                .append_job_event_and_update_job(job.clone(), failure_event)?;
            return Err(error);
        }
        let result_event = job_event(
            EventSubject::tool_result(&result.id),
            JobEventKind::ToolResultRecorded {
                status: result.status,
            },
            tool_result_event_severity(result.status),
            "tool result recorded",
            refs_for_result(&job, &policy_decision, &invocation, &result),
        );

        let audit_record = AuditRecord {
            id: AuditRecordId::new(),
            schema_version: RUNTIME_SCHEMA_VERSION,
            created_at: LedgerTimestamp::now(),
            actor: request.requester,
            repository_id: request.repository_id,
            requested_capability: tool.requested_capability().to_string(),
            policy_decision_id: policy_decision.id.clone(),
            tool_invocation_id: Some(invocation.id.clone()),
            effect_summary: format!("{} completed with {:?}", invocation.tool_id, result.status),
            output_refs: result.output_refs.clone(),
            redactions: result.redactions.clone(),
        };
        let audit_event = job_event(
            EventSubject::audit_record(&audit_record.id),
            JobEventKind::AuditRecorded,
            EventSeverity::Info,
            "audit record created",
            refs_for_audit(&job, &policy_decision, &invocation, &result, &audit_record),
        );

        let (result_event, audit_event) = self.store.record_tool_result_and_audit_with_events(
            job.clone(),
            result.clone(),
            result_event,
            audit_record.clone(),
            audit_event,
        )?;
        job.latest_event_id = Some(audit_event.id.clone());
        events.push(result_event);
        events.push(audit_event);

        let terminal_status = match result.status {
            ToolResultStatus::Succeeded => JobStatus::Succeeded,
            ToolResultStatus::Failed | ToolResultStatus::TimedOut => JobStatus::Failed,
            ToolResultStatus::Canceled => JobStatus::Canceled,
        };
        let previous = job.status;
        job.transition_status(terminal_status, LedgerTimestamp::now())?;
        let terminal_event = job_event(
            EventSubject::job(&job.id),
            JobEventKind::JobStatusChanged {
                from: previous,
                to: terminal_status,
            },
            EventSeverity::Info,
            "job completed",
            refs_for_audit(&job, &policy_decision, &invocation, &result, &audit_record),
        );
        events.push(
            self.store
                .append_job_event_and_update_job(job.clone(), terminal_event)?,
        );
        job.latest_event_id = events.last().map(|event| event.id.clone());

        let rendered_output = render_tool_result(&result, &request.render_options)?;

        Ok(RuntimeJobReceipt {
            job,
            policy_decision,
            tool_invocation: Some(invocation),
            tool_result: Some(result),
            rendered_output: Some(rendered_output),
            audit_record: Some(audit_record),
            events,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelJobReceipt {
    pub job: JobRecord,
    pub events: Vec<JobEvent>,
}

#[derive(Debug, Clone)]
pub struct JobLifecycleService<S = InMemoryStore, P = DefaultPolicyEngine> {
    runtime: SecretaryRuntime<S, P>,
}

impl JobLifecycleService<InMemoryStore, DefaultPolicyEngine> {
    pub fn in_memory() -> Self {
        Self {
            runtime: SecretaryRuntime::in_memory(),
        }
    }
}

impl<S, P> JobLifecycleService<S, P>
where
    S: SecretaryStore,
    P: PolicyEngine,
{
    pub fn new(runtime: SecretaryRuntime<S, P>) -> Self {
        Self { runtime }
    }

    pub fn runtime(&self) -> &SecretaryRuntime<S, P> {
        &self.runtime
    }

    pub fn submit_echo_job(&self, request: RuntimeJobRequest) -> RuntimeResult<RuntimeJobReceipt> {
        self.runtime.run_tool_job(request, &EchoTool)
    }

    pub fn get_job(&self, id: &JobId) -> RuntimeResult<JobRecord> {
        Ok(self.runtime.store().get_job(id)?)
    }

    pub fn list_jobs(&self) -> RuntimeResult<Vec<JobRecord>> {
        Ok(self.runtime.store().list_jobs()?)
    }

    pub fn query_jobs(&self, query: JobQuery) -> RuntimeResult<JobPage> {
        Ok(self.runtime.store().query_jobs(query)?)
    }

    pub fn cancel_job(
        &self,
        id: &JobId,
        reason: impl Into<String>,
    ) -> RuntimeResult<CancelJobReceipt> {
        let mut job = self.runtime.store().get_job(id)?;
        let previous_status = job.status;

        if !matches!(previous_status, JobStatus::Queued | JobStatus::Running) {
            return Err(RuntimeError::Store(StoreError::Conflict {
                collection: "jobs",
                reason: format!(
                    "job {} cannot be cancelled in state {:?}",
                    id.as_str(),
                    previous_status
                ),
            }));
        }

        job.cancellation_state = CancellationState::Requested;
        let cancel_event = job_event(
            EventSubject::job(&job.id),
            JobEventKind::CancellationRequested,
            EventSeverity::Warning,
            reason,
            refs_for_job(&job),
        );
        let cancel_event = self
            .runtime
            .store()
            .append_job_event_and_update_job(job.clone(), cancel_event)?;
        job.latest_event_id = Some(cancel_event.id.clone());

        let now = LedgerTimestamp::now();
        job.transition_status(JobStatus::Canceled, now)?;
        let status_event = job_event(
            EventSubject::job(&job.id),
            JobEventKind::JobStatusChanged {
                from: previous_status,
                to: JobStatus::Canceled,
            },
            EventSeverity::Warning,
            "job cancelled",
            refs_for_job(&job),
        );
        let status_event = self
            .runtime
            .store()
            .append_job_event_and_update_job(job.clone(), status_event)?;
        job.latest_event_id = Some(status_event.id.clone());

        Ok(CancelJobReceipt {
            job,
            events: vec![cancel_event, status_event],
        })
    }

    pub fn replay_events(
        &self,
        cursor: EventCursor,
        limit: Option<usize>,
    ) -> RuntimeResult<Vec<JobEvent>> {
        Ok(self.runtime.store().replay_job_events(cursor, limit)?)
    }
}

fn policy_summary(policy_decision: &PolicyDecision) -> PolicySummary {
    PolicySummary {
        decision_id: Some(policy_decision.id.clone()),
        outcome: policy_decision.outcome,
        risk_tier: policy_decision.risk_tier,
        reason_code: policy_decision.reason_code.clone(),
    }
}

fn validate_runtime_tool_result(
    invocation: &ToolInvocation,
    result: &ToolResult,
) -> RuntimeResult<()> {
    if result.invocation_id != invocation.id {
        return Err(RuntimeError::InvalidToolResult {
            reason: format!(
                "tool result invocation_id {} does not match active invocation {}",
                result.invocation_id.as_str(),
                invocation.id.as_str()
            ),
        });
    }
    if result.tool_id != invocation.tool_id {
        return Err(RuntimeError::InvalidToolResult {
            reason: format!(
                "tool result tool_id {} does not match active invocation tool_id {}",
                result.tool_id, invocation.tool_id
            ),
        });
    }

    Ok(())
}

fn job_event(
    subject: EventSubject,
    kind: JobEventKind,
    severity: EventSeverity,
    public_message: impl Into<String>,
    refs: EventRefs,
) -> JobEvent {
    JobEvent {
        id: JobEventId::new(),
        schema_version: RUNTIME_SCHEMA_VERSION,
        sequence_number: 0,
        created_at: LedgerTimestamp::now(),
        subject,
        kind,
        severity,
        public_message: public_message.into(),
        refs,
        redactions: Vec::new(),
    }
}

fn refs_for_job(job: &JobRecord) -> EventRefs {
    EventRefs {
        repository_id: Some(job.repository_id.clone()),
        job_id: Some(job.id.clone()),
        ..EventRefs::default()
    }
}

fn refs_for_policy(job: &JobRecord, policy_decision: &PolicyDecision) -> EventRefs {
    EventRefs {
        policy_decision_id: Some(policy_decision.id.clone()),
        ..refs_for_job(job)
    }
}

fn refs_for_invocation(
    job: &JobRecord,
    policy_decision: &PolicyDecision,
    invocation: &ToolInvocation,
) -> EventRefs {
    EventRefs {
        tool_invocation_id: Some(invocation.id.clone()),
        ..refs_for_policy(job, policy_decision)
    }
}

fn refs_for_result(
    job: &JobRecord,
    policy_decision: &PolicyDecision,
    invocation: &ToolInvocation,
    result: &ToolResult,
) -> EventRefs {
    EventRefs {
        tool_result_id: Some(result.id.clone()),
        ..refs_for_invocation(job, policy_decision, invocation)
    }
}

fn refs_for_audit(
    job: &JobRecord,
    policy_decision: &PolicyDecision,
    invocation: &ToolInvocation,
    result: &ToolResult,
    audit_record: &AuditRecord,
) -> EventRefs {
    EventRefs {
        audit_record_id: Some(audit_record.id.clone()),
        ..refs_for_result(job, policy_decision, invocation, result)
    }
}

fn policy_event_severity(outcome: PolicyOutcome) -> EventSeverity {
    match outcome {
        PolicyOutcome::Allowed | PolicyOutcome::Audited => EventSeverity::Info,
        PolicyOutcome::NeedsApproval => EventSeverity::Warning,
        PolicyOutcome::Blocked => EventSeverity::Error,
    }
}

fn tool_result_event_severity(status: ToolResultStatus) -> EventSeverity {
    match status {
        ToolResultStatus::Succeeded => EventSeverity::Info,
        ToolResultStatus::Failed | ToolResultStatus::TimedOut | ToolResultStatus::Canceled => {
            EventSeverity::Error
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{RepositoryRecord, RepositoryTrustState};
    use crate::store::EventCursor;

    fn actor() -> Actor {
        Actor::Agent {
            id: "agent:test".to_string(),
            display_name: Some("Test Agent".to_string()),
        }
    }

    fn repository() -> RepositoryRecord {
        RepositoryRecord::new(
            "atelia-secretary",
            "/workspace/atelia-secretary",
            RepositoryTrustState::Trusted,
            LedgerTimestamp::from_unix_millis(1_700_000_000_000),
        )
    }

    #[derive(Debug, Clone, Default)]
    struct ApprovalTool;

    impl RuntimeTool for ApprovalTool {
        fn tool_id(&self) -> &'static str {
            "secretary.approval_fixture"
        }

        fn requested_capability(&self) -> &'static str {
            "external.network"
        }

        fn declared_effect(&self) -> &'static str {
            "would call an external service"
        }

        fn args_summary(&self, request: &RuntimeJobRequest) -> String {
            format!("goal={}", request.goal)
        }

        fn execute(&self, invocation: &ToolInvocation, _request: &RuntimeJobRequest) -> ToolResult {
            ToolResult {
                id: ToolResultId::new(),
                schema_version: RUNTIME_SCHEMA_VERSION,
                created_at: LedgerTimestamp::now(),
                invocation_id: invocation.id.clone(),
                tool_id: invocation.tool_id.clone(),
                status: ToolResultStatus::Succeeded,
                schema_ref: None,
                fields: Vec::new(),
                evidence_refs: Vec::new(),
                output_refs: Vec::new(),
                truncation: None,
                redactions: Vec::new(),
            }
        }
    }

    #[derive(Debug, Clone, Default)]
    struct WrongInvocationTool;

    impl RuntimeTool for WrongInvocationTool {
        fn tool_id(&self) -> &'static str {
            "secretary.bad_fixture"
        }

        fn requested_capability(&self) -> &'static str {
            "capability.discovery"
        }

        fn declared_effect(&self) -> &'static str {
            "return a malformed result"
        }

        fn args_summary(&self, request: &RuntimeJobRequest) -> String {
            format!("goal={}", request.goal)
        }

        fn execute(&self, invocation: &ToolInvocation, _request: &RuntimeJobRequest) -> ToolResult {
            ToolResult {
                id: ToolResultId::new(),
                schema_version: RUNTIME_SCHEMA_VERSION,
                created_at: LedgerTimestamp::now(),
                invocation_id: ToolInvocationId::new(),
                tool_id: invocation.tool_id.clone(),
                status: ToolResultStatus::Succeeded,
                schema_ref: None,
                fields: Vec::new(),
                evidence_refs: Vec::new(),
                output_refs: Vec::new(),
                truncation: None,
                redactions: Vec::new(),
            }
        }
    }

    #[test]
    fn echo_tool_job_records_policy_execution_result_audit_and_rendered_output() {
        let runtime = SecretaryRuntime::in_memory();
        let repository = repository();
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();

        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "prove the runtime loop",
        );

        let receipt = runtime.run_tool_job(request, &EchoTool).unwrap();

        assert_eq!(JobStatus::Succeeded, receipt.job.status);
        assert!(receipt.job.started_at.is_some());
        assert!(receipt.job.completed_at.is_some());
        assert_eq!(PolicyOutcome::Allowed, receipt.policy_decision.outcome);
        assert!(receipt.tool_invocation.is_some());
        assert!(receipt.tool_result.is_some());
        assert!(receipt.audit_record.is_some());
        assert!(receipt
            .rendered_output
            .as_ref()
            .unwrap()
            .body
            .contains("echoed goal: prove the runtime loop"));
        assert_eq!(7, receipt.events.len());

        let stored_job = runtime.store().get_job(&receipt.job.id).unwrap();
        assert_eq!(JobStatus::Succeeded, stored_job.status);
        assert_eq!(receipt.job.latest_event_id, stored_job.latest_event_id);
        let replayed = runtime
            .store()
            .replay_job_events(EventCursor::Beginning, None)
            .unwrap();
        assert_eq!(receipt.events, replayed);
    }

    #[test]
    fn blocked_policy_stops_before_tool_invocation() {
        let runtime = SecretaryRuntime::in_memory();
        let mut repository = repository();
        repository.trust_state = RepositoryTrustState::Blocked;
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();

        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "should not execute",
        );

        let receipt = runtime.run_tool_job(request, &EchoTool).unwrap();

        assert_eq!(JobStatus::Blocked, receipt.job.status);
        assert_eq!(PolicyOutcome::Blocked, receipt.policy_decision.outcome);
        assert!(receipt.tool_invocation.is_none());
        assert!(receipt.tool_result.is_none());
        assert!(receipt.rendered_output.is_none());
        assert!(runtime.store().list_tool_invocations().unwrap().is_empty());
        assert_eq!(3, receipt.events.len());
    }

    #[test]
    fn needs_approval_policy_stops_before_tool_invocation() {
        let runtime = SecretaryRuntime::in_memory();
        let repository = repository();
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();

        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "external lookup",
        );

        let receipt = runtime.run_tool_job(request, &ApprovalTool).unwrap();

        assert_eq!(JobStatus::Blocked, receipt.job.status);
        assert_eq!(
            PolicyOutcome::NeedsApproval,
            receipt.policy_decision.outcome
        );
        assert!(receipt.tool_invocation.is_none());
        assert!(receipt.tool_result.is_none());
        assert!(runtime.store().list_tool_invocations().unwrap().is_empty());
        assert_eq!(3, receipt.events.len());
    }

    #[test]
    fn runtime_rejects_tool_result_for_wrong_invocation() {
        let runtime = SecretaryRuntime::in_memory();
        let repository = repository();
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();

        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "malformed result",
        );

        let error = runtime
            .run_tool_job(request, &WrongInvocationTool)
            .unwrap_err();

        assert!(matches!(error, RuntimeError::InvalidToolResult { .. }));
        let jobs = runtime.store().list_jobs().unwrap();
        assert_eq!(1, jobs.len());
        assert_eq!(JobStatus::Failed, jobs[0].status);
        assert!(jobs[0].completed_at.is_some());
        assert!(runtime.store().list_tool_results().unwrap().is_empty());
    }

    fn service_with_repo() -> (JobLifecycleService, RepositoryRecord) {
        let service = JobLifecycleService::in_memory();
        let repo = repository();
        service
            .runtime()
            .store()
            .create_repository(repo.clone())
            .unwrap();
        (service, repo)
    }

    fn seed_job_in_status(
        store: &dyn SecretaryStore,
        repository_id: RepositoryId,
        target_status: JobStatus,
    ) -> JobRecord {
        let created_at = LedgerTimestamp::from_unix_millis(1_700_000_001_000);
        let mut job = JobRecord::new(
            actor(),
            repository_id,
            JobKind::Read,
            "seeded job",
            created_at,
        );

        let submit_event = job_event(
            EventSubject::job(&job.id),
            JobEventKind::JobSubmitted,
            EventSeverity::Info,
            "job submitted",
            refs_for_job(&job),
        );
        let event = store
            .create_job_with_initial_event(job.clone(), submit_event)
            .unwrap();
        job.latest_event_id = Some(event.id);

        if target_status != JobStatus::Queued {
            let at = LedgerTimestamp::from_unix_millis(created_at.unix_millis + 1000);
            let from = job.status;
            job.transition_status(target_status, at).unwrap();
            let status_event = job_event(
                EventSubject::job(&job.id),
                JobEventKind::JobStatusChanged {
                    from,
                    to: target_status,
                },
                EventSeverity::Info,
                format!("job transitioned to {:?}", target_status),
                refs_for_job(&job),
            );
            let event = store
                .append_job_event_and_update_job(job.clone(), status_event)
                .unwrap();
            job.latest_event_id = Some(event.id);
        }

        job
    }

    #[test]
    fn lifecycle_submit_echo_traverses_queued_running_succeeded() {
        let (service, repo) = service_with_repo();

        let request = RuntimeJobRequest::new(
            actor(),
            repo.id.clone(),
            JobKind::Read,
            "lifecycle echo test",
        );
        let receipt = service.submit_echo_job(request).unwrap();

        assert_eq!(JobStatus::Succeeded, receipt.job.status);
        assert!(receipt.job.started_at.is_some());
        assert!(receipt.job.completed_at.is_some());
        assert!(receipt.tool_result.is_some());

        let stored = service.get_job(&receipt.job.id).unwrap();
        assert_eq!(JobStatus::Succeeded, stored.status);

        let all = service.list_jobs().unwrap();
        assert_eq!(1, all.len());

        let page = service
            .query_jobs(JobQuery {
                status: Some(JobStatus::Succeeded),
                ..JobQuery::default()
            })
            .unwrap();
        assert_eq!(1, page.jobs.len());
        assert!(page.next_page_token.is_none());
    }

    #[test]
    fn lifecycle_blocked_repo_stops_before_tool_execution() {
        let service = JobLifecycleService::in_memory();
        let mut repo = repository();
        repo.trust_state = RepositoryTrustState::Blocked;
        service
            .runtime()
            .store()
            .create_repository(repo.clone())
            .unwrap();

        let request =
            RuntimeJobRequest::new(actor(), repo.id.clone(), JobKind::Read, "blocked test");
        let receipt = service.submit_echo_job(request).unwrap();

        assert_eq!(JobStatus::Blocked, receipt.job.status);
        assert!(receipt.tool_invocation.is_none());
        assert!(receipt.tool_result.is_none());

        let stored = service.get_job(&receipt.job.id).unwrap();
        assert_eq!(JobStatus::Blocked, stored.status);
    }

    #[test]
    fn lifecycle_cancel_queued_job_emits_ledger_events() {
        let (service, repo) = service_with_repo();
        let job = seed_job_in_status(
            service.runtime().store(),
            repo.id.clone(),
            JobStatus::Queued,
        );

        let receipt = service
            .cancel_job(&job.id, "user requested cancel")
            .unwrap();

        assert_eq!(JobStatus::Canceled, receipt.job.status);
        assert_eq!(CancellationState::Requested, receipt.job.cancellation_state);
        assert!(receipt.job.completed_at.is_some());
        assert_eq!(2, receipt.events.len());
        assert!(matches!(
            receipt.events[0].kind,
            JobEventKind::CancellationRequested
        ));
        assert!(matches!(
            receipt.events[1].kind,
            JobEventKind::JobStatusChanged {
                from: JobStatus::Queued,
                to: JobStatus::Canceled
            }
        ));

        let stored = service.get_job(&job.id).unwrap();
        assert_eq!(JobStatus::Canceled, stored.status);
        assert_eq!(CancellationState::Requested, stored.cancellation_state);
    }

    #[test]
    fn lifecycle_cancel_running_job_emits_ledger_events() {
        let (service, repo) = service_with_repo();
        let job = seed_job_in_status(
            service.runtime().store(),
            repo.id.clone(),
            JobStatus::Running,
        );

        let receipt = service.cancel_job(&job.id, "timeout exceeded").unwrap();

        assert_eq!(JobStatus::Canceled, receipt.job.status);
        assert_eq!(CancellationState::Requested, receipt.job.cancellation_state);
        assert!(receipt.job.completed_at.is_some());
        assert_eq!(2, receipt.events.len());
        assert!(matches!(
            receipt.events[1].kind,
            JobEventKind::JobStatusChanged {
                from: JobStatus::Running,
                to: JobStatus::Canceled
            }
        ));

        let stored = service.get_job(&job.id).unwrap();
        assert_eq!(JobStatus::Canceled, stored.status);
    }

    #[test]
    fn lifecycle_cancel_rejects_terminal_job() {
        let (service, repo) = service_with_repo();
        let request =
            RuntimeJobRequest::new(actor(), repo.id.clone(), JobKind::Read, "already done");
        let done = service.submit_echo_job(request).unwrap();
        assert_eq!(JobStatus::Succeeded, done.job.status);

        let error = service.cancel_job(&done.job.id, "too late").unwrap_err();
        assert!(matches!(
            error,
            RuntimeError::Store(StoreError::Conflict { .. })
        ));
    }

    #[test]
    fn lifecycle_replay_events_from_cursors() {
        let (service, repo) = service_with_repo();

        let r1 = service
            .submit_echo_job(RuntimeJobRequest::new(
                actor(),
                repo.id.clone(),
                JobKind::Read,
                "first job",
            ))
            .unwrap();
        let _r2 = service
            .submit_echo_job(RuntimeJobRequest::new(
                actor(),
                repo.id.clone(),
                JobKind::Read,
                "second job",
            ))
            .unwrap();

        let all = service.replay_events(EventCursor::Beginning, None).unwrap();
        assert_eq!(14, all.len());

        let mid_event = &r1.events[3];
        let after = service
            .replay_events(EventCursor::AfterEventId(mid_event.id.clone()), None)
            .unwrap();
        assert_eq!(10, after.len());

        let limited = service
            .replay_events(EventCursor::Beginning, Some(5))
            .unwrap();
        assert_eq!(5, limited.len());

        let after_seq = service
            .replay_events(EventCursor::AfterSequence(mid_event.sequence_number), None)
            .unwrap();
        assert_eq!(after.len(), after_seq.len());
    }
}
