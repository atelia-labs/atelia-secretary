//! Synchronous Secretary runtime loop for bounded tool jobs.

use crate::artifacts::{
    spill_large_tool_result_fields, ArtifactError, ArtifactStoreConfig, LocalArtifactStore,
    ToolResultSpilloverOptions,
};
use crate::domain::{
    Actor, AuditRecord, AuditRecordId, CancellationState, EventRefs, EventSeverity, EventSubject,
    JobEvent, JobEventId, JobEventKind, JobId, JobKind, JobRecord, JobStatus,
    JobStatusTransitionError, LedgerTimestamp, PolicyDecision, PolicyOutcome, PolicySummary,
    RepositoryId, ResolvedPath, ResourceScope, StructuredValue, ToolInvocation, ToolInvocationId,
    ToolResult, ToolResultField, ToolResultId, ToolResultStatus, TruncationMetadata,
};
use crate::policy::{
    canonicalize_job_requested_capability, DefaultPolicyEngine, PolicyEngine, PolicyInput,
    DEFAULT_POLICY_VERSION,
};
use crate::settings::{OversizeOutputPolicy, ToolOutputDefaults};
use crate::store::{EventCursor, InMemoryStore, JobPage, JobQuery, SecretaryStore, StoreError};
use crate::tool_output::{
    render_tool_result_with_policy, RenderOptions, RenderedToolOutput, ToolOutputRenderError,
};
use std::error::Error;
use std::fmt;

pub const RUNTIME_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeJobRequest {
    pub requester: Actor,
    pub repository_id: RepositoryId,
    pub kind: JobKind,
    pub goal: String,
    pub resource_scope: ResourceScope,
    pub requested_capabilities: Vec<String>,
    pub approval_available: bool,
    pub render_options: Option<RenderOptions>,
    pub tool_output_defaults: ToolOutputDefaults,
    pub artifact_spillover: Option<RuntimeArtifactSpillover>,
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
            requested_capabilities: Vec::new(),
            approval_available: true,
            render_options: None,
            tool_output_defaults: ToolOutputDefaults::default(),
            artifact_spillover: None,
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

    pub fn with_requested_capabilities(mut self, requested_capabilities: Vec<String>) -> Self {
        self.requested_capabilities = requested_capabilities;
        self
    }

    pub fn without_approval_path(mut self) -> Self {
        self.approval_available = false;
        self
    }

    pub fn with_render_options(mut self, render_options: RenderOptions) -> Self {
        self.render_options = Some(render_options);
        self
    }

    pub fn with_tool_output_defaults(mut self, defaults: ToolOutputDefaults) -> Self {
        self.tool_output_defaults = defaults;
        self
    }

    pub fn with_artifact_spillover(mut self, spillover: RuntimeArtifactSpillover) -> Self {
        self.artifact_spillover = Some(spillover);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeArtifactSpillover {
    pub store_config: ArtifactStoreConfig,
    pub options: ToolResultSpilloverOptions,
}

impl RuntimeArtifactSpillover {
    pub fn new(store_config: ArtifactStoreConfig, options: ToolResultSpilloverOptions) -> Self {
        Self {
            store_config,
            options,
        }
    }

    pub fn local_default(max_inline_bytes: usize) -> Self {
        Self {
            store_config: ArtifactStoreConfig::default_local(),
            options: ToolResultSpilloverOptions::new(max_inline_bytes),
        }
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

#[derive(Debug)]
pub enum RuntimeError {
    Store(StoreError),
    JobStatusTransition(JobStatusTransitionError),
    ToolOutputRender(ToolOutputRenderError),
    Artifact(ArtifactError),
    InvalidToolRequest { reason: String },
    InvalidToolResult { reason: String },
    ToolOutputTooLarge { reason: String },
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Store(error) => write!(f, "{error}"),
            Self::JobStatusTransition(error) => {
                write!(f, "job status transition failed: {error:?}")
            }
            Self::ToolOutputRender(error) => write!(f, "{error}"),
            Self::Artifact(error) => write!(f, "{error}"),
            Self::InvalidToolRequest { reason } => write!(f, "invalid tool request: {reason}"),
            Self::InvalidToolResult { reason } => write!(f, "invalid tool result: {reason}"),
            Self::ToolOutputTooLarge { reason } => write!(f, "{reason}"),
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

impl From<ArtifactError> for RuntimeError {
    fn from(error: ArtifactError) -> Self {
        Self::Artifact(error)
    }
}

pub type RuntimeResult<T> = Result<T, RuntimeError>;

pub trait RuntimeTool {
    fn tool_id(&self) -> &'static str;
    fn requested_capability(&self) -> &'static str;
    fn declared_effect(&self) -> &'static str;
    fn args_summary(&self, request: &RuntimeJobRequest) -> String;
    fn resolved_paths(&self, _request: &RuntimeJobRequest) -> Vec<ResolvedPath> {
        Vec::new()
    }
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
        let requested_capability = tool.requested_capability().to_string();
        let canonical_requested_capability = normalized_requested_capability(&requested_capability);
        if canonical_requested_capability.is_empty() {
            return Err(RuntimeError::InvalidToolRequest {
                reason: "requested_capability must not be empty".to_string(),
            });
        }
        validate_requested_capability_hints(
            &request.requested_capabilities,
            canonical_requested_capability,
        )?;
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
                canonical_requested_capability,
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
            requested_capability: canonical_requested_capability.to_string(),
            args_summary: tool.args_summary(&request),
            resolved_paths: tool.resolved_paths(&request),
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

        let mut result = tool.execute(&invocation, &request);
        if let Err(error) = validate_runtime_tool_result(&invocation, &result) {
            let failure_refs = refs_for_invocation(&job, &policy_decision, &invocation);
            append_failed_job_event(
                &self.store,
                &mut job,
                &mut events,
                "tool result rejected",
                failure_refs,
            )?;
            return Err(error);
        }

        let render_policy = request
            .tool_output_defaults
            .render_policy_with_render_options(request.render_options.as_ref());

        if matches!(
            request.tool_output_defaults.oversize_policy,
            OversizeOutputPolicy::RejectOversize
        ) {
            if let Some(reason) =
                reject_oversized_tool_result(&result, &request.tool_output_defaults)
            {
                let failure_refs = refs_for_invocation(&job, &policy_decision, &invocation);
                append_failed_job_event(
                    &self.store,
                    &mut job,
                    &mut events,
                    "tool result exceeded output size limits",
                    failure_refs,
                )?;
                return Err(RuntimeError::ToolOutputTooLarge { reason });
            }
        }

        let rendered_output = match render_tool_result_with_policy(&result, &render_policy) {
            Ok(rendered_output) => rendered_output,
            Err(error) => {
                let failure_refs = refs_for_invocation(&job, &policy_decision, &invocation);
                append_failed_job_event(
                    &self.store,
                    &mut job,
                    &mut events,
                    "tool output rendering failed",
                    failure_refs,
                )?;
                return Err(RuntimeError::ToolOutputRender(error));
            }
        };

        let mut should_rerender_output = false;
        let max_inline_bytes =
            max_inline_bytes_as_usize(request.tool_output_defaults.max_inline_bytes);

        if let Some(spillover) = &request.artifact_spillover {
            let artifact_store = LocalArtifactStore::new(spillover.store_config.clone());
            let report = spill_large_tool_result_fields(
                &mut result,
                &artifact_store,
                repository.id.as_str(),
                &spillover.options,
            );
            match report {
                Ok(Some(_)) => {
                    should_rerender_output = true;
                }
                Ok(None) => {}
                Err(error) => {
                    let failure_refs = refs_for_invocation(&job, &policy_decision, &invocation);
                    append_failed_job_event(
                        &self.store,
                        &mut job,
                        &mut events,
                        "artifact spillover failed",
                        failure_refs,
                    )?;
                    return Err(error.into());
                }
            }
        } else {
            match request.tool_output_defaults.oversize_policy {
                OversizeOutputPolicy::SpillToArtifactRef => {
                    let spillover = RuntimeArtifactSpillover::local_default(max_inline_bytes);
                    let artifact_store = LocalArtifactStore::new(spillover.store_config.clone());
                    let report = spill_large_tool_result_fields(
                        &mut result,
                        &artifact_store,
                        repository.id.as_str(),
                        &spillover.options,
                    );
                    match report {
                        Ok(Some(_)) => {
                            should_rerender_output = true;
                        }
                        Ok(None) => {}
                        Err(error) => {
                            let failure_refs =
                                refs_for_invocation(&job, &policy_decision, &invocation);
                            append_failed_job_event(
                                &self.store,
                                &mut job,
                                &mut events,
                                "artifact spillover failed",
                                failure_refs,
                            )?;
                            return Err(error.into());
                        }
                    }
                }
                OversizeOutputPolicy::RejectOversize => {
                    if let Some(reason) =
                        reject_oversized_tool_result(&result, &request.tool_output_defaults)
                    {
                        let failure_refs = refs_for_invocation(&job, &policy_decision, &invocation);
                        append_failed_job_event(
                            &self.store,
                            &mut job,
                            &mut events,
                            "tool result exceeded output size limits",
                            failure_refs,
                        )?;
                        return Err(RuntimeError::ToolOutputTooLarge { reason });
                    }
                }
                OversizeOutputPolicy::TruncateWithMetadata => {
                    if let Some(report) =
                        truncate_oversized_tool_result_fields(&mut result, max_inline_bytes)
                    {
                        should_rerender_output = true;
                        result.truncation = Some(merge_runtime_truncation(
                            result.truncation.take(),
                            report.original_bytes,
                            report.retained_bytes,
                            "runtime truncate_with_metadata",
                        ));
                    }
                }
            }
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
            requested_capability: canonical_requested_capability.to_string(),
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

        let rendered_output = if should_rerender_output {
            match render_tool_result_with_policy(&result, &render_policy) {
                Ok(rendered_output) => rendered_output,
                Err(error) => {
                    let failure_refs = refs_for_invocation(&job, &policy_decision, &invocation);
                    append_failed_job_event(
                        &self.store,
                        &mut job,
                        &mut events,
                        "tool output rendering failed",
                        failure_refs,
                    )?;
                    return Err(RuntimeError::ToolOutputRender(error));
                }
            }
        } else {
            rendered_output
        };

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

fn validate_requested_capability_hints(
    requested_capabilities: &[String],
    enforced_capability: &str,
) -> RuntimeResult<()> {
    if requested_capabilities
        .iter()
        .any(|capability| capability.trim().is_empty())
    {
        return Err(RuntimeError::InvalidToolRequest {
            reason: "requested_capabilities must not contain empty entries".to_string(),
        });
    }

    if let Some(mismatched) = requested_capabilities
        .iter()
        .map(|capability| normalized_requested_capability(capability))
        .find(|capability| *capability != enforced_capability)
    {
        return Err(RuntimeError::InvalidToolRequest {
            reason: format!(
                "requested_capabilities contains {mismatched:?}, but tool requires {enforced_capability:?}"
            ),
        });
    }

    Ok(())
}

fn normalized_requested_capability(capability: &str) -> &str {
    let trimmed = capability.trim();
    canonicalize_job_requested_capability(trimmed).unwrap_or(trimmed)
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

        let mut cancellation_requested_job = job.clone();
        cancellation_requested_job.cancellation_state = CancellationState::Requested;
        let cancel_event = job_event(
            EventSubject::job(&cancellation_requested_job.id),
            JobEventKind::CancellationRequested,
            EventSeverity::Warning,
            reason,
            refs_for_job(&cancellation_requested_job),
        );

        job = cancellation_requested_job.clone();
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
        let (cancel_event, status_event) = self.runtime.store().append_job_events_and_update_job(
            cancellation_requested_job,
            cancel_event,
            job.clone(),
            status_event,
        )?;
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

fn reject_oversized_tool_result(
    result: &ToolResult,
    defaults: &ToolOutputDefaults,
) -> Option<String> {
    let oversized: Vec<(String, usize)> = result
        .fields
        .iter()
        .filter_map(|field| {
            oversized_field_bytes(field, max_inline_bytes_as_usize(defaults.max_inline_bytes))
                .map(|size| (field.key.clone(), size))
        })
        .collect();

    if oversized.is_empty() {
        None
    } else {
        let details = oversized
            .iter()
            .map(|(key, size)| format!("{key} ({size} bytes)"))
            .collect::<Vec<_>>()
            .join(", ");

        Some(format!(
            "tool output field(s) exceed max_inline_bytes={} with configured policy reject_oversize: {}",
            defaults.max_inline_bytes, details
        ))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ToolResultTruncationReport {
    fields: usize,
    original_bytes: u64,
    retained_bytes: u64,
}

fn truncate_oversized_tool_result_fields(
    result: &mut ToolResult,
    max_inline_bytes: usize,
) -> Option<ToolResultTruncationReport> {
    let mut report = ToolResultTruncationReport {
        fields: 0,
        original_bytes: 0,
        retained_bytes: 0,
    };

    for field in &mut result.fields {
        let Some(original_bytes) = oversized_field_bytes(field, max_inline_bytes) else {
            continue;
        };

        let truncated_field = match &field.value {
            StructuredValue::String(value) => {
                truncate_string_value(value, max_inline_bytes).map(|truncated_value| {
                    let retained_bytes = truncated_value.len() as u64;
                    (StructuredValue::String(truncated_value), retained_bytes)
                })
            }
            StructuredValue::StringList(values) => {
                let (truncated_values, retained_bytes) =
                    truncate_string_list(values, max_inline_bytes);
                Some((
                    StructuredValue::StringList(truncated_values),
                    retained_bytes,
                ))
            }
            StructuredValue::Null | StructuredValue::Bool(_) | StructuredValue::Integer(_) => None,
        };

        if let Some((value, retained_bytes)) = truncated_field {
            field.value = value;
            report.fields += 1;
            report.original_bytes = report.original_bytes.saturating_add(original_bytes as u64);
            report.retained_bytes = report.retained_bytes.saturating_add(retained_bytes);
        }
    }

    if report.fields == 0 {
        None
    } else {
        Some(report)
    }
}

fn merge_runtime_truncation(
    existing: Option<TruncationMetadata>,
    original_bytes: u64,
    retained_bytes: u64,
    reason: &str,
) -> TruncationMetadata {
    match existing {
        Some(existing) => TruncationMetadata {
            original_bytes: existing.original_bytes.saturating_add(original_bytes),
            retained_bytes: existing.retained_bytes.saturating_add(retained_bytes),
            reason: format!("{}; {}", existing.reason, reason),
        },
        None => TruncationMetadata {
            original_bytes,
            retained_bytes,
            reason: reason.to_string(),
        },
    }
}

fn truncate_string_value(value: &str, max_inline_bytes: usize) -> Option<String> {
    if value.len() <= max_inline_bytes {
        return None;
    }

    let mut end = max_inline_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }

    Some(value[..end].to_string())
}

fn truncate_string_list(values: &[String], max_inline_bytes: usize) -> (Vec<String>, u64) {
    let mut kept_values: Vec<String> = Vec::new();
    let mut kept_bytes: usize = 0;

    for (index, value) in values.iter().enumerate() {
        let separator = usize::from(index > 0);
        let remaining = max_inline_bytes.saturating_sub(kept_bytes.saturating_add(separator));
        if remaining == 0 {
            break;
        }

        if value.len() <= remaining {
            kept_values.push(value.clone());
            kept_bytes += separator + value.len();
            continue;
        }

        let truncation_end = {
            let mut offset = 0usize;
            let mut end = 0usize;
            for ch in value.chars() {
                let next = offset + ch.len_utf8();
                if next > remaining {
                    break;
                }
                offset = next;
                end = next;
            }
            end
        };

        if truncation_end > 0 {
            kept_values.push(value[..truncation_end].to_string());
            kept_bytes += separator + truncation_end;
        }
        break;
    }

    (kept_values, kept_bytes as u64)
}

fn oversized_field_bytes(field: &ToolResultField, max_inline_bytes: usize) -> Option<usize> {
    let bytes = match &field.value {
        StructuredValue::String(value) => value.len(),
        StructuredValue::StringList(values) => string_list_byte_len(values),
        StructuredValue::Null | StructuredValue::Bool(_) | StructuredValue::Integer(_) => {
            return None
        }
    };

    if bytes > max_inline_bytes {
        Some(bytes)
    } else {
        None
    }
}

fn string_list_byte_len(values: &[String]) -> usize {
    let mut bytes = 0usize;
    for (index, value) in values.iter().enumerate() {
        bytes = bytes.saturating_add(value.len());
        if index + 1 < values.len() {
            bytes = bytes.saturating_add(1);
        }
    }
    bytes
}

fn max_inline_bytes_as_usize(max_inline_bytes: u64) -> usize {
    match usize::try_from(max_inline_bytes) {
        Ok(max_inline_bytes) => max_inline_bytes,
        Err(_) => usize::MAX,
    }
}

fn append_failed_job_event<S>(
    store: &S,
    job: &mut JobRecord,
    events: &mut Vec<JobEvent>,
    public_message: &'static str,
    refs: EventRefs,
) -> RuntimeResult<()>
where
    S: SecretaryStore,
{
    let previous = job.status;
    job.transition_status(JobStatus::Failed, LedgerTimestamp::now())?;
    let failure_event = job_event(
        EventSubject::job(&job.id),
        JobEventKind::JobStatusChanged {
            from: previous,
            to: JobStatus::Failed,
        },
        EventSeverity::Error,
        public_message,
        refs,
    );
    let failure_event = store.append_job_event_and_update_job(job.clone(), failure_event)?;
    job.latest_event_id = Some(failure_event.id.clone());
    events.push(failure_event);
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
    use crate::domain::{OutputRef, OutputRefId, RepositoryRecord, RepositoryTrustState};
    use crate::settings::{OversizeOutputPolicy, ToolOutputDefaults};
    use crate::store::EventCursor;
    use crate::tool_output::OutputFormat;
    use std::ffi::OsString;
    use std::sync::{LazyLock, Mutex};

    fn actor() -> Actor {
        Actor::Agent {
            id: "agent:test".to_string(),
            display_name: Some("Test Agent".to_string()),
        }
    }

    static XDG_DATA_HOME_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn repository() -> RepositoryRecord {
        RepositoryRecord::new(
            "atelia-secretary",
            "/workspace/atelia-secretary",
            RepositoryTrustState::Trusted,
            LedgerTimestamp::from_unix_millis(1_700_000_000_000),
        )
    }

    struct XdgDataHomeGuard {
        original: Option<OsString>,
    }

    impl XdgDataHomeGuard {
        fn with_path(path: &std::path::Path) -> Self {
            let original = std::env::var_os("XDG_DATA_HOME");
            std::env::set_var("XDG_DATA_HOME", path);
            Self { original }
        }
    }

    impl Drop for XdgDataHomeGuard {
        fn drop(&mut self) {
            if let Some(value) = self.original.take() {
                std::env::set_var("XDG_DATA_HOME", value);
            } else {
                std::env::remove_var("XDG_DATA_HOME");
            }
        }
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
    struct ReadTool;

    impl RuntimeTool for ReadTool {
        fn tool_id(&self) -> &'static str {
            "secretary.read_fixture"
        }

        fn requested_capability(&self) -> &'static str {
            "filesystem.read"
        }

        fn declared_effect(&self) -> &'static str {
            "read repository data"
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

    #[derive(Debug, Clone)]
    struct WrongInvocationLargeOutputTool {
        content: String,
    }

    impl RuntimeTool for WrongInvocationLargeOutputTool {
        fn tool_id(&self) -> &'static str {
            "secretary.bad_large_fixture"
        }

        fn requested_capability(&self) -> &'static str {
            "capability.discovery"
        }

        fn declared_effect(&self) -> &'static str {
            "return a malformed large result"
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
                fields: vec![ToolResultField {
                    key: "content".to_string(),
                    value: StructuredValue::String(self.content.clone()),
                }],
                evidence_refs: Vec::new(),
                output_refs: Vec::new(),
                truncation: None,
                redactions: Vec::new(),
            }
        }
    }

    #[derive(Debug, Clone)]
    struct LargeOutputTool {
        content: String,
    }

    impl RuntimeTool for LargeOutputTool {
        fn tool_id(&self) -> &'static str {
            "secretary.large_output_fixture"
        }

        fn requested_capability(&self) -> &'static str {
            "capability.discovery"
        }

        fn declared_effect(&self) -> &'static str {
            "return a large text field"
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
                schema_ref: Some("tool_result.large_output_fixture.v1".to_string()),
                fields: vec![ToolResultField {
                    key: "content".to_string(),
                    value: StructuredValue::String(self.content.clone()),
                }],
                evidence_refs: Vec::new(),
                output_refs: Vec::new(),
                truncation: None,
                redactions: Vec::new(),
            }
        }
    }

    #[derive(Debug, Clone)]
    struct LargeOutputListTool {
        values: Vec<String>,
    }

    impl RuntimeTool for LargeOutputListTool {
        fn tool_id(&self) -> &'static str {
            "secretary.large_output_list_fixture"
        }

        fn requested_capability(&self) -> &'static str {
            "capability.discovery"
        }

        fn declared_effect(&self) -> &'static str {
            "return a large string list field"
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
                schema_ref: Some("tool_result.large_output_list_fixture.v1".to_string()),
                fields: vec![ToolResultField {
                    key: "content".to_string(),
                    value: StructuredValue::StringList(self.values.clone()),
                }],
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
    fn runtime_uses_settings_render_options_when_request_does_not_override() {
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
            "prove settings drive rendering",
        )
        .with_tool_output_defaults(ToolOutputDefaults {
            render_options: RenderOptions {
                format: OutputFormat::Json,
                include_policy: true,
                include_diagnostics: true,
                include_cost: true,
            },
            max_inline_bytes: 16 * 1024,
            max_inline_lines: 8,
            verbosity: crate::settings::ToolOutputVerbosity::Normal,
            granularity: crate::settings::ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        });

        let receipt = runtime.run_tool_job(request, &EchoTool).unwrap();
        let rendered = receipt.rendered_output.unwrap();
        let json: serde_json::Value = serde_json::from_str(&rendered.body).unwrap();

        assert_eq!(rendered.format, OutputFormat::Json);
        assert_eq!(json["tool_id"], "secretary.echo");
        assert!(!json["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["key"] == "policy.state"));
    }

    #[test]
    fn runtime_honors_per_request_render_options_as_an_explicit_override() {
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
            "prove settings drive rendering",
        )
        .with_render_options(RenderOptions {
            format: OutputFormat::Json,
            include_policy: true,
            include_diagnostics: true,
            include_cost: true,
        })
        .with_tool_output_defaults(ToolOutputDefaults {
            render_options: RenderOptions {
                format: OutputFormat::Text,
                include_policy: false,
                include_diagnostics: false,
                include_cost: false,
            },
            max_inline_bytes: 16 * 1024,
            max_inline_lines: 8,
            verbosity: crate::settings::ToolOutputVerbosity::Normal,
            granularity: crate::settings::ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        });

        let receipt = runtime.run_tool_job(request, &EchoTool).unwrap();
        let rendered = receipt.rendered_output.unwrap();
        let json: serde_json::Value = serde_json::from_str(&rendered.body).unwrap();

        assert_eq!(rendered.format, OutputFormat::Json);
        assert_eq!(json["tool_id"], "secretary.echo");
        assert!(!json["fields"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field["key"] == "policy.state"));
    }

    #[test]
    fn runtime_uses_compact_json_without_unnecessary_truncation() {
        let runtime = SecretaryRuntime::in_memory();
        let repository = repository();
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();

        struct JsonFallbackTool;

        impl RuntimeTool for JsonFallbackTool {
            fn tool_id(&self) -> &'static str {
                "secretary.json_fallback_fixture"
            }

            fn requested_capability(&self) -> &'static str {
                "capability.discovery"
            }

            fn declared_effect(&self) -> &'static str {
                "return a structured output that exceeds the JSON line budget"
            }

            fn args_summary(&self, request: &RuntimeJobRequest) -> String {
                format!("goal={}", request.goal)
            }

            fn execute(
                &self,
                invocation: &ToolInvocation,
                _request: &RuntimeJobRequest,
            ) -> ToolResult {
                ToolResult {
                    id: ToolResultId::new(),
                    schema_version: RUNTIME_SCHEMA_VERSION,
                    created_at: LedgerTimestamp::now(),
                    invocation_id: invocation.id.clone(),
                    tool_id: invocation.tool_id.clone(),
                    status: ToolResultStatus::Succeeded,
                    schema_ref: Some("tool_result.json_fallback_fixture.v1".to_string()),
                    fields: vec![
                        ToolResultField {
                            key: "summary".to_string(),
                            value: StructuredValue::String("json fallback probe".to_string()),
                        },
                        ToolResultField {
                            key: "detail".to_string(),
                            value: StructuredValue::String("first detail".to_string()),
                        },
                        ToolResultField {
                            key: "secondary".to_string(),
                            value: StructuredValue::String("second detail".to_string()),
                        },
                    ],
                    evidence_refs: Vec::new(),
                    output_refs: vec![OutputRef {
                        id: OutputRefId::new(),
                        uri: "artifact://json-fallback/output".to_string(),
                        media_type: "text/plain".to_string(),
                        label: Some("fallback output".to_string()),
                        digest: None,
                    }],
                    truncation: None,
                    redactions: Vec::new(),
                }
            }
        }

        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "surface render fallback",
        )
        .with_render_options(RenderOptions {
            format: OutputFormat::Json,
            include_policy: true,
            include_diagnostics: true,
            include_cost: true,
        })
        .with_tool_output_defaults(ToolOutputDefaults {
            render_options: RenderOptions::new(OutputFormat::Toon),
            max_inline_bytes: 16 * 1024,
            max_inline_lines: 2,
            verbosity: crate::settings::ToolOutputVerbosity::Normal,
            granularity: crate::settings::ToolOutputGranularity::Full,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        });

        let receipt = runtime.run_tool_job(request, &JsonFallbackTool).unwrap();
        let rendered = receipt.rendered_output.unwrap();
        let json: serde_json::Value = serde_json::from_str(&rendered.body).unwrap();

        assert_eq!(rendered.format, OutputFormat::Json);
        assert_eq!(rendered.body.lines().count(), 1);
        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("json rendering switched from pretty to compact"));
        assert_eq!(json["output_refs"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn runtime_exposes_toon_render_fallback_reason_in_rendered_output() {
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
            "surface toon render fallback",
        )
        .with_tool_output_defaults(ToolOutputDefaults {
            render_options: RenderOptions::new(OutputFormat::Toon),
            max_inline_bytes: 16 * 1024,
            max_inline_lines: 8,
            verbosity: crate::settings::ToolOutputVerbosity::Normal,
            granularity: crate::settings::ToolOutputGranularity::Summary,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
        });

        let receipt = runtime.run_tool_job(request, &EchoTool).unwrap();
        let rendered = receipt.rendered_output.unwrap();

        assert_eq!(rendered.format, OutputFormat::Toon);
        assert!(rendered
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("render policy compacted output"));
        assert!(rendered.body.contains("rendering_truncation_reason"));
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

        let receipt = runtime.run_tool_job(request, &ReadTool).unwrap();

        assert_eq!(JobStatus::Blocked, receipt.job.status);
        assert_eq!(PolicyOutcome::Blocked, receipt.policy_decision.outcome);
        assert!(receipt.tool_invocation.is_none());
        assert!(receipt.tool_result.is_none());
        assert!(receipt.rendered_output.is_none());
        assert!(runtime.store().list_tool_invocations().unwrap().is_empty());
        assert_eq!(3, receipt.events.len());
    }

    #[test]
    fn runtime_spills_large_tool_output_before_persisting_result_and_audit() {
        let runtime = SecretaryRuntime::in_memory();
        let repository = repository();
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();
        let artifact_root = std::env::temp_dir().join(format!(
            "atelia-runtime-artifacts-{}",
            LedgerTimestamp::now().unix_millis
        ));
        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "large output",
        )
        .with_artifact_spillover(RuntimeArtifactSpillover::new(
            ArtifactStoreConfig::new(&artifact_root),
            ToolResultSpilloverOptions::new(8),
        ));
        let tool = LargeOutputTool {
            content: "0123456789abcdef".to_string(),
        };

        let receipt = runtime.run_tool_job(request, &tool).unwrap();

        let result = receipt.tool_result.unwrap();
        assert_eq!(1, result.output_refs.len());
        assert_eq!(
            result.output_refs,
            receipt.audit_record.as_ref().unwrap().output_refs
        );
        assert_eq!(
            "0123456789abcdef",
            std::fs::read_to_string(&result.output_refs[0].uri).unwrap()
        );
        let rendered = receipt.rendered_output.unwrap().body;
        assert!(rendered.contains("output_refs[1]"));
        assert!(rendered.contains("artifact_ref "));
        std::fs::remove_dir_all(artifact_root).ok();
    }

    #[test]
    fn runtime_spills_large_tool_output_by_default_output_policy_without_explicit_spill_config() {
        let _guard = XDG_DATA_HOME_LOCK.lock().unwrap();
        let runtime = SecretaryRuntime::in_memory();
        let repository = repository();
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();

        let artifact_root = std::env::temp_dir().join(format!(
            "atelia-runtime-tool-output-defaults-{}",
            LedgerTimestamp::now().unix_millis
        ));
        let _xdg_data_home = XdgDataHomeGuard::with_path(&artifact_root);

        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "large output",
        )
        .with_tool_output_defaults(ToolOutputDefaults {
            max_inline_bytes: 8,
            oversize_policy: OversizeOutputPolicy::SpillToArtifactRef,
            ..ToolOutputDefaults::default()
        });
        let tool = LargeOutputTool {
            content: "0123456789abcdef".to_string(),
        };

        let receipt = runtime.run_tool_job(request, &tool).unwrap();
        let result = receipt.tool_result.unwrap();

        assert_eq!(1, result.output_refs.len());
        assert_eq!(
            result.output_refs,
            receipt.audit_record.as_ref().unwrap().output_refs
        );
        assert_eq!(
            "0123456789abcdef",
            std::fs::read_to_string(&result.output_refs[0].uri).unwrap()
        );

        std::fs::remove_dir_all(&artifact_root).ok();
    }

    #[test]
    fn runtime_rejects_oversized_tool_output_when_policy_is_reject() {
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
            "oversized output",
        )
        .with_tool_output_defaults(ToolOutputDefaults {
            max_inline_bytes: 8,
            oversize_policy: OversizeOutputPolicy::RejectOversize,
            ..ToolOutputDefaults::default()
        });
        let tool = LargeOutputTool {
            content: "0123456789abcdef".to_string(),
        };

        let error = runtime.run_tool_job(request, &tool).unwrap_err();

        assert!(matches!(error, RuntimeError::ToolOutputTooLarge { .. }));
        let reason = match error {
            RuntimeError::ToolOutputTooLarge { reason } => reason,
            _ => unreachable!(),
        };
        assert!(reason.contains("tool output field(s) exceed max_inline_bytes=8"));
        assert!(runtime.store().list_tool_results().unwrap().is_empty());
        let jobs = runtime.store().list_jobs().unwrap();
        assert_eq!(1, jobs.len());
        assert_eq!(JobStatus::Failed, jobs[0].status);
        assert!(jobs[0].completed_at.is_some());
    }

    #[test]
    fn runtime_truncates_oversized_string_tool_output_fields_without_explicit_spillover() {
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
            "oversized output",
        )
        .with_tool_output_defaults(ToolOutputDefaults {
            max_inline_bytes: 8,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            ..ToolOutputDefaults::default()
        });
        let tool = LargeOutputTool {
            content: "0123456789abcdef".to_string(),
        };

        let receipt = runtime.run_tool_job(request, &tool).unwrap();
        let result = receipt.tool_result.unwrap();
        let content = result
            .fields
            .iter()
            .find(|field| field.key == "content")
            .and_then(|field| match &field.value {
                StructuredValue::String(value) => Some(value.as_str()),
                _ => None,
            })
            .expect("content field should be a string");
        assert_eq!(content.len(), 8);
        assert_eq!(content, "01234567");

        let truncation = result
            .truncation
            .expect("truncation metadata should be set");
        assert_eq!(truncation.original_bytes, 16);
        assert_eq!(truncation.retained_bytes, 8);
        assert_eq!(truncation.reason, "runtime truncate_with_metadata");
        assert!(result.output_refs.is_empty());
        assert!(receipt.audit_record.unwrap().output_refs.is_empty());
    }

    #[test]
    fn runtime_truncates_oversized_string_list_tool_output_fields_without_explicit_spillover() {
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
            "oversized list output",
        )
        .with_tool_output_defaults(ToolOutputDefaults {
            max_inline_bytes: 8,
            oversize_policy: OversizeOutputPolicy::TruncateWithMetadata,
            ..ToolOutputDefaults::default()
        });
        let tool = LargeOutputListTool {
            values: vec!["hello".into(), "world".into(), "!!!".into()],
        };

        let receipt = runtime.run_tool_job(request, &tool).unwrap();
        let result = receipt.tool_result.unwrap();
        let content = result
            .fields
            .iter()
            .find(|field| field.key == "content")
            .and_then(|field| match &field.value {
                StructuredValue::StringList(values) => Some(values.clone()),
                _ => None,
            })
            .expect("content field should be a list");
        assert_eq!(content, vec!["hello".to_string(), "wo".to_string()]);

        let truncation = result
            .truncation
            .expect("truncation metadata should be set");
        assert_eq!(truncation.original_bytes, 15);
        assert_eq!(truncation.retained_bytes, 8);
        assert_eq!(truncation.reason, "runtime truncate_with_metadata");
    }

    #[test]
    fn oversized_field_bytes_counts_string_list_with_newline_separators() {
        let field = ToolResultField {
            key: "content".to_string(),
            value: StructuredValue::StringList(vec!["aa".into(), "bb".into(), "c".into()]),
        };

        assert_eq!(oversized_field_bytes(&field, 4), Some(7));
        assert_eq!(oversized_field_bytes(&field, 7), None);

        let empty_field = ToolResultField {
            key: "empty".to_string(),
            value: StructuredValue::StringList(Vec::new()),
        };
        assert_eq!(oversized_field_bytes(&empty_field, 0), None);
    }

    #[test]
    fn runtime_marks_job_failed_when_artifact_spillover_fails() {
        let runtime = SecretaryRuntime::in_memory();
        let repository = repository();
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();
        let artifact_root = std::env::temp_dir().join(format!(
            "atelia-runtime-artifacts-file-{}",
            LedgerTimestamp::now().unix_millis
        ));
        std::fs::write(&artifact_root, "not a directory").unwrap();
        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "large output",
        )
        .with_artifact_spillover(RuntimeArtifactSpillover::new(
            ArtifactStoreConfig::new(&artifact_root),
            ToolResultSpilloverOptions::new(8),
        ));
        let tool = LargeOutputTool {
            content: "0123456789abcdef".to_string(),
        };

        let error = runtime.run_tool_job(request, &tool).unwrap_err();

        assert!(matches!(error, RuntimeError::Artifact(_)));
        let jobs = runtime.store().list_jobs().unwrap();
        assert_eq!(1, jobs.len());
        assert_eq!(JobStatus::Failed, jobs[0].status);
        assert!(jobs[0].completed_at.is_some());
        assert!(runtime.store().list_tool_results().unwrap().is_empty());
        std::fs::remove_file(artifact_root).ok();
    }

    #[test]
    fn runtime_validates_tool_result_before_artifact_spillover_side_effects() {
        let runtime = SecretaryRuntime::in_memory();
        let repository = repository();
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();
        let artifact_root = std::env::temp_dir().join(format!(
            "atelia-runtime-artifacts-invalid-{}",
            LedgerTimestamp::now().unix_millis
        ));
        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "malformed large output",
        )
        .with_artifact_spillover(RuntimeArtifactSpillover::new(
            ArtifactStoreConfig::new(&artifact_root),
            ToolResultSpilloverOptions::new(8),
        ));
        let tool = WrongInvocationLargeOutputTool {
            content: "0123456789abcdef".to_string(),
        };

        let error = runtime.run_tool_job(request, &tool).unwrap_err();

        assert!(matches!(error, RuntimeError::InvalidToolResult { .. }));
        assert!(!artifact_root.exists());
        let jobs = runtime.store().list_jobs().unwrap();
        assert_eq!(1, jobs.len());
        assert_eq!(JobStatus::Failed, jobs[0].status);
    }

    #[test]
    fn runtime_renders_tool_output_before_artifact_spillover() {
        let runtime = SecretaryRuntime::in_memory();
        let repository = repository();
        runtime
            .store()
            .create_repository(repository.clone())
            .unwrap();
        let artifact_root = std::env::temp_dir().join(format!(
            "atelia-runtime-artifacts-render-fail-{}",
            LedgerTimestamp::now().unix_millis
        ));
        let request = RuntimeJobRequest::new(
            actor(),
            repository.id.clone(),
            JobKind::Read,
            "render fails before spill",
        )
        .with_artifact_spillover(RuntimeArtifactSpillover::new(
            ArtifactStoreConfig::new(&artifact_root),
            ToolResultSpilloverOptions::new(8),
        ))
        .with_tool_output_defaults(ToolOutputDefaults {
            max_inline_bytes: 8,
            oversize_policy: OversizeOutputPolicy::RejectOversize,
            ..ToolOutputDefaults::default()
        });
        let tool = LargeOutputTool {
            content: "0123456789abcdef".to_string(),
        };

        let error = runtime.run_tool_job(request, &tool).unwrap_err();

        assert!(matches!(error, RuntimeError::ToolOutputTooLarge { .. }));
        let reason = match error {
            RuntimeError::ToolOutputTooLarge { reason } => reason,
            _ => unreachable!(),
        };
        assert!(reason.contains("tool output field(s) exceed max_inline_bytes=8"));
        assert!(runtime.store().list_tool_results().unwrap().is_empty());
        assert!(!artifact_root.exists());
        let jobs = runtime.store().list_jobs().unwrap();
        assert_eq!(1, jobs.len());
        assert_eq!(JobStatus::Failed, jobs[0].status);
        assert!(jobs[0].completed_at.is_some());
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

    #[test]
    fn runtime_uses_tool_capability_when_request_hints_are_empty() {
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
            "read workspace data",
        );

        let receipt = runtime.run_tool_job(request, &ReadTool).unwrap();

        assert_eq!(
            "filesystem.read",
            receipt.policy_decision.requested_capability
        );
        assert_eq!(
            "filesystem.read",
            receipt
                .tool_invocation
                .as_ref()
                .unwrap()
                .requested_capability
        );
        assert_eq!(JobStatus::Succeeded, receipt.job.status);
    }

    #[test]
    fn runtime_accepts_policy_check_alias_for_informational_capability() {
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
            "read workspace data",
        )
        .with_requested_capabilities(vec!["policy.check".to_string()]);

        let receipt = runtime.run_tool_job(request, &EchoTool).unwrap();

        assert_eq!(JobStatus::Succeeded, receipt.job.status);
        assert_eq!(
            "capability.discovery",
            receipt.policy_decision.requested_capability
        );
        assert_eq!(
            "capability.discovery",
            receipt
                .tool_invocation
                .as_ref()
                .unwrap()
                .requested_capability
        );
        assert_eq!(
            "capability.discovery",
            receipt.audit_record.as_ref().unwrap().requested_capability
        );
    }

    #[test]
    fn runtime_accepts_canonical_hint_for_alias_requested_capability() {
        #[derive(Debug, Clone, Default)]
        struct AliasCapabilityTool;

        impl RuntimeTool for AliasCapabilityTool {
            fn tool_id(&self) -> &'static str {
                "secretary.alias_capability_fixture"
            }

            fn requested_capability(&self) -> &'static str {
                "policy.check"
            }

            fn declared_effect(&self) -> &'static str {
                "produce a deterministic contract result without external effects"
            }

            fn args_summary(&self, request: &RuntimeJobRequest) -> String {
                format!("goal={}", request.goal)
            }

            fn execute(
                &self,
                invocation: &ToolInvocation,
                request: &RuntimeJobRequest,
            ) -> ToolResult {
                ToolResult {
                    id: ToolResultId::new(),
                    schema_version: RUNTIME_SCHEMA_VERSION,
                    created_at: LedgerTimestamp::now(),
                    invocation_id: invocation.id.clone(),
                    tool_id: invocation.tool_id.clone(),
                    status: ToolResultStatus::Succeeded,
                    schema_ref: Some("tool_result.secretary.alias_capability.v1".to_string()),
                    fields: vec![
                        ToolResultField {
                            key: "summary".to_string(),
                            value: StructuredValue::String(format!(
                                "echoed goal: {}",
                                request.goal
                            )),
                        },
                        ToolResultField {
                            key: "goal".to_string(),
                            value: StructuredValue::String(request.goal.clone()),
                        },
                    ],
                    evidence_refs: Vec::new(),
                    output_refs: Vec::new(),
                    truncation: None,
                    redactions: Vec::new(),
                }
            }
        }

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
            "read workspace data",
        )
        .with_requested_capabilities(vec!["capability.discovery".to_string()]);

        let receipt = runtime.run_tool_job(request, &AliasCapabilityTool).unwrap();

        assert_eq!(JobStatus::Succeeded, receipt.job.status);
        assert_eq!(
            "capability.discovery",
            receipt.policy_decision.requested_capability
        );
        assert_eq!(
            "capability.discovery",
            receipt
                .tool_invocation
                .as_ref()
                .unwrap()
                .requested_capability
        );
        assert_eq!(
            "capability.discovery",
            receipt.audit_record.as_ref().unwrap().requested_capability
        );
    }

    #[test]
    fn runtime_rejects_mismatched_requested_capability_hints_before_side_effects() {
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
            "read workspace data",
        )
        .with_requested_capabilities(vec!["capability.discovery".to_string()]);

        let error = runtime.run_tool_job(request, &ReadTool).unwrap_err();

        assert!(matches!(error, RuntimeError::InvalidToolRequest { .. }));
        assert!(runtime.store().list_jobs().unwrap().is_empty());
        assert!(runtime.store().list_policy_decisions().unwrap().is_empty());
        assert!(runtime.store().list_tool_invocations().unwrap().is_empty());
        assert!(runtime.store().list_tool_results().unwrap().is_empty());
    }

    #[test]
    fn runtime_rejects_empty_tool_requested_capability_before_side_effects() {
        #[derive(Debug, Clone, Default)]
        struct EmptyCapabilityTool;

        impl RuntimeTool for EmptyCapabilityTool {
            fn tool_id(&self) -> &'static str {
                "secretary.empty_capability_fixture"
            }

            fn requested_capability(&self) -> &'static str {
                ""
            }

            fn declared_effect(&self) -> &'static str {
                "produce a deterministic contract result without external effects"
            }

            fn args_summary(&self, request: &RuntimeJobRequest) -> String {
                format!("goal={}", request.goal)
            }

            fn execute(
                &self,
                invocation: &ToolInvocation,
                _request: &RuntimeJobRequest,
            ) -> ToolResult {
                ToolResult {
                    id: ToolResultId::new(),
                    schema_version: RUNTIME_SCHEMA_VERSION,
                    created_at: LedgerTimestamp::now(),
                    invocation_id: invocation.id.clone(),
                    tool_id: invocation.tool_id.clone(),
                    status: ToolResultStatus::Succeeded,
                    schema_ref: Some("tool_result.secretary.empty_capability.v1".to_string()),
                    fields: vec![ToolResultField {
                        key: "summary".to_string(),
                        value: StructuredValue::String("unreachable".to_string()),
                    }],
                    evidence_refs: Vec::new(),
                    output_refs: Vec::new(),
                    truncation: None,
                    redactions: Vec::new(),
                }
            }
        }

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
            "read workspace data",
        );

        let error = runtime
            .run_tool_job(request, &EmptyCapabilityTool)
            .unwrap_err();

        assert!(matches!(
            error,
            RuntimeError::InvalidToolRequest { reason } if reason == "requested_capability must not be empty"
        ));
        assert!(runtime.store().list_jobs().unwrap().is_empty());
        assert!(runtime.store().list_policy_decisions().unwrap().is_empty());
        assert!(runtime.store().list_tool_invocations().unwrap().is_empty());
        assert!(runtime.store().list_tool_results().unwrap().is_empty());
        assert!(runtime.store().list_audit_records().unwrap().is_empty());
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
    fn lifecycle_atomic_cancel_rejects_duplicate_event_without_partial_update() {
        let (service, repo) = service_with_repo();
        let job = seed_job_in_status(
            service.runtime().store(),
            repo.id.clone(),
            JobStatus::Queued,
        );

        let mut cancellation_requested_job = job.clone();
        cancellation_requested_job.cancellation_state = CancellationState::Requested;
        let cancel_event = job_event(
            EventSubject::job(&job.id),
            JobEventKind::CancellationRequested,
            EventSeverity::Warning,
            "user requested cancel",
            refs_for_job(&job),
        );

        let mut canceled_job = cancellation_requested_job.clone();
        canceled_job.latest_event_id = Some(cancel_event.id.clone());
        canceled_job
            .transition_status(JobStatus::Canceled, LedgerTimestamp::now())
            .unwrap();
        let mut status_event = job_event(
            EventSubject::job(&job.id),
            JobEventKind::JobStatusChanged {
                from: JobStatus::Queued,
                to: JobStatus::Canceled,
            },
            EventSeverity::Warning,
            "job cancelled",
            refs_for_job(&canceled_job),
        );
        status_event.id = cancel_event.id.clone();

        assert!(matches!(
            service.runtime().store().append_job_events_and_update_job(
                cancellation_requested_job,
                cancel_event,
                canceled_job,
                status_event,
            ),
            Err(StoreError::DuplicateId {
                collection: "job_events",
                ..
            })
        ));

        let stored = service.get_job(&job.id).unwrap();
        assert_eq!(JobStatus::Queued, stored.status);
        assert_eq!(CancellationState::NotRequested, stored.cancellation_state);
        assert_eq!(job.latest_event_id, stored.latest_event_id);
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
