# Execution Semantics

execution semantics は、daemon が work を実行するとはどういうことかを定義します。job、tool、cancellation、timeout、effect を client と agent にとって predictable にします。

## Job Lifecycle

初期 lifecycle:

1. `SubmitJob` が request shape を validate する
2. `job: queued` を persist する
3. initial `job_event` を記録する
4. policy を evaluate する
5. `policy_decision` を記録する
6. allowed なら job を `running` に transition する
7. tool invocation が `tool_invocation` record を作る
8. tool output が `tool_result` record を作る
9. effect が audit record を作る
10. job が `succeeded`、`failed`、`blocked`、`canceled` のいずれかに transition する

policy evaluation は execution effect より前に行います。

implementation requirement は storage ledger が定義します。要点として、`job: queued` と initial `job_event` の永続化は1つの atomic store commit にし、`policy_decision` は、どの `tool_invocation`、`tool_result`、audit effect record より先に durable に記録します。

## Cancellation

`CancelJob` は cancellation を request します。即時停止を保証するものではありません。

cancellation state:

- `not_requested`
- `requested`
- `cooperative_stop`
- `force_stop`
- `completed`

tool は cooperative cancellation を support するか宣言します。process tool は force termination 前に graceful shutdown window を受けます。

## Timeouts

すべての job と tool invocation は timeout を持ちます。

- daemon policy 由来の default timeout
- request 由来の optional narrower timeout
- policy が enforce する maximum timeout

timeout は、daemon が cancellation を始めたかどうかに応じて `failed` または `canceled` state を作ります。

precedence は明示し、[Protocol Contract](protocol-contract.ja.md#event-ordering) の daemon store event ordering を使います。logical instant は daemon store が割り当てる monotonic event sequence number で決まります。cancellation と timeout が同じ logical instant を共有するのは、その2つの event が sequence 上で隣接し、かつ cancellation が先に現れる場合だけです。timeout を先に検出した場合、terminal state は `failed` です。後から来た cancellation request は、すでに emit された terminal state を上書きしません。timeout 前に始まった cooperative stop は `canceled`、timeout によって strict に発火した `force_stop` は `failed` です。

| Scenario | Precedence | Terminal state |
| --- | --- | --- |
| cancellation requested, then timeout fires | cancellation の event sequence が先 | `canceled` |
| cancellation and timeout share the same logical instant | 隣接 event sequence で cancellation が先 | `canceled` |
| timeout fires, then cancellation is requested | timeout を先に検出 | `failed` |
| cooperative stop begins, then timeout fires during grace period | cooperative stop が timeout より先に開始 | `canceled` |
| timeout triggers `force_stop` | timeout が strict に force stop を発火 | `failed` |

## Concurrency

daemon は保守的に始めます。

- repository ごとに mutating job は1つ
- bounded read job は store と policy が許す場合に複数可
- process execution は daemon concurrency budget で制限
- extension/provider concurrency は後で明示

concurrent job は、recorded policy と `lock_decision` なしに write を interleave してはいけません。`lock_decision` は `id`、`repository_id`、`policy_decision_id`、owner job/process id、locked path または repository scope、`locked_at`、`expires_at`、status を記録します。active lock は `(repository_id, locked_scope, active status)` で unique であり、同じ scope を保持できる mutating job は1つだけです。`policy_decision_id` は linkage metadata であり ownership key には含めません。reclaim は idempotent です。同じ `lock_decision.id` と owner job/process id に対する repeated reclaim は、最初の persisted reclaim 以後 no-op success を返します。restart recovery は、expired lock について `lock_decision.id`、owner id、`reclaimed_at` を持つ reclaim record を append した後にだけ、その lock を reclaimed と扱います。その job の execution state transition はこの record より前に始めてはいけません。reclaim persistence 後、daemon は `policy_decision_id` が参照する policy rule を再評価して次の action を決めます。

## Filesystem Scope

filesystem tool は execution 前に path を resolve します。

- normalize path
- 下の authoritative algorithm で path を resolve
- registered scope 外への traversal を reject
- 明示許可がない symlink escape を reject
- resolved path と、必要に応じて redacted display path を記録

authoritative path algorithm は canonicalization with verification です。daemon は requested path と repository root を absolute canonical path に resolve し、resolved path が canonical allowed root の下にない場合は reject します。そのうえで、[Storage And Ledger Design](storage-ledger.ja.md) が定義する tool invocation / audit record に resolved path と redacted display path を記録します。TOCTOU mitigation として、mutating tool は use 直前に target を open または stat し、可能なら final inode または platform equivalent を記録し、その final target が validated resolved path と一致しない場合は fail します。将来の platform-specific implementation は `openat` / dirfd chaining に置き換えて構いませんが、同じ visible record と rejection semantics を保つ必要があります。

write と patch は audit が必要です。

## Process Execution

process execution は次を要求します。

- explicit argv array
- registered repository scope 内の cwd
- env allowlist
- timeout
- stdout/stderr capture policy
- maximum output bytes
- exit status capture

stdout / stderr artifact は tool result と同じ扱いです。`tool_results` の truncation metadata と redaction marker を使い、大きな payload は output refs / evidence refs で参照できます。exit status と captured output はどちらも redaction rule の対象です。rendered output は、canonical result または audit policy が隠した data を漏らしてはいけません。

shell-string execution は default contract ではありません。shell execution を許す場合、それは higher risk の別 capability です。

## Tool Output

すべての tool invocation は次を作ります。

- invocation record
- canonical result
- optional artifact refs
- effect の audit record

large output は metadata 付きで truncation します。truncation は agent から見える必要があります。

## Target Architecture AX Check

この section は current な persisted runtime record model を説明します。具体的な Rust schema と store contract は
`crates/atelia-core/src/domain.rs` と `crates/atelia-core/src/store.rs` に既にあります。repo 内で動いている実装は in-memory store なので、durable な on-disk backend はまだ separate な follow-up です。

execution model により、agent は次に答えられるべきです。

- job は始まったか
- どの policy が許可または block したか
- どの tool が走ったか
- 何を変更したか
- canonical result はどこか
- retry、scope 縮小、approval request、stop のどれを選ぶべきか

record mapping:

- policy allow/block: `policy_decisions`
- どの tool が走り、canonical result がどこにあるか: `tool_invocations` と `tool_results`
- 何が変わったか: effect summary と output refs を持つ `audit_records`
- retry、scope reduction、approval、stop: `policy_decisions` と runtime failure taxonomy

record からこれに答えられないなら、runtime は十分に inspectable ではありません。
