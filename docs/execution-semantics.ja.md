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

## Concurrency

daemon は保守的に始めます。

- repository ごとに mutating job は1つ
- bounded read job は store と policy が許す場合に複数可
- process execution は daemon concurrency budget で制限
- extension/provider concurrency は後で明示

concurrent job は、recorded policy と lock decision なしに write を interleave してはいけません。

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

record からこれに答えられないなら、runtime は十分に inspectable ではありません。
