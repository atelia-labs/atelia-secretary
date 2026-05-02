# Error And Recovery Taxonomy

Errors are part of the product surface. Atelia Secretary errors must be stable
enough for clients and agents to recover without guessing.

## Error Shape

Every client-visible error includes:

- `code`
- `reason`
- `recoverable`
- `next_state`
- optional `retry_after`
- optional `details`
- optional `audit_ref`

`reason` is user-facing. `details` may be diagnostic and can be hidden by
clients unless needed.

## Error Codes

| Code | Meaning | `next_state` |
| --- | --- | --- |
| `INVALID_REQUEST` | request shape is invalid | `retry_same_request` after fixing request |
| `UNSUPPORTED_CAPABILITY` | daemon does not support requested capability | `refresh_status` |
| `NOT_FOUND` | id or scoped resource not found | `refresh_status` |
| `SCOPE_DENIED` | requested path/resource is outside scope | `retry_with_narrower_scope` |
| `POLICY_BLOCKED` | policy blocks action | `stop` |
| `NEEDS_APPROVAL` | approval required before execution | `request_approval` |
| `CONFLICT` | state changed or lock unavailable | `refresh_status` |
| `TIMEOUT` | execution exceeded timeout | `retry_with_narrower_scope` |
| `CANCELED` | job or tool was canceled | `inspect_audit_record` |
| `STORE_UNAVAILABLE` | local store unavailable | `wait_for_daemon` |
| `CURSOR_EXPIRED` | event cursor can no longer resume | `refresh_status` |
| `OUTPUT_TRUNCATED` | output is partial | `inspect_tool_result` |
| `INTERNAL` | unexpected daemon error | `inspect_audit_record` |

## Recovery States

Common `next_state` values:

- `refresh_status`
- `retry_same_request`
- `retry_with_narrower_scope`
- `request_approval`
- `inspect_audit_record`
- `inspect_tool_result`
- `wait_for_daemon`
- `stop`

## Client Display

Clients should be able to display:

- short title from code;
- human-readable reason;
- recommended next action;
- affected job/tool/repository;
- audit ref if available.

Do not expose stack traces as product copy.

## Agent Behavior

Agents should treat recovery states as instructions:

- `request_approval`: ask the human with the policy reason.
- `retry_with_narrower_scope`: propose a smaller path, command, or job.
- `refresh_status`: call `GetProjectStatus`.
- `inspect_tool_result`: call `RenderToolOutput`.
- `stop`: do not improvise around policy.

## AX Check

A good error keeps momentum without hiding risk. The agent should not need to
reverse-engineer daemon internals from logs. The human should not be asked a
vague question when the daemon already knows the blocked capability, scope, and
reason.
