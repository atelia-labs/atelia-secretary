# Protocol Contract

この文書は、Atelia Secretary の最初の durable protocol contract を定義します。daemon runtime architecture、`atelia-kit`、native client の間の境界です。

protocol は退屈で、typed で、versioned で、event-friendly であるべきです。storage detail をそのまま expose しませんが、client と agent が起きたことを理解できる identity と audit reference は保ちます。

## Versioning

daemon state を表す response は次を含みます。

- `protocol_version`: semantic protocol contract version。string semver、例: `"1.0.0"`
- `daemon_version`: daemon implementation version。string semver、例: `"0.1.0"`
- `storage_version`: local store schema version。string semver、例: `"0.1.0"`
- `capabilities`: この daemon が持つ named protocol capability。string array、例: `["health.v1", "jobs.v1"]`

client は unknown enum value や unknown capability を fatal crash ではなく recoverable compatibility event として扱います。

## Identity

protocol id は stable prefix を持つ opaque string です。

| Prefix | Entity |
| --- | --- |
| `repo_` | repository |
| `job_` | job |
| `evt_` | event |
| `pol_` | policy decision |
| `lock_` | lock decision |
| `tool_` | tool invocation |
| `res_` | tool result |
| `aud_` | audit record |

id は user-facing copy ではありません。client は diagnostic metadata としてだけ短縮表示して構いません。

## Service Surface

この table は required service contract であり、現在の protobuf implementation の説明ではありません。現在の proto は `SecretaryService.Health` だけを expose している場合があります。他の RPC group は planned / required contract surface であり、client や agent が依存する前に追加します。

必要な RPC group:

| RPC | Purpose |
| --- | --- |
| `Health` | daemon availability と version を inspect する |
| `RegisterRepository` | trusted workspace root を追加または更新する |
| `ListRepositories` | registered repository を inspect する |
| `GetProjectStatus` | repository、job、policy、event state を summarize する |
| `SubmitJob` | bounded unit of work を作成する |
| `GetJob` | 1つの job を inspect する |
| `ListJobs` | filter 付きで recent jobs を inspect する |
| `CancelJob` | cancellation を request する |
| `WatchEvents` | cursor から ordered events を stream する |
| `CheckPolicy` | requested action の policy outcome を preview する |
| `RenderToolOutput` | canonical tool result を TOON、JSON、text として render する |

## Core Messages

### Health

`HealthResponse` は次を含みます。

- daemon status: `starting`, `ready`, `degraded`, `stopping`
- daemon version
- protocol version
- storage version
- storage status: `ready`, `migrating`, `read_only`, `unavailable`
- capability names

### Repository

`Repository` は次を含みます。

- repository id
- display name
- root path
- allowed path scope
- trust state: `trusted`, `read_only`, `blocked`
- created / updated timestamps

`RegisterRepository` は requested root が存在し、allowed local scope 内にあり、policy により block されていないことを validate します。

### Job

`Job` は次を含みます。

- job id
- repository id
- requester
- kind
- goal
- status
- policy summary
- created / started / completed timestamps
- latest event id
- cancellation state

`SubmitJob` は policy evaluation 前に work を execute してはいけません。最初の observable effect は persisted `job` と `job_event` です。

### Event

`Event` は次を含みます。

- event id
- sequence number
- timestamp
- subject type and id
- event kind
- severity
- public message
- job、policy decision、lock decision、tool invocation、tool result、audit record への ref

`WatchEvents` は cursor を受け取り、その cursor 以後の event を返します。client は reconnect 後に event を replay でき、job history を失いません。

### Policy Decision

`PolicyDecision` は次を含みます。

- decision id
- outcome: `allowed`, `audited`, `needs_approval`, `blocked`
- risk tier: `R0`, `R1`, `R2`, `R3`, `R4`
- requested capability
- reason code
- user-facing reason
- approval request ref, if any
- audit ref

### Tool Result

`ToolResultRef` は canonical structured output を指します。protocol は default では ref を返し、`RenderToolOutput` で TOON、JSON、text を選べるようにします。underlying result は変わりません。

## Event Ordering

event ordering は daemon store 単位です。

- 各 event は monotonically increasing sequence number を持つ
- event は append-only
- retry は duplicate semantic effect を作らない
- client は last seen sequence number または event id で resume する

daemon が continuity を保証できない場合、`WatchEvents` は `CURSOR_EXPIRED` recovery error を返し、client に `GetProjectStatus` を呼んで returned latest event から resume するよう伝えます。

## Error Shape

すべての RPC error は error taxonomy に map します。

- stable `code`
- user-facing `reason`
- `recoverable` boolean
- required `next_state`
- optional `retry_after`
- optional `audit_ref`

transport error も client に届く時点では、`next_state` を含む同じ shape に包みます。これにより recovery logic の一貫性を保ちます。

## AX Check

agent にとって protocol は repeated guessing を減らす必要があります。

- `GetProjectStatus` は compact な orienting call になる
- `SubmitJob` は job id と initial policy summary をすぐ返す
- `WatchEvents` は job、policy、audit、repository change の single stream になる
- `RenderToolOutput` は format 変更のためだけに tool を rerun しなくて済む
- policy error は next state を持ち、agent が human に聞くべきか、scope を狭めて retry すべきか、止まるべきかを判断できる
