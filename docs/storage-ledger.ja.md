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
| `lock_decisions` | durable repository/path mutual-exclusion decisions |
| `schema_migrations` | applied storage migrations |

## Record Requirements

すべての record は次を含みます。

- id
- schema version
- created timestamp
- mutation が許される場合は updated timestamp
- redaction state

append-only record は in-place mutation ではなく superseding event を使います。

## Atomicity And Ordering

backend は、少なくとも次のどちらかの storage primitive を提供します。

- single multi-row transaction
- fsync-equivalent durability を持つ atomic append / write-ahead-log commit

最初の lifecycle boundary では、`job: queued` と initial `job_event` を1つの atomic commit で永続化します。`policy_decision` は、どの `tool_invocation`、`tool_result`、audit effect record より先に durable commit される必要があります。restart 時、daemon はこの durable boundary を使って deterministic に recovery します。`policy_decision` がなければ policy evaluation を再実行し、`tool_invocation` があり `tool_result` がない場合は、その job の新しい work を受ける前に retry または cleanup 対象として mark します。

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

sequence number は daemon store ごとに strictly monotonic で、その store 内の replay に total order を与えます。

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

## Lock Decisions

`lock_decisions` は write exclusion を execution 前に記録します。

- repository id
- policy decision id
- owner job/process id
- locked path または repository scope
- `locked_at`
- `expires_at`
- reclaimed の場合は `reclaimed_at`
- status: `held`, `released`, `expired`, `reclaimed`

daemon は protected effect より前に lock decision を書き込みます。active lock は `(repository_id, locked_scope, active status)` で unique です。`policy_decision_id` は linkage metadata として保持し、同じ scope に2つの active lock を許す理由にしてはいけません。reclaim は同じ lock decision と owner に対して safe-repeatable です。duplicate reclaim attempt は、最初の persisted reclaim record 以後 no-op success を返します。restart 時、daemon は expired lock について `lock_decision.id`、owner id、`reclaimed_at` を持つ reclaim event を append します。その durable record が存在して初めて lock を reclaimed と扱えます。その後、`policy_decision_id` が参照する policy rule を再評価してから job を継続します。

## Migration Policy

storage migration は次を満たします。

- ordered
- 可能な限り idempotent
- `schema_migrations` に記録する
- `storage_status: migrating` を report できる
- 明示的に safe でない限り、job 実行中に migration しない

daemon は migration 前に、leader id、started timestamp、safe flag、timeout を持つ single migration lock を `schema_migrations` に記録します。running job は configured timeout まで drain します。drain できない場合、daemon は `degraded` または `read_only` に入り failure を記録します。non-leader daemon は `storage_status: migrating` を report し、migration lock が release されるまで新しい mutating work を受け付けません。

migration が失敗した場合、daemon は新しい work を黙って受けず、`degraded` または `read_only` state で起動します。

## Retention

初期 retention は保守的にします。

- repository、job、policy、lock、audit metadata は default で indefinite に保持する
- recent event history は default で少なくとも90日保持する
- large tool artifact は output ref、digest、truncation metadata、metadata tombstone を残す場合に限り、30日後に expire できる
- security-relevant audit evidence は、明示 compliance policy が sensitive field を redaction marker に置き換える場合を除き indefinite に保持する
- retention は data class ごとに configurable とし、override は policy version 付きで記録する

PII deletion request は、audit continuity が必要な場合、physical deletion より redaction を優先します。redaction record は id、timestamp、reason、actor、legal basis を保持します。reversible redaction を戻せるのは、指定された vault-backed process のみです。

## AX Check

ledger は agent の orientation を助けるべきです。

- 1つの job id から policy、event、tool call、output、audit record に辿れる
- event replay により、agent は restart 後も user に「何が起きたか」を聞かずに回復できる
- redaction marker は「data は存在したが意図的に隠された」と伝える
- canonical tool result により、format 変更のために work を rerun しなくて済む
