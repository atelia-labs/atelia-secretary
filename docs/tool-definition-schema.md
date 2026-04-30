# Tool Definition Schema

This document defines how Atelia Secretary describes tools before they become
callable by Secretary or supporting agents.

A tool definition is a workplace contract. It tells Secretary what the tool can
do, what authority it needs, what evidence it returns, how it fails, and how the
result can be shaped for AX.

## Minimum Definition

```yaml
schema: atelia.tool.v1
id: fs.search
name: File Search
version: 0.1.0
provider:
  kind: builtin | extension | remote | mcp | workflow
  id: atelia-secretary
description: Search files inside an allowed workspace scope
input_schema_ref: schema:fs.search.input.v1
output_schema_ref: schema:tool_result.v1
default_result_format: toon
supported_result_formats:
  - toon
  - json
risk: R1
permissions:
  - fs.read
effects:
  filesystem: read
  repository: none
  network: none
  secrets: none
idempotency: idempotent
streaming: false
cancellable: true
timeout_ms: 10000
artifact_policy:
  max_primary_tokens: 1200
  large_payload: artifact_ref
audit:
  required: true
  redaction: standard
failure:
  unavailable: structured
  retry: none
customization:
  format: secretary_preference
  field_order: allowed
  omission: allowed_with_required_fields
  language: allowed
  token_budget: allowed
```

## Identity

Tool ids are stable and namespaced:

- built-in: `fs.search`, `proc.run`, `job.status`
- extension: `extension.<extension_id>.<tool_id>`
- remote: `remote.<provider_id>.<tool_id>`
- workflow: `workflow.<workflow_id>.<tool_id>`

Renaming a tool id is a breaking change. Display names can change.

## Input Schema

Input schemas use typed fields and explicit defaults.

Rules:

- every field has a type
- optional fields declare defaults or absence behavior
- path fields declare scope and normalization
- network fields declare allowed hosts or capability references
- secret fields use secret handles, not raw values
- destructive fields require explicit booleans and policy checks
- free-form strings declare max length and redaction class

Example:

```yaml
schema: fs.search.input.v1
fields:
  root:
    type: workspace_path
    required: true
  query:
    type: string
    required: true
    max_length: 1024
  include:
    type: array<string>
    default: []
  max_hits:
    type: integer
    default: 50
    max: 500
```

## Built-In Tool Schemas

See [Tool Catalog](tool-catalog.md) for the built-in surface boundary.

Secretary built-ins are intentionally small. Each built-in tool must have a
definition using this schema before implementation.

Initial built-ins:

- `fs.read`
- `fs.list`
- `fs.search`
- `fs.stat`
- `fs.diff`
- `fs.write`
- `fs.patch`
- `proc.run`
- `proc.spawn`
- `proc.kill`
- `proc.status`
- `proc.stream`
- `search.files`
- `search.text`
- `search.symbols`
- `job.create`
- `job.status`
- `job.cancel`
- `job.events`
- `event.subscribe`
- `event.publish_internal`
- `event.ack`
- `policy.check`
- `approval.request`
- `approval.submit`
- `approval.status`
- `extension.install`
- `extension.update`
- `extension.remove`
- `extension.rollback`
- `extension.enable`
- `extension.disable`
- `extension.status`
- `bundle.install`
- `bundle.update`
- `bundle.remove`
- `bundle.rollback`
- `bundle.status`
- `service.call`
- `service.status`
- `service.schema`
- `hook.create`
- `hook.update`
- `hook.enable`
- `hook.disable`
- `webhook.receive`
- `schedule.create`
- `output.render`
- `output.negotiate`
- `output.preview`
- `output.schema`
- `agent.register`
- `agent.delegate`
- `agent.status`
- `agent.cancel`
- `agent.takeover`

Git helpers, GitHub, Linear, OM providers, memory, notification, review agents,
and approval agents are defined by extensions using the same schema.

The `approval.*` built-ins are boundary tools for submitting and verifying
decisions. Approval judgment itself comes from humans or Approval Agent
extensions.

## Output Schema

Every tool returns the canonical `tool_result.v1` envelope. Tool-specific
evidence appears under stable evidence records.

Tool definitions declare:

- primary result schema
- evidence record schemas
- artifact types
- status codes
- `tool_code` values
- required fields that renderers and customizers preserve
- default TOON field order
- JSON shape for integration/debug use

## Effects And Risk

Effects are declared separately from permissions.

Permissions express authority. Effects express what can change.

Effect dimensions:

- filesystem: `none`, `read`, `write`, `delete`
- repository: `none`, `read`, `branch`, `commit`, `merge`, `destructive`
- network: `none`, `read`, `write`, `webhook_receive`
- secrets: `none`, `named_read`
- memory: `none`, `read`, `write`
- service: `none`, `provide`, `call`
- notification: `none`, `send`
- workflow: `none`, `run`, `delegate`
- browser: `none`, `use`
- computer: `none`, `use`
- client: `none`, `view`, `action`, `settings`

Risk tier follows the highest declared effect and permission.

## Runtime Behavior

Tool definitions declare runtime behavior so Secretary can plan work safely.

- idempotency: `idempotent`, `repeatable`, `non_idempotent`
- cancellation: supported / unsupported
- streaming: supported / unsupported
- expected duration
- timeout
- concurrency limit
- checkpoint requirement
- rollback availability
- offline availability
- dependency requirements

Unavailable dependencies return `status unavailable` with `reason` and
`next_action`.

## Customization Surface

Tool definitions explicitly mark customizable result behavior.

Customizable:

- default result format
- per-call format override
- field order
- verbosity
- omission of optional fields
- summarization
- language
- token budget
- artifact threshold

Protected:

- `status`
- `tool`
- `schema_version`
- `policy`
- `needs_approval`
- `blocked_reason`
- `redactions`
- `critical_errors`
- `audit_ref`
- required evidence identifiers

Persistent defaults are owned by Secretary preferences. Supporting agents and
extensions can propose changes through visible preference updates.

## Extension Tools

Extension-provided tools declare the same schema as built-ins. The extension
manifest references each tool definition and the permissions needed to expose
it.

Secretary validates:

- manifest and tool definition compatibility
- declared permissions against effects
- supported output schema range
- required audit fields
- runtime availability
- provenance and extension version

If validation fails, the tool is unavailable with structured reason.
