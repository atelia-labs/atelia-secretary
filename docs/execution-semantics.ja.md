# Execution Semantics

execution semantics は、daemon が work を実行するとはどういうことかを定義します。job、tool、cancellation、timeout、effect を client と agent にとって predictable にします。

## Job Lifecycle

初期 lifecycle:

1. `SubmitJob` が request shape を validate する
2. daemon が `job: queued` を persist する
3. daemon が initial `job_event` を記録する
4. daemon が policy を evaluate する
5. daemon が `policy_decision` を記録する
6. allowed なら job を `running` に transition する
7. tool invocation が `tool_invocation` record を作る
8. tool output が `tool_result` record を作る
9. effect が audit record を作る
10. job が `succeeded`、`failed`、`blocked`、`canceled` のいずれかに transition する

policy evaluation は execution effect より前に行います。

`job: queued` と initial `job_event` の永続化は、1つの atomic store commit にします。`policy_decision` は、どの `tool_invocation`、`tool_result`、audit effect record より先に durable に記録します。restart 時、daemon はこの境界を使って deterministic に recovery します。policy がなければ policy を再評価し、`tool_invocation` があり `tool_result` がない場合は、新しい work を続ける前に retry または cleanup 対象として mark します。

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

precedence は明示します。daemon が timeout より前、または同じ logical instant に cancellation を観測していた場合、cooperative cancellation を優先し terminal state は `canceled` です。timeout を先に検出した場合、terminal state は `failed` です。後から来た cancellation request は、すでに emit された terminal state を上書きしません。timeout 前に始まった cooperative stop は `canceled`、timeout によって strict に発火した `force_stop` は `failed` です。

## Concurrency

daemon は保守的に始めます。

- repository ごとに mutating job は1つ
- bounded read job は store と policy が許す場合に複数可
- process execution は daemon concurrency budget で制限
- extension/provider concurrency は後で明示

concurrent job は、recorded policy と `lock_decision` なしに write を interleave してはいけません。`lock_decision` は `id`、`repository_id`、`policy_decision_id`、owner job/process id、locked path または repository scope、`locked_at`、`expires_at`、status を記録します。ledger は execution 前に lock decision を永続化します。restart recovery は expired lock を stale と扱い、reclaim event を記録したうえで、継続前に policy を再評価します。

## Filesystem Scope

filesystem tool は execution 前に path を resolve します。

- normalize path
- registered scope 外への traversal を reject
- 明示許可がない symlink escape を reject
- resolved path と、必要に応じて redacted display path を記録

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

## AX Check

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
