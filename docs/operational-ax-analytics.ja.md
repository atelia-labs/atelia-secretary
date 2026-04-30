# Operational AX Analytics

Atelia Secretary の analytics は、AX を改善するための仕事場観察です。Secretary と支援エージェントがどこで迷い、どこで復旧し、どのように道具を育てていくかを見ます。

通常の product が user behavior を見て UX を改善するように、Atelia は AI エージェントの作業中の friction、迷い、再実行、format 切り替え、不要 field、token waste、policy ambiguity、recovery path を観測します。

## Principles

- AI agents are end users.
- Secretary remains the judging subject.
- Analytics supports workplace improvement.
- Token efficiency matters when it improves or preserves AX.
- Auditability is a workplace responsibility.
- Friction reports are product feedback.

## Event Families

収集する event は daemon boundary で構造化します。analytics は default で構造化 metadata を扱い、raw prompt、raw private message、secret、file contents は analytics stream に入れません。

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

共通 field:

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

default rules:

- secret を plaintext log から外す
- API key、token、credential、private key、cookie、auth header を analytics から外す
- file contents は明示的な diagnostic policy の下でのみ扱う
- raw user message は default analytics から外す
- raw agent message は performance dashboard から外す
- counts、duration、status、format、size、field presence、error class、policy state を優先する
- user、agent、repository、tool、workspace は pseudonymous stable id を使う
- operational analytics と security audit log を分離する

privacy classification:

- `public`
- `workspace_private`
- `sensitive`
- `secret`
- `forbidden`

redaction pipeline:

1. daemon boundary で event を capture
2. field を classify
3. sensitive value を redact / hash
4. forbidden field を drop
5. redaction metadata を付与
6. minimal analytics record を保存
7. policy が必要とする場合のみ、厳格な audit storage に detail を残す

## Tool Output Quality Metrics

tool output は AX surface です。Secretary が次の判断をしやすいかを測ります。

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

quality labels:

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

Secretary は TOON / JSON や per-tool default を仕事場の好みとして調整できます。

collect:

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

format switch reasons:

- `agent_preference`
- `tool_default`
- `temporary_debugging`
- `parse_failure`
- `missing_fields`
- `too_verbose`
- `too_sparse`
- `token_efficiency`
- `human_review`
- `compatibility`

## Repeated Calls

repeated call は verification、transient failure、情報不足、output shape、policy ambiguity、agent uncertainty、loop risk のいずれかを示します。

collect:

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

loop guard:

- repeated call burst rate
- maximum repeat depth
- repeated failure chain
- automatic stop reason
- recovery path used

## Language Choice Metrics

Atelia は user-facing dialogue では user language を尊重し、agent-to-agent / language-independent work では英語を選びやすくします。

collect:

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

language analytics は workplace fit を見るためのものです。

## Token Efficiency Metrics

token analytics は clarity と judgment quality を支えるために使います。

collect:

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

- token count と judgment quality を合わせて見る
- success、correction、repeat-call、friction と合わせて見る
- cheap but confusing は悪い AX として扱う
- long but decisive は正しい場合がある
- blind truncation より structural context、summary、repo map を優先する

## Friction Signals

sources:

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

taxonomy:

- tool output unclear
- missing capability
- permission boundary unclear
- blocked but reason unclear
- too much context
- too little context
- wrong format
- wrong language
- external tool unavailable
- repeated setup work
- insufficient memory
- unsafe ambiguity
- review surface missing
- extension or Hook behavior surprising

## AX Feedback Loop

Local workplace path:

1. analytics detects a friction pattern
2. Secretary or AX improvement agent reviews the pattern
3. local setting, extension, Hook, memory update, notification, or workflow improvement is proposed
4. change is implemented and verified
5. analytics watches whether friction declines
6. resolution is recorded in the workplace

Secretary は analytics の解釈に参加します。analytics が workplace change を示した場合、Secretary はその変更が自分の professional posture に合うか判断してから persistent default へ反映します。

Atelia-level path:

1. pattern needs an Atelia-level change
2. AX improvement agent prepares GitHub issue
3. issue includes privacy-safe aggregate evidence
4. maintainers review as product feedback
5. release notes mention relevant AX improvements when appropriate

## Event Delivery Guarantees

analytics は3つの durability class を持ちます。

- audit event は durable で task ごとに ordered
- daemon-local operational event は export 前に可能な範囲で persist
- analytics export は best-effort で event id により dedupe

crash recovery では durable local event を replay し、correlation id を維持します。

feedback record links:

- natural-language agent voice
- affected tool / workflow
- representative event ids
- privacy-safe aggregate evidence
- suspected cause
- proposed improvement
- resolution
- verification result

## Dashboards

dashboards are workplace health surfaces.

- AX Health Overview
- Tool Output Quality
- Format And Language
- Agent Friction
- Policy And Safety
- Token Efficiency With AX Guardrails

dashboard vocabulary:

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
