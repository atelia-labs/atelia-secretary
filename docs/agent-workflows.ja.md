# Agent Workflows And AX Review

この文書は、agent 視点の realistic Secretary runtime workflow を確認します。protocol、tool、policy、output、ledger が、agent が推測なしに働くための文脈を与えるかを点検します。

## Workflow 1: Repository で向きを掴む

Goal: agent が Atelia workspace を開き、自分がどこにいるか理解する。

Calls:

1. `Health`
2. `ListRepositories`
3. workspace が未登録なら `RegisterRepository`
4. `GetProjectStatus`
5. latest known cursor から `WatchEvents`

Good AX:

- 1つの status call で daemon、repository、jobs、policy、storage summary がわかる
- registration は stable repository id を返す
- event stream により restart 後も continuity がある
- error が register、refresh、stop のどれをすべきか伝える

Bad AX to avoid:

- trust 確立前に agent に local file scan を要求する
- typed repository status ではなく logs だけ返す
- missing registration を daemon failure のように見せる

## Workflow 2: Bounded Read Task

Goal: stale protocol reference がないか docs を inspect する。

Calls:

1. `SubmitJob(kind: documentation_review, repository_id, goal)`
2. daemon records `job: queued`
3. `CheckPolicy(capability: filesystem.read, scope: docs)`
4. policy returns `allowed` or `audited`
5. tool invocation: `fs.search`
6. `RenderToolOutput(format: toon)`
7. `WatchEvents` observes job progress

Good AX:

- allowed の場合でも policy reason が見える
- search result が truncation metadata を含む
- canonical result を JSON として rerender できる
- job event からどの file を inspect したかがわかる

## Workflow 3: Local Patch Task

Goal: design document を更新する。

Calls:

1. `SubmitJob(kind: docs_patch, repository_id, goal)`
2. `CheckPolicy(capability: filesystem.write, scope: docs/runtime-architecture.md)`
3. policy returns `audited`
4. tool invocation: `fs.patch`
5. daemon records `tool_result`
6. daemon records `audit_record`
7. `WatchEvents` emits patch applied and job completed

Good AX:

- agent は patch 前に write が audited であることを理解する
- audit record が requester、policy、file path、output ref をつなぐ
- client は穏やかな "changed under audit" state を表示できる
- failure は scope 縮小 retry または conflict inspect を提案する

## Workflow 4: Process Execution

Goal: verification を走らせる。

Calls:

1. `CheckPolicy(capability: process.run, argv: ["cargo", "test"], cwd)`
2. policy returns `audited` or `needs_approval` depending on repo trust
3. tool invocation: `proc.run`
4. daemon captures exit status, stdout/stderr refs, truncation metadata
5. `RenderToolOutput(format: toon)`

Good AX:

- argv は shell string ではなく structured
- timeout と env allowlist が見える
- output truncation が omitted content を伝える
- nonzero exit が stable error と next state に map される

## Workflow 5: Approval-Gated Action

Goal: broad mutation または external side effect を実行したい。

Calls:

1. `CheckPolicy(capability: repository.merge or external.call)`
2. policy returns `needs_approval`
3. daemon records approval request and audit ref
4. client displays approval state
5. agent waits or asks the human with the user-facing reason

Good AX:

- agent は approval message を即興で作らなくてよい
- human は scope、expected effect、risk tier を見られる
- approval UI が unavailable でも、runtime は実行せず stable `needs_approval` state を返す

## Workflow 6: Disconnect 後の回復

Goal: agent が event stream loss 後に reconnect する。

Calls:

1. `WatchEvents(cursor: last_seen)`
2. cursor valid なら daemon が missed events を stream
3. cursor expired なら daemon returns `CURSOR_EXPIRED`
4. agent calls `GetProjectStatus`
5. agent resumes from latest event id

Good AX:

- reconnect は日常的な操作で、怖いものではない
- agent は user に「何が起きましたか」と聞かずに回復できる
- event gap が明示される

## Current Skeleton Check

現在の repository には次があります。

- `proto/atelia/v1/secretary.proto` には `SecretaryService.Health` のみ
- `ateliad` skeleton は起動し、policy state を log し、Ctrl-C を待つ
- `atelia-core` primitive は `ProjectId`、`PolicyState`、AX feedback を持つ

local timed execution では、skeleton が起動し、RPC server はまだ未配線であることを report するところまで確認できます。

方向確認には十分ですが、proto を ad hoc に広げるべきではありません。protocol contract、storage、policy、error、execution shape を先に置き、その後 implementation slice 順に実装します。

## AX Review Checklist

runtime tool を実装または変更する前に確認します。

- agent は 1つの orienting call で自分の state を理解できるか
- すべての execution effect に preceding policy decision があるか
- すべての output に rendering 前の canonical result があるか
- interruption 後に agent は recover できるか
- human は approval が必要な理由を見られるか
- failure state は next action を選べるほど具体的か
- tool は繰り返し呼びやすいか、それとも log archaeology を強いるか
