# Storage And Ledger Design

Atelia Secretary には、複雑な database より先に local ledger が必要です。store は work を inspectable、replayable、必要に応じて redacted、そして client と agent が信頼できるものにします。

## Principles

- domain record は明示する
- event と audit record は append-friendly にする
- rendered tool output を source of truth にしない
- redaction は event の存在を消さない
- schema migration は versioned で記録する

## Store Shape

最初の store は SQLite、別の embedded database、file-backed append log のいずれでも構いません。architecture は次の logical collection を要求します。

| Collection | Record |
| --- | --- |
| `repositories` | trusted workspace roots |
| `jobs` | requested work |
| `job_events` | ordered lifecycle and observation events |
| `policy_decisions` | policy outcome before execution |
| `tool_invocations` | attempted built-in or extension tool calls |
| `tool_results` | canonical structured outputs |
| `audit_records` | durable policy and execution evidence |
| `schema_migrations` | applied storage migrations |

## Record Requirements

すべての record は次を含みます。

- id
- schema version
- created timestamp
- mutation が許される場合は updated timestamp
- redaction state

append-only record は in-place mutation ではなく superseding event を使います。

## Repository Records

repository record は次を含みます。

- display name
- root path
- allowed path scope
- trust state
- owner hint
- last observed metadata

blocked repository も store に残します。audit record と job history が解決できるためです。

## Job Records

job record は次を含みます。

- requester
- repository id
- kind
- goal
- status
- policy summary
- cancellation state
- timestamps
- latest event id

job status change は `job_event` record を作ります。

## Event Records

event は client と agent が work を理解するための timeline です。

event record は次を含みます。

- sequence number
- event kind
- subject type and id
- severity
- public message
- referenced ids
- redaction markers

event は stream できる程度に compact で、replay できる程度に complete であるべきです。

## Audit Records

audit record は「誰/何が action を要求し、policy が何を決め、何が実行され、どんな effect が起きたか」に答えます。

audit record は次を含みます。

- actor / requester
- repository id
- requested capability
- policy decision id
- tool invocation id, if any
- effect summary
- output refs
- redactions

audit record は append-only です。後から detail を redact する必要がある場合でも、original id と redaction reason を持つ redacted record を残します。

## Tool Results

tool result は canonical structured data を保存します。

- tool id
- invocation id
- status
- schema ref
- typed fields
- evidence refs
- truncation metadata
- redaction metadata

TOON、JSON、text rendering は derived view です。

## Migration Policy

storage migration は次を満たします。

- ordered
- 可能な限り idempotent
- `schema_migrations` に記録する
- `storage_status: migrating` を report できる
- 明示的に safe でない限り、job 実行中に migration しない

migration が失敗した場合、daemon は新しい work を黙って受けず、`degraded` または `read_only` state で起動します。

## Retention

初期 retention は保守的にします。

- repository、job、policy、audit record は indefinite に保持する
- large tool artifact は metadata tombstone を残す場合のみ expire できる
- client が recent work を replay できる event history を保持する
- security-relevant audit evidence は明示 policy なしに削除しない

## AX Check

ledger は agent の orientation を助けるべきです。

- 1つの job id から policy、event、tool call、output、audit record に辿れる
- event replay により、agent は restart 後も user に「何が起きたか」を聞かずに回復できる
- redaction marker は「data は存在したが意図的に隠された」と伝える
- canonical tool result により、format 変更のために work を rerun しなくて済む
