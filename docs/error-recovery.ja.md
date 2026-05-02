# Error And Recovery Taxonomy

error は product surface の一部です。Atelia Secretary の error は、client と agent が推測なしに recover できるほど stable である必要があります。

## Error Shape

client-visible error は次を含みます。

- `code`
- `reason`
- `recoverable`
- `next_state`
- optional `retry_after`
- optional `details`
- optional `audit_ref`

`reason` は user-facing です。`details` は diagnostic で、client は必要な場合だけ表示できます。

## Error Codes

| Code | Meaning | Recovery |
| --- | --- | --- |
| `INVALID_REQUEST` | request shape is invalid | fix request |
| `UNSUPPORTED_CAPABILITY` | daemon does not support requested capability | inspect capabilities |
| `NOT_FOUND` | id or scoped resource not found | refresh status / ids |
| `SCOPE_DENIED` | requested path/resource is outside scope | narrow scope |
| `POLICY_BLOCKED` | policy blocks action | stop or change request |
| `NEEDS_APPROVAL` | approval required before execution | ask human / wait |
| `CONFLICT` | state changed or lock unavailable | refresh and retry |
| `TIMEOUT` | execution exceeded timeout | retry narrower or longer if allowed |
| `CANCELED` | job or tool was canceled | inspect final events |
| `STORE_UNAVAILABLE` | local store unavailable | retry after daemon recovery |
| `CURSOR_EXPIRED` | event cursor can no longer resume | refresh project status |
| `OUTPUT_TRUNCATED` | output is partial | request artifact or narrower output |
| `INTERNAL` | unexpected daemon error | inspect audit / logs |

## Recovery States

common `next_state`:

- `refresh_status`
- `retry_same_request`
- `retry_with_narrower_scope`
- `request_approval`
- `inspect_audit_record`
- `inspect_tool_result`
- `wait_for_daemon`
- `stop`

## Client Display

client は次を表示できるべきです。

- code 由来の short title
- human-readable reason
- recommended next action
- affected job/tool/repository
- audit ref if available

stack trace を product copy として expose しません。

## Agent Behavior

agent は recovery state を instruction として扱います。

- `request_approval`: policy reason を添えて human に聞く
- `retry_with_narrower_scope`: より小さい path、command、job を提案する
- `refresh_status`: `GetProjectStatus` を呼ぶ
- `inspect_tool_result`: `RenderToolOutput` を呼ぶ
- `stop`: policy を迂回する即興をしない

## AX Check

良い error は risk を隠さずに momentum を保ちます。agent が log から daemon internal を reverse-engineer する必要があってはいけません。daemon が blocked capability、scope、reason を知っているのに、human に曖昧な質問を投げてはいけません。
