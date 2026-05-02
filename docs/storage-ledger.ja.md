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

TOON（Token-Oriented Object Notation。canonical schema は [Tool Output Schema](tool-output-schema.ja.md)）、JSON、text rendering は derived view です。

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

daemon は migration 前に、leader id、started timestamp、safe flag、timeout を持つ single migration lock を `schema_migrations` に記録します。acquisition は unique key と compare-and-set または transactional insert を使い、unexpired `migration_lock` row がすでに存在する場合は失敗します。`storage_status: migrating` の間、leader は `safe_flag` の値に関係なく、新しい external mutating request を受け付けたり実行したりしません。leader が許可できるのは、既存 running job の drain と、`schema_migrations` / `migration_lock` / ledger に retry enqueue や cleanup を記録する internal housekeeping だけです。enqueue された retry は ledger entry として persist するだけで、migration lock が release または expire されるまでは実行しません。ただし `safe_flag: true` かつ operation が明示的に marked-safe な場合に限り、その marked-safe operation を実行できます。running job は configured timeout まで drain します。drain できない場合、retriable job は backoff 付き retry に enqueue し、non-retriable work は cleanup steps を ledger に記録したうえで `failed` に mark します。non-leader daemon は `storage_status: migrating` を report し、migration lock が release または expire されるまで新しい mutating work を受け付けません。

migration が失敗した場合、daemon は `storage_status: migrating` から `degraded` または `read_only` への transition と failure reason を `schema_migrations` と ledger に記録し、新しい work を黙って受けずに起動します。`running` job は configured timeout まで drain を継続するか、storage が安全に続行できない場合は paused として ledger に記録します。missing `tool_result` を待つ retriable work は backoff 付き retry として enqueue し、non-retriable work は cleanup steps を記録して `failed` に mark します。`queued` job は pending のまま保持し、`degraded` / `read_only` 中に新しい tool invocation や mutation work を開始しません。ただし `read_only` では bounded read/status/replay を許可でき、`degraded` では `safe_flag: true` かつ明示的に marked-safe な recovery housekeeping だけを許可できます。

## Retention

初期 retention は保守的にします。

- repository、job、policy、lock、audit metadata は default で indefinite に保持する
- recent event history は configured full-event retention window の間保持する
- minimal replay spine は default で indefinite に保持する。full event window が expire した後も何が起きたかを復元するために必要な key job、policy、lock、audit、output ref、digest、redaction、terminal-state event を含める
- large tool artifact は output ref、digest、truncation metadata、metadata tombstone を残す場合に限り、configured artifact retention window 後に expire できる
- security-relevant audit evidence は、明示 compliance policy が sensitive field を redaction marker に置き換える場合を除き indefinite に保持する
- retention は data class ごとに configurable とし、override は policy version 付きで記録する

PII deletion request は、audit continuity が必要な場合、physical deletion より redaction を優先します。redaction record は id、timestamp、reason、actor、legal basis を保持します。reversible redaction を戻せるのは、指定された vault-backed process のみです。

audit continuity が不要な場合、physical deletion または crypto-shredding は指定された vault-backed deletion process が PII deletion request の承認後 configured deletion window 内に必ず実行します。policy が non-continuity PII、user-requested hard deletion、revoked consent、secret material と分類した data には reversible redaction を許可しません。physical deletion 後は、configured deletion-proof retention window または policy-versioned override の間だけ minimal deletion record を保持できます。その record は id、timestamp、actor、legal basis、non-sensitive proof of deletion だけを含め、削除済み PII や reversible redaction material は含めません。reversible redaction を戻せるのは指定された vault-backed process のみであり、physical deletion または crypto-shredded data にはその経路はありません。

## AX Check

ledger は agent の orientation を助けるべきです。

- 1つの job id から policy、retained event spine、tool call、output、audit record に辿れる
- full event replay により、agent は configured full-event retention window 内で restart 後も回復できる。その window を過ぎた後も、minimal replay spine により user に「何が起きたか」を聞かずに要点を復元できる
- redaction marker は「data は存在したが意図的に隠された」と伝える
- canonical tool result により、format 変更のために work を rerun しなくて済む
