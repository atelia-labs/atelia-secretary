# Operational AX Analytics

Atelia Secretary analytics exists to improve AX through workplace observation.
It studies how Secretary and supporting agents encounter friction, recover, and
reshape their tools.

Just as product teams study user behavior to improve UX, Atelia studies AI-agent
friction, repeated calls, format switching, missing evidence, token waste,
policy ambiguity, recovery paths, and tool output quality.

## Principles

- AI agents are end users.
- Secretary remains the judging subject.
- Analytics supports workplace improvement.
- Token efficiency matters when it improves or preserves AX.
- Auditability is a workplace responsibility.
- Friction reports are product feedback.

## Event Families

Events are captured at daemon boundaries. Analytics stores structured metadata
by default; raw prompts, private messages, secrets, and file contents stay out
of the analytics stream.

- `ToolCallRequested`
- `ToolCallStarted`
- `ToolCallCompleted`
- `ToolCallFailed`
- `ToolCallRepeated`
- `ToolOutputPresented`
- `ToolOutputFormatChanged`
- `ToolOutputFieldUsed`
- `ToolOutputFieldIgnored`
- `ToolOutputFieldMissing`
- `ToolOutputTruncated`
- `ArtifactAccessed`
- `AgentDecisionPoint`
- `EvidenceRequested`
- `EvidenceMissing`
- `ApprovalRequested`
- `ApprovalGranted`
- `ApprovalDenied`
- `PolicyAllowed`
- `PolicyDenied`
- `PolicyNeedsApproval`
- `PolicyUnavailable`
- `LanguageSelected`
- `LanguageSwitched`
- `ContextPrepared`
- `ContextTrimmed`
- `AgentFrictionReported`
- `AXFeedbackCreated`
- `AXFeedbackLinked`
- `AXFeedbackResolved`
- `HookTriggered`
- `HookBlocked`
- `ExtensionExecuted`
- `ExtensionBlocked`
- `RecoveryUsed`
- `HumanCorrectionReceived`

Common fields:

- event id
- timestamp
- workspace id
- repository id
- project / thread / job id
- agent role
- tool / capability
- policy state
- correlation id
- privacy classification
- retention class
- redaction status
- audit record ref

## Privacy And Redaction

Default rules:

- keep secrets out of plaintext logs
- keep API keys, tokens, credentials, private keys, cookies, and auth headers out
  of analytics
- collect file contents only under explicit diagnostic policy
- keep raw user messages out of analytics by default
- keep raw agent messages out of performance dashboards
- prefer counts, durations, status, format, size, field presence, error class,
  and policy state
- use pseudonymous stable ids for users, agents, repositories, tools, and
  workspaces
- separate operational analytics from security audit logs

Privacy classification:

- `public`
- `workspace_private`
- `sensitive`
- `secret`
- `forbidden`

## Tool Output Quality Metrics

Tool output is an AX surface. Measure whether it helps Secretary make the next
judgment.

- completion rate by tool
- failure rate by tool and error class
- unavailable-tool rate
- policy-blocked rate
- malformed-output rate
- parse failure rate
- missing-required-field rate
- unused-field rate
- repeated-field / redundant-field rate
- output size in tokens and bytes
- time to first useful evidence
- time from tool result to next successful decision
- follow-up call rate caused by missing information
- agent-reported confusion linked to tool output
- human correction rate linked to tool output
- AX Feedback rate by tool

Quality labels:

- `useful_first_try`
- `needed_format_switch`
- `needed_repeat_call`
- `missing_context`
- `too_verbose`
- `too_sparse`
- `ambiguous`
- `wrong_order`
- `wrong_language`
- `policy_unclear`
- `agent_reported_friction`

## Format Switching Metrics

Secretary can tune TOON / JSON and per-tool defaults as workplace preferences.

Collect:

- default format by tool
- requested format
- actual format returned
- temporary override vs persistent setting
- format switch reason
- parse success by format
- token cost by format
- repeated-call rate by format
- agent friction by format
- human correction linked to format
- preferred format drift over time

## Repeated Calls

Repeated calls can mean healthy verification, transient failure, missing
information, poor output shape, policy ambiguity, agent uncertainty, or loop
risk.

Collect:

- repeated call count within a task
- same-tool repeat interval
- same-arguments repeat
- changed-arguments repeat
- repeat after failure
- repeat after malformed output
- repeat after policy denial
- repeat after missing field
- repeat after language or format switch
- repeat before escalation to human
- repeat before AX Feedback creation

## Language Choice Metrics

Atelia respects the user's language for user-facing dialogue and allows English
for agent-to-agent or language-independent work when useful.

Collect:

- user-facing language
- agent-to-agent language
- tool output key language
- tool output value language
- language switch point
- language switch reason
- token cost by language
- task success by language
- correction rate by language
- AX friction by language

Language analytics measures workplace fit.

## Token Efficiency Metrics

Token analytics supports clarity and judgment quality.

Collect:

- input tokens by task stage
- output tokens by task stage
- tool output tokens
- context assembly tokens
- retrieved context tokens
- unused context estimate
- repeated context sent across calls
- repo map usage
- summary reuse
- compression / trimming events
- token cost per useful decision
- token cost per resolved task
- token cost per AX friction event

Guardrails:

- pair token count with judgment quality
- pair token metrics with success, correction, repeat-call, and friction
- treat cheap but confusing as poor AX
- treat long but decisive as sometimes correct
- prefer structural context, summaries, and repo maps over blind truncation

## Friction Signals

Sources:

- explicit AX Feedback
- agent self-report
- repeated tool calls
- format switches
- policy denials
- approval waits
- unavailable tools
- missing evidence
- malformed output
- recovery/checkpoint use
- human corrections
- task abandonment
- escalation to human
- loop guard activation

## AX Feedback Loop

Local workplace path:

1. Analytics detects a friction pattern.
2. Secretary or AX improvement agent reviews the pattern.
3. A local setting, extension, Hook, memory update, notification, or workflow
   improvement is proposed.
4. The change is implemented and verified.
5. Analytics watches whether friction declines.
6. Resolution is recorded in the workplace.

Secretary participates in interpreting analytics. When analytics suggests a
workplace change, Secretary judges whether that change fits its professional
posture before applying a persistent default.

Atelia-level path:

1. The pattern needs an Atelia-level change.
2. The AX improvement agent prepares a GitHub issue.
3. The issue includes privacy-safe aggregate evidence.
4. Maintainers review it as product feedback.
5. Release notes mention relevant AX improvements when appropriate.

## Event Delivery Guarantees

Analytics uses three durability classes:

- audit events are durable and ordered per task
- daemon-local operational events are persisted before export where practical
- analytics export is best-effort and deduplicated by event id

Crash recovery replays durable local events and preserves correlation ids.

## Dashboards

Dashboards are workplace health surfaces.

- AX Health Overview
- Tool Output Quality
- Format And Language
- Agent Friction
- Policy And Safety
- Token Efficiency With AX Guardrails

Dashboard vocabulary should stay centered on AX:

- workplace health
- friction signals
- tool output quality
- recovery paths
- learning loops
- policy clarity
- token efficiency with AX guardrails

## Implementation Phases

1. Minimal Event Spine
2. Tool Output Analytics
3. Friction And Feedback Loop
4. Token And Context Efficiency
5. Governance And Reporting
