# MDP Runtime Contract

この文書は、Atelia Secretary の最初の具体的な runtime contract を定義します。MDP は Minimum Desirable / Delightful Product、つまり「小さいが、欲しいと思える理由があり、信頼でき、見通せて、また使いたくなる最初の product」です。Desirable は「その product がその人の生活や仕事に存在する理由があるか」を問い、Delightful は「使うときに丁寧さ、読みやすさ、生きた手触りがあるか」を問います。単なる技術的最小限ではなく、最初の usable daemon loop に Atelia らしい手触りを持たせるための実装目標を置きます。

## 目的

MDP daemon は、Atelia client と local project workspace の間に立ちます。
typed state を expose し、bounded work request を受け、policy を enforce し、
起きたことを記録し、その境界が user にとって読める必要があります。

MDP はまだ general automation platform ではありません。Atelia の境界を証明しつつ、日常的に触っても雑に感じない最初の runtime です。

- client は privileged work を直接実行しない
- infrastructure の証明だけでなく、最初の loop が実在する user need を解く
- work 実行前に Secretary から見える policy を確認する
- work はあとから inspect できる structured record を作る
- tool output は TOON / JSON rendering から独立した canonical result を持つ
- status、error、approval state が、単に技術的に正しいだけでなく理解できる
- full extension host に踏み込まず、extension point を予約する

## Runtime の形

MDP daemon は長時間動作する Rust process です。

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

## 最初の Protocol Surface

最初の protocol は、現在の health endpoint から小さな versioned service surface へ育てます。

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

## Core Domain Records

daemon は最初に次の record を永続化します。

| Record | Purpose | Notes |
| --- | --- | --- |
| repository | registered workspace root と trust setting | display name、root path、allowed path scope、created/updated timestamps を含む |
| job | user または agent が要求した仕事 | kind、goal、repository id、status、requester、created/started/completed timestamps を含む |
| job_event | observable job lifecycle event | append-only。streaming と replay を支える |
| policy_decision | allow / audit / approval / block の結果 | risk tier、reason、policy version、requested capability を含む |
| tool_invocation | built-in または extension tool call | tool id、input digest、permission、status、output ref を含む |
| tool_result | canonical structured result | rendered format から独立する |
| audit_record | durable execution / policy record | append-only。必要に応じて redaction する |

MDP は simple local database や file-backed store で構いません。storage の選択より、record が明示的で append-friendly であることを優先します。

## Built-In Tool Boundary

MDP built-in は小さく、信頼できる状態に保ちます。

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

MDP policy engine は次を扱います。

- R0 informational actions
- R1 bounded read actions
- R2 audited local changes
- R3 approval-gated actions
- R4 blocked actions

R3 と R4 は初期は保守的で構いません。approval flow が未実装なら、R3 は実行せず `needs_approval` を返します。

Policy decision は client から inspect でき、audit record に記録される必要があります。

## Tool Output Foundation

daemon は canonical tool result を保存し、それとは別に render します。

MDP renderer は次を支えます。

- agent-facing output の default TOON
- integration / debugging 用 JSON
- request ごとの temporary format override
- tool ごとの default format metadata
- truncation metadata と redaction marker

output format を「raw log vs structured record」のように雑に比較しません。各 tool contract は field、order、omission、reference、redundancy を意図的に定義します。

## MDP で予約する Extension Boundary

MDP は full extension installation を必要としません。

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

## 対象外

MDP は次を含みません。

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

これらは重要ですが、MDP record と policy boundary の上に積みます。

## Acceptance Criteria

最初の MDP 実装は、次を満たしたら完了です。

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
