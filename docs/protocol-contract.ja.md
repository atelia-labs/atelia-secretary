# Protocol Contract

この文書は、Atelia Secretary の最初の durable protocol contract を定義します。daemon runtime architecture、`atelia-kit`、native client の間の境界です。

beta contract は Rust RPC boundary では transport-neutral に保ちます。shipping している beta transport は HTTP/JSON で、proto/gRPC-generated の client / server path は future work であり、まだ ship していません。

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

この table は required service contract であり、現在の protobuf implementation だけの説明ではありません。現在の daemon は health、repository、job、policy、event replay、project status、tool-output settings、`RenderToolOutput`、read-only AEP backend package manifest validation を含む beta package-management RPC group をすでに expose しています。registry / blocklist operation は現時点では daemon の HTTP/JSON beta transport で expose されています。read-only の package trust index beta surface も HTTP/JSON ですでに ship されており、`ListPackageTrustIndex` として proto には同じ contract を予約しています。`ateliad` の Rust RPC boundary は transport-neutral のままにしてあり、将来の proto/gRPC client path が同じ contract に接続できるようにしています。`WatchEvents` は live beta subscription surface で、`ReplayEvents` と `/v1/events/replay` は bounded replay と compatibility のために残しています。

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
| `WatchEvents` | cursor から live ordered events を stream する |
| `ReplayEvents` | cursor から ordered events を replay する |
| `CheckPolicy` | requested action の policy outcome を preview する |
| `RenderToolOutput` | canonical tool result を TOON、JSON、text として render する |
| `ListPackageTrustIndex` | provenance と block marker 付きで package trust index を読む |
| `PackageInspect` | 1つの installed package について manifest、permissions、services、source、trust、block、rollback detail を inspect する |
| `ValidateExtension` | AEP backend package manifest を install せず registry state も mutate せずに validate する。beta RPC name は compatibility のため維持する |
| `InstallExtension` | new AEP backend package manifest を install する。beta RPC name は compatibility のため維持する |
| `UpdateExtension` | installed AEP backend package manifest を update する |
| `ExtensionStatus` | 1つの package installation と blocklist state を inspect する |
| `ListExtensions` | installed package status を list する |
| `RollbackExtension` | package の previous version を restore する |
| `DisableExtension` | installed package を disable する |
| `EnableExtension` | disabled package を enable する |
| `RemoveExtension` | installed package を remove する |
| `ApplyBlocklist` | blocklist entry を追加する |
| `ListBlocklist` | current blocklist を inspect する |

`ListRepertoire` は [Agent Repertoire](https://github.com/atelia-labs/atelia/blob/main/docs/agent-repertoire.ja.md) の最初の beta protocol slice です。現在の context にある live tool surface の beta `RepertoireEntry` projection を metadata とともに返し、persisted store は返しません。

Beta RPC group:

| RPC | Purpose |
| --- | --- |
| `ListRepertoire` | beta repertoire projection を inspect する |

最初の beta server surface は意図的に小さく、この beta slice で dispatch 可能な built-in Secretary tool を、すなわち `fs.delete`、`fs.diff`、`fs.list`、`fs.read`、`fs.search`、`fs.stat`、`secretary.echo` を beta repertoire entry として projection します。`fs.delete` は R2、他は R1。より広い built-in は将来の slice または runtime-backed slice に存在し得ますが、dispatch が存在するまでは `ListRepertoire` では claim しません。package-backed の repertoire entry は次の slice です。

beta slice では package management API は operator-facing です。RPC name は現在の beta wire surface として `Extension*` を維持しますが、docs と新しい product language では AEP backend package management として扱い、公開 package storefront として扱いません。package execution の RPC は beta では未対応です。endpoint が存在する場合でも、invoke されたら unsupported-capability を返さなければなりません。

## Core Messages

### Health

`HealthResponse` は次を含みます。

- daemon status: `starting`, `running`, `ready`, `degraded`, `stopping`
- daemon version
- protocol version
- storage version
- storage status: `ready`, `migrating`, `read_only`, `unavailable`
- optional beta state hint, field `beta_state`:
  - `scope`
  - `durability`
  - `restart_semantics`
  - `limits` はこの beta state に付随する制約を表す stable code token です
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
- goal（任意の bounded-job intent / summary。空または未指定なら durable な Goal object は作成しません）
- status
- policy summary
- created / started / completed timestamps
- latest event id (optional)
- cancellation details（`state` と必要に応じた cancellation の request / completion メタデータ）

`SubmitJobRequest.goal` は任意です。Secretary は空または未指定の goal を受け入れ、
`None` / absent として保持します。durable な `Goal` lifecycle と OM default package
policy は、この PR とは別の future product lane です。この job request が持つのは
あくまで optional な goal summary であり、first-class な goal lifecycle contract は
将来に予約されています。

`SubmitJob` は policy evaluation 前に work を execute してはいけません。最初の observable effect は persisted `job` と `job_event` です。
成功した submission は `idempotency_key` で replay でき、durable restart 後も有効です。failed submission は現在 replay result として cache されません。

`SubmitJobRequest.tool_args` は capability ごとに以下の形が必須です。

- `filesystem.search` / `fs.search`（読み取り）
  - 必須: `pattern`（文字列・空文字不可）
  - 任意: `max`（u64）
  - 非対応: `comparison_path`、`max_bytes`、`max_chars`
- `filesystem.diff` / `fs.diff`（読み取り）
  - 必須: `comparison_path`（文字列・空文字不可）
  - 任意: `max_bytes`（u64）、`max_chars`（u64）
  - 非対応: `pattern`、`max`
- その他の capability: `tool_args` は省略のみ許可されます。

例:

```json
{ "tool_args": { "pattern": "needle", "max": 10 } }
{ "tool_args": { "comparison_path": "right.txt", "max_bytes": 4096, "max_chars": 120 } }
```

非対応フィールドを含む `SubmitJob` は、実行前に却下されます。

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

`WatchEvents` は cursor を受け取り、その cursor に対する replay snapshot を返したあと、新しい ordered event を stream し続けます。reconnect guarantee は process-local です。つまり、同じ daemon process の中なら client は job history を失わずに live surface へ resume できます。daemon restart 後は、durable な最新状態を知るために `GetProjectStatus` を呼び、bounded replay-only の compatibility path が必要なら `ReplayEvents` を使います。

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

### Package Trust Index

`ListPackageTrustIndex` は read-only です。返す package projection には次が含まれます。

- `package_id`
- `version`
- `status`
- `boundary`
- `manifest_digest`
- `artifact_digest`
- lineage と publication を含む source snapshot
- block marker

record に既にある installed source / provenance / publication snapshot は保持します。install / update / execute の流れや audit / quarantine history は追加せず、approved permissions や rollback snapshots のような mutable install-only fields も意図的に含めません。

## Event Ordering

event ordering は daemon store 単位です。

- 各 event は monotonically increasing sequence number を持つ
- event は append-only
- retry は duplicate semantic effect を作らない
- client は last seen sequence number または event id で resume する

daemon が current process 内で continuity を保証できない場合、`WatchEvents` は `CURSOR_EXPIRED` recovery error を返し、client に `GetProjectStatus` を呼ぶよう伝えます。restart 後は、durable snapshot が必要なら `GetProjectStatus`、bounded replay-only semantics が必要なら `ReplayEvents` を使います。
replay/query で unknown event id を指定した request は `INVALID_REQUEST` のままです。live watch stream が continuity を失った場合のみ `WatchEvents` は `CURSOR_EXPIRED` を返します。page token や replay/query cursor syntax の不正は `INVALID_REQUEST` のままです。

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
- `WatchEvents` は job、policy、audit、repository change の live stream になる。compatibility 用に `ReplayEvents` も残す
- `RenderToolOutput` は format 変更のためだけに tool を rerun しなくて済む
- policy error は next state を持ち、agent が human に聞くべきか、scope を狭めて retry すべきか、止まるべきかを判断できる
