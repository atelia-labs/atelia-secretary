# Secretary Runtime Architecture

この文書は、Atelia Secretary の runtime architecture を定義します。daemon の protocol surface、domain record、policy boundary、audit model、tool execution、extension seam の実装上の基準点です。

この architecture は Atelia の MDP quality bar に従います。MDP は Minimum Desirable / Delightful Product です。Desirable は「その product がその人の生活や仕事に存在する理由があるか」を問い、Delightful は「使うときに丁寧さ、読みやすさ、生きた手触りがあるか」を問います。runtime は規律を持って作れるほど小さく、それでも product の土台として信頼できるほど完全であるべきです。

これは捨てる前提の first-loop sketch ではありません。後続の implementation slice で capability、storage engine、transport、extension host、hosted sync が追加されても、ここで定義する domain boundary は保ちます。

## Design Commitments

runtime architecture は次を約束します。

- implicit process state より explicit domain record
- append-friendly な event / audit history
- execution effect より前の policy decision
- rendered output より前の canonical tool result
- client-specific view state より前の typed protocol contract
- built-in service integration より前の provider boundary
- generic error より recoverable failure state

## Product Contract

daemon は、Atelia client、agent、local project workspace の間に立ちます。typed state を expose し、bounded work request を受け、policy を enforce し、起きたことを記録し、その境界を user にとって読めるものにします。

runtime は generic automation platform ではありません。Atelia を成立させる product boundary です。

- client は privileged work を直接実行しない
- work 実行前に Secretary から見える policy を確認する
- work はあとから inspect できる structured record を作る
- tool output は TOON / JSON rendering から独立した canonical result を持つ
- status、error、approval state が、単に技術的に正しいだけでなく理解できる
- full extension host に踏み込まず、extension point を予約する

## Runtime の形

daemon は長時間動作する Rust process です。

daemon が所有するもの:

- process lifecycle と health
- repository registration
- project / thread state snapshot
- job submission、observation、cancellation、completion
- job と tool invocation ごとの policy check
- policy decision と execution effect の audit record
- tool result record と output rendering metadata
- 小さな built-in tool surface

daemon が所有しないもの:

- Mac / iOS UI state
- 長期 personal memory
- 任意の GitHub / Linear / 外部サービス integration
- delegated cloud agent orchestration
- third-party extension installation

これらの surface は client または extension に寄せます。

## Protocol Surface

protocol は、現在の health endpoint から versioned service surface へ育てます。これらの group は、client と agent が依存する durable contract です。

必要な RPC group:

- `Health`: daemon status、daemon version、protocol version、storage state
- `RegisterRepository`: workspace root を宣言し repository id を返す
- `ListRepositories`: registered repository を inspect する
- `GetProjectStatus`: project、repository、job、policy summary を返す
- `SubmitJob`: bounded job request を作成する
- `GetJob`: 1つの job を inspect する
- `ListJobs`: filter 付きで recent jobs を inspect する
- `CancelJob`: cancellation を request する
- `WatchEvents`: job、policy、audit、repository event を stream する
- `CheckPolicy`: requested action が allowed / audited / approval-gated / blocked のどれかを事前確認する
- `RenderToolOutput`: canonical tool result を TOON、JSON、text として render する

初期 transport は gRPC または別の typed RPC transport で構いません。ただし domain contract は versioned かつ stable にします。

## Domain Records

daemon は明示的な domain record を永続化します。storage は simple local database や file-backed store から始めて構いませんが、record shape と append-friendly semantics は architecture の一部です。

| Record | Purpose | Notes |
| --- | --- | --- |
| repository | registered workspace root と trust setting | display name、root path、allowed path scope、created/updated timestamps を含む |
| job | user または agent が要求した仕事 | kind、goal、repository id、status、requester、created/started/completed timestamps を含む |
| job_event | observable job lifecycle event | append-only。streaming と replay を支える |
| policy_decision | allow / audit / approval / block の結果 | risk tier、reason、policy version、requested capability を含む |
| tool_invocation | built-in または extension tool call | tool id、input digest、permission、status、output ref を含む |
| tool_result | canonical structured result | rendered format から独立する |
| audit_record | durable execution / policy record | append-only。必要に応じて redaction する |

## State Machines

runtime は state transition を handler 内の ad hoc string としてではなく、明示的な model として扱います。

初期 job state:

- `queued`: accepted but not started
- `running`: execution has started
- `cancel_requested`: cancellation has been requested
- `succeeded`: completed without blocking errors
- `failed`: completed with an execution or validation failure
- `blocked`: policy or missing approval prevents execution
- `canceled`: cancellation completed

初期 policy outcome:

- `allowed`: 通常 record 以外の追加 audit なしで実行できる
- `audited`: 実行できるが audit evidence が必要
- `needs_approval`: approval path が available になるまで実行しない
- `blocked`: 実行しない

client-facing name は stable で、穏やかで、そのまま表示できるものにします。internal error detail はより詳しくて構いませんが、client に expose する failure は、具体的な reason と、可能な場合は recoverable next state を持つ必要があります。

## Built-In Tool Boundary

built-in は小さく、信頼できる状態に保ちます。

含めるもの:

- registered repository scope 内の filesystem read/list/search/stat/diff
- policy と audit の背後にある filesystem write/patch
- cwd、timeout、env allowlist を持つ explicit argv process execution
- policy check/request/status boundary
- output render/negotiate/schema

後回しにするもの:

- GitHub
- Linear
- cloud storage
- browser / computer use
- memory providers
- notification providers
- approval agents
- delegated agent providers

Git は bounded process tool 経由の shell/process 利用から始められます。より豊かな Git surface は後で official extension にできます。

## Policy Foundation

すべての job と tool invocation は、実行前に policy decision を受けます。

policy engine は次を扱います。

- R0 informational actions
- R1 bounded read actions
- R2 audited local changes
- R3 approval-gated actions
- R4 blocked actions

R3 と R4 は初期は保守的で構いません。approval flow が未実装なら、R3 は実行せず `needs_approval` を返します。

Policy decision は client から inspect でき、audit record に記録される必要があります。

## Tool Output Foundation

daemon は canonical tool result を保存し、それとは別に render します。

renderer は次を支えます。

- agent-facing output の default TOON
- integration / debugging 用 JSON
- request ごとの temporary format override
- tool ごとの default format metadata
- truncation metadata と redaction marker

output format を「raw log vs structured record」のように雑に比較しません。各 tool contract は field、order、omission、reference、redundancy を意図的に定義します。

## Extension Boundary

runtime architecture は、full extension installation の前から extension concept を予約します。

ただし domain と protocol naming では次の概念を予約します。

- tool provider
- service provider
- hook provider
- surface attachment
- permission request
- output customizer
- approval agent
- delegated agent provider
- OM / memory provider

最初の実装は built-in provider だけを expose して構いません。GitHub、Linear、memory、external agent を Secretary core に焼き込まないことを優先します。

## Deferred Product Surface

runtime architecture は次を後回しにします。

- full Rust / WASM extension runtime
- third-party extension registry
- extension bundles
- service-to-service extension calls
- `needs_approval` state を返す以上の human approval UI
- long-term memory / preference storage
- autonomous delegated agent scheduling
- hosted synchronization
- multi-user organization policy
- production secret vaulting

これらは重要ですが、runtime record と policy boundary の上に積みます。

## Implementation Slices

実装は、architecture を保ったまま次の slice で進めます。

1. protocol/storage version 付きの daemon lifecycle と health
2. repository registration と listing
3. job submission、listing、inspection、cancellation、state transition
4. event / audit persistence と replay
5. execution 前の policy decision record
6. bounded filesystem read/list/search/stat/diff
7. policy-gated filesystem write/patch
8. cwd、timeout、env allowlist を持つ explicit argv process execution
9. TOON / JSON rendering を持つ canonical tool result
10. 将来の extension boundary のための provider identifier 予約

## Acceptance Criteria

runtime architecture は、次を満たしたら実装済みです。

- daemon が起動し、protocol/storage version 付きで health を返す
- client が repository を register できる
- client が job を submit、list、inspect、cancel できる
- job lifecycle event を stream または replay できる
- execution 前に policy decision が記録される
- bounded filesystem / process tool が policy の下で実行される
- tool result が canonical に保存され、TOON または JSON として render できる
- audit record が requester、policy decision、tool effect、output ref を示す
- R3/R4 action が黙って実行されない
- よくある failure state が、具体的な user-facing reason と recoverable next state を返す
- event / job status name が穏やかで一貫しており、native client にそのまま表示できる
- docs と protocol definition の name / status value が一致している
