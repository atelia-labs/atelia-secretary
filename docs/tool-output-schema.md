# Tool Output Schema

This document defines how Atelia Secretary returns tool execution results to
Secretary and supporting agents.

Tool output is part of the workplace for AI agents. Poor structure, noisy
fields, vague keys, and bad ordering directly harm AX.

## Policy

Atelia treats TOON as the first agent-facing tool output format.

TOON is Token-Oriented Object Notation. It is a compact representation for
passing structured data to LLMs, especially useful for object arrays and tabular
data where JSON repeats the same keys many times.

Secretary can still switch between TOON and JSON from settings.

- global default format
- per-tool default format
- per-call temporary override
- client capability fallback
- debug / audit / artifact formats

This configurability is AX customization. It lets Secretary shape its own
workplace.

## Canonical Result Model

The internal canonical result is independent of rendering format. TOON, JSON,
and text are rendering choices over the same domain model.

Minimum model:

- `id`
- `tool`
- `call_id`
- `run_id`
- `schema_version`
- `format`
- `status`: `ok`, `partial`, `failed`, `blocked`, `needs_approval`,
  `cancelled`, `timeout`, `unavailable`
- `summary`
- `reason`: short structured reason for unavailable / blocked / failed states
- `tool_code`: stable tool-specific code when applicable
- `evidence`
- `artifacts`
- `actions`: possible next actions and recovery affordances
- `policy`
- `diagnostics`
- `cost`
- `audit_ref`

`summary` gives Secretary a short entry point for the next inspection.

## Result Channels

Tool execution produces multiple channels.

| Channel | Purpose | Consumers | Format |
| --- | --- | --- | --- |
| primary | immediate judgment material | Secretary, agents | TOON default, JSON configurable |
| evidence | material used for judgment | Secretary, agents, clients | canonical structured data |
| artifact | large payloads, diffs, logs, traces, reports | clients, storage | original or stored format |
| audit | complete execution record, policy, approval, redaction | daemon, security, review | JSON / JSONL |
| diagnostic | tool health, parser error, retry hints | Secretary, maintainers | structured |
| analytics | AX improvement events | AX improvement flow | privacy-filtered events |

Large stdout, stderr, diffs, and traces belong in artifacts. The primary result
contains references, summaries, omitted ranges, and retrieval affordances.

## Field Design

Every field should answer these questions.

- Does Secretary need it for the next judgment?
- Does it belong in audit instead?
- Is an artifact reference enough?
- Does it duplicate another field?
- Would omission make meaning ambiguous?
- Is the token cost justified?
- Do agents repeatedly ask for it?

Field order is AX. High-signal fields come first.

Standard order:

1. `status`
2. `summary`
3. identity / scope: `tool`, `repo`, `path`, `issue`, `branch`
4. policy / approval: `policy`, `needs_approval`, `blocked_reason`
5. evidence / changed state
6. artifacts
7. diagnostics
8. cost / timing
9. audit refs

## Key Naming

Agent-facing keys use English `snake_case` by default.

- durable id: `_id`
- retrievable handle: `_ref`
- timestamp: `_at`
- duration: `_ms`
- byte count: `_bytes`
- token estimate: `_tokens`

Avoid vague keys:

- `data`
- `result`
- `info`
- `content`
- `misc`

## TOON Rendering

TOON rendering is generated from the primary and evidence parts of the
canonical result.

Rules:

- put `schema_version` and `format toon` near the top
- put `status` first
- render repeated homogeneous objects as tables where practical
- omit empty / default / null fields
- move long text to artifact refs
- always include error / blocked / approval state
- always keep policy and critical errors intact
- include `redactions` when redaction happened

Example:

```toon
schema_version tool_result.v1
format toon
status partial
tool git_status
repo atelia-secretary
branch main
summary 2 modified docs, 1 untracked file
changed_files[3]{path,status}
  docs/tool-output-schema.ja.md,modified
  docs/research/agent-harness-survey.ja.md,modified
  docs/extensions-runtime.ja.md,untracked
artifacts
  diff_ref artifact:diff:8f13
policy
  state allowed_with_audit
audit_ref audit:tool-call:01h
```

## JSON Rendering

JSON is another format Secretary can choose.

Useful cases:

- external integrations
- debug
- client / protocol compatibility
- deeply nested irregular data
- machine-to-machine handoff
- payloads that need JSON for safe representation

## Schema Compatibility

`tool_result.v1` is a public implementation contract once extensions, clients,
or golden fixtures depend on it.

Compatibility rules:

- additive optional fields are compatible
- removed or renamed fields require a new schema version
- field meaning changes require a new schema version
- status and `tool_code` additions need documented fallback behavior
- deprecated fields remain through at least one minor release line
- tool-output customizers declare supported schema ranges
- unsupported customizer versions fall back to canonical Secretary output
- TOON and JSON renderers share the same canonical schema version
- default format changes are release-note-visible behavior changes

## Schema Negotiation

Schema resolution order:

1. supported schema range declared by tool-output customizer or client
2. newest schema the daemon can produce safely
3. compatible fallback schema
4. canonical Secretary output without customizer involvement

Fallbacks keep `status`, `reason`, `tool_code`, `policy`, `blocked_reason`,
`needs_approval`, `redactions`, `critical_errors`, and `audit_ref`.

## Format Negotiation

Format resolution order:

1. per-call override
2. per-tool default
3. Secretary global default
4. client capability
5. safe fallback

When the requested format is unavailable, the tool result returns a safe
fallback with the reason.

```toon
status partial
format json
requested_format toon
format_fallback_reason deeply_nested_payload
artifact_ref artifact:full-result:91a
```

## Secretary Preferences

Secretary can keep preferences for tool output.

- `default_result_format`: `toon` / `json`
- `per_tool_format`
- `verbosity`: `minimal`, `normal`, `expanded`, `debug`
- `language_mode`: `user`, `english_agent`, `mixed`
- `include_policy`
- `include_cost`
- `include_diagnostics`
- `artifact_threshold`
- `truncation_strategy`
- `evidence_order`
- `risk_sensitivity`

Preferences are workplace ergonomics.

These preferences express Secretary's professional judgment about its workplace.
Clients, extensions, and supporting agents can propose changes; Secretary owns
the persistent preference.

## Recovery Contract

`reason`, `tool_code`, and `actions` guide recovery.

- `INVALID_PARAMS`: fix input and retry
- `UNAVAILABLE`: inspect `reason` and follow an authentication, installation,
  or dependency action
- `PERMISSION`: request capability or adjust scope
- `POLICY_BLOCKED`: inspect policy and approval path
- `NEEDS_APPROVAL`: surface approval request
- `TIMEOUT`: retry with narrower scope or longer timeout when policy permits
- `CANCELLED`: preserve partial artifacts and task state
- `NOT_FOUND`: inspect scope, ids, and repository selection
- `CONFLICT`: refresh state before retry
- `PARSE_FAILED`: return canonical output and record renderer/customizer issue
- `INTERNAL`: preserve audit ref and diagnostic artifact

Recovery hints live in `actions`; detailed retry diagnostics live in the
diagnostic channel.

## Language And Token Efficiency

Direct user dialogue uses the user's language.

Agent-to-agent communication, tool output keys, and language-independent work
can prefer English when useful for token efficiency or task performance. The
choice remains adjustable by task, agent preference, and tool behavior.

## Truncation

When primary results are too large, Secretary truncates them while preserving:

- `status`
- `summary`
- `policy`
- `needs_approval`
- `blocked_reason`
- `critical_errors`
- `artifact_ref`
- `truncated true`
- `shown_count` / `total_count`
- retrieval affordances

## Error Output

Error output should be short and actionable. Stack traces go to diagnostic
artifacts.

```toon
status failed
tool cargo_test
summary tests failed
tool_code TEST_FAILED
failed_count 4
artifact_ref artifact:test-log:77b
audit_ref audit:tool-call:01k
```

Standard `tool_code` values:

- `INVALID_PARAMS`
- `UNAVAILABLE`
- `PERMISSION`
- `POLICY_BLOCKED`
- `NEEDS_APPROVAL`
- `TIMEOUT`
- `CANCELLED`
- `NOT_FOUND`
- `CONFLICT`
- `PARSE_FAILED`
- `INTERNAL`

## Analytics Feedback

Tool output schema improves through operational logs.

Track:

- selected format
- format override
- format fallback
- repeated tool calls
- parse failures
- missing-field follow-up
- unused fields
- artifact opens
- truncation rate
- token estimates
- human corrections
- AX Feedback links

The goal is better tool output for Secretary and agents.
