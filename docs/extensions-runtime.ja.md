# AEP Package Runtime

この文書は、Atelia Secretary が AEP backend package をどのように検証、導入、監査、rollback、quarantine、revocation、管理するかを定義します。package の実行は beta では無効で、将来のリリース向けです。

規範的な AEP、package authoring、Surface Protocol、service、registry、hook、tool-output、broker-boundary contract は [`atelia`](https://github.com/atelia-labs/atelia/blob/main/README.ja.md) repository が所有します。[Package Authoring, Remix, and Discovery](https://github.com/atelia-labs/atelia/blob/main/docs/package-authoring-discovery.ja.md)、[Package Sharing and Source Policy](https://github.com/atelia-labs/atelia/blob/main/docs/package-sharing-source-policy.ja.md)、[AEP Manifest](https://github.com/atelia-labs/atelia/blob/main/docs/aep-manifest.ja.md)、[AEP Services](https://github.com/atelia-labs/atelia/blob/main/docs/aep-services.ja.md)、[Surface Protocol](https://github.com/atelia-labs/atelia/blob/main/docs/surface-protocol.ja.md)、[AEP Registry](https://github.com/atelia-labs/atelia/blob/main/docs/aep-registry.ja.md)、[Broker Boundary](https://github.com/atelia-labs/atelia/blob/main/docs/broker-boundary.ja.md)、[Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.ja.md)、[Tool Output](https://github.com/atelia-labs/atelia/blob/main/docs/tool-output.ja.md) を参照します。この文書は、Secretary が信頼できる package workplace を持つための daemon-side enforcement を定義します。AEP において、これは backend host reference implementation slice です。

Atelia は user-owned harness であり、公開 storefront や native app distribution channel ではありません。Secretary が gate するのは registry submission、searchability、installability、mount eligibility、quarantine、revocation、service execution、policy、audit、rollback です。Atelia 外での raw GitHub publication は gate しません。

## Package Kinds

package は複数の kind を宣言できます。kind が増えるほど、表示される permission surface も増えます。

- `tool`: Secretary / agent に callable tool を追加する
- `service`: 他の package が Secretary 経由で呼べる typed service を追加する
- `tool_output_customizer`: tool result の format、field order、省略、summary、TOON/JSON default を調整する
- `hook_provider`: package-created hook を登録する
- `webhook_receiver`: external event endpoint と verification rule を持つ
- `workflow`: bounded multi-step job を実行する
- `notification`: notification を送信または整形する
- `memory_provider`: scoped workplace memory または preference surface を提供する
- `memory_strategy`: raw messages と compressed memory をどのように維持し、agent context に渡すかを制御する
- `approval_agent`: approval request を review し、bounded approval decision を提出する
- `review`: review、evidence、critique、policy check に参加する
- `review_agent`: code、document、workflow を review する同僚エージェントとして働く
- `agent_provider`: Codex、Claude、Devin、Jules、CodeRabbit など外部 agent system に接続する
- `delegated_agent`: Secretary が仕事を委譲できる bounded colleague / subordinate agent を追加する
- `presentation`: human / agent facing 表示を変える
- `integration`: GitHub、Linear、Slack など外部サービスへ接続する

Semantic presentation は AEP package が宣言し、Atelia Mac / iOS のような presentation host が描画します。Secretary は、それらの host が表示すべき backend audit、permission、provenance fact を保持します。

## Manifest

Secretary は現在 backend-only compatibility slice を validate します。public AEP package schema は `aep.package.v0` です。この daemon slice は実装が追いつくまで `atelia.extension.v1` です。ただし product / docs language では generic extension ではなく AEP package として扱います。`failure.degrade: disable_extension` のような schema enum value も、compatibility slice が移行するまでは beta schema vocabulary を保持します。

```yaml
schema: atelia.extension.v1
id: com.example.package
name: Example Package
version: 1.2.3
publisher:
  name: Example Org
  url: https://example.com
description: Short purpose
types:
  - tool
  - hook_provider
compatibility:
  atelia_protocol: ">=0.1 <0.3"
  atelia_secretary: ">=0.1 <0.2"
entrypoints:
  realm: backend
  runtime: wasm-rust | wasm | process
  command: null
  image: null
  wasm: null
  protocol: atelia-extension-rpc.v1
permissions: {}
tools: []
services:
  provides: []
  consumes: []
tool_output: []
hooks: []
webhooks: []
composition:
  attachments: []
bundle:
  id: null
  required: null
failure:
  degrade: disable_extension | disable_feature | return_unavailable | require_human
  retry_policy: none | bounded
provenance:
  source: github | registry | local
  repository: null
  commit: null
  registry_identity: null
  artifact_digest: sha256:...
  manifest_digest: sha256:...
  signature: null
  signer: null
migration:
  from: []
  notes: null
```

`process` は local-development only であり、明示的に有効化された場合だけ使えます。Docker と remote runtime profile は future または special-purpose AEP profile であり、この beta backend slice には含めません。downloaded native UI、JavaScript、WebView code、dynamic loader、direct native API access、native client executable extension は Secretary backend host の launch path ではありません。

これらの additive section は daemon の manifest model では空の collection か null の note として default されるため、古い manifest も問題なく deserialize できます。validation は、manifest が実際に対応する content を宣言した場合にだけ section を要求し、legacy install を壊さずに new manifest の section/type mismatch だけを止めます。

manifest は enforceable contract です。runtime behavior が manifest を超える場合、Secretary は実行を block します。

初期の backend enforcement は `atelia-core::extensions` の
`ExtensionManifest` validation と in-memory `ExtensionRegistry` として実装します。
この first slice は backend `wasm-rust` / `wasm` manifest、明示的な
local-development process manifest、version ごとの provenance digest、
blocklist check、rollback pointer、brokered service-call authorization を扱います。
WASM execution はまだ実装しません。

現在の Rust identifier と RPC name は beta compatibility のため `extension` を残します。これは実装 vocabulary であり、product positioning ではありません。

初期実装では具体的な id boundary も予約します。

- official backend package は `ai.atelia.*` namespace と `atelia-official`
  registry identity を使う
- local development package は `local.*` namespace を使い、unsigned の場合は
  明示的な unsigned dev-mode approval を必要とする
- third-party package は official / local namespace を使えない

この beta slice では、package management API を operator-facing client に
提供します。execution を伴う request は意図的に未実装であり、黙って実行した
ふりをせず、structured unsupported-capability error を返します。

この slice では package status と metadata を inspect する resolver client 向けに
read-only の package trust index beta surface も公開します。installed package を
blocked でも隠さず投影し、
record にすでにある source / provenance / publication snapshot を保持し、
approved permissions や rollback snapshot のような mutable install-only fields
は意図的に含めません。publisher、compatibility、broker-family、
content-risk、data-disclosure summary は install record にまだ永続化されて
いないため、後続の manifest-summary projection が必要です。

HTTP/JSON beta transport はこの surface をすでに `ListPackageTrustIndex` として
ship しており、proto でも同じ read-only contract を予約しています。含まれるのは
`package_id`、`version`、`status`、`boundary`、`manifest_digest`、
`artifact_digest`、source snapshot、lineage、publication、block marker です。
install / update / execute の flow や audit / quarantine history までは広げません。

同じ HTTP/JSON beta transport は、client の detail surface 向けに
`POST /v1/packages/{id}/inspect` の `PackageInspect` も公開します。
`ListPackageTrustIndex` と異なり、inspect は 1つの installed package について
active manifest、approved permissions、service declarations、source snapshot、
publication trust、block match、rollback availability / snapshot を返します。
ただし read-only であり、package の install、update、execute、audit、
quarantine は行いません。

## Permission And Capability

permission と capability を分けます。

- `permission`: install / update 時に承認される権限
- `capability`: 1回の実行に対して Secretary が grant する具体的な権限

例:

- `repo.read`
- `repo.write`
- `repo.branch.create`
- `repo.pr.create`
- `repo.merge`
- `repo.destructive`
- `secret.read:name`
- `network.none`
- `network.allowlist`
- `network.github`
- `network.connect:host`
- `network.webhook.receive:source`
- `memory.read:scope`
- `memory.write:scope`
- `service.provide:name`
- `service.call:name`
- `notification.send:channel`
- `workflow.run`
- `workflow.run:workflow_id`
- `workflow.schedule`
- `workflow.delegate_agent`
- `review.comment`
- `review.approve`
- `review.request_changes`
- `hooks.create`
- `hooks.modify`
- `hooks.receive_external:source`
- `tool_output.customize:tool_id`
- `client.view.provide`
- `client.action.run`
- `client.settings.modify`
- `browser.use`
- `computer.use`

capability grant は、expiry、invocation id、actor、package version、requested operation、input digest、policy decision、max effect を持ちます。

default lifecycle:

- R0/R1 grant は task または session boundary で expire できます。
- R2 grant は、workflow が short-lived task grant を明示的に保持する場合を除き、invocation boundary で expire します。
- R3/R4 grant は invocation boundary で expire し、approval ref を持ちます。
- grant renewal は新しい policy decision を作ります。
- revocation は package disable、blocklist hit、manifest mismatch、approval withdrawal、permission change、version change、policy update、task cancellation で発生します。

## Tool Output Customization

tool output customization は security-sensitive な AEP package capability です。

Rules:

- raw observation は immutable audit / evidence に残す
- customizer は agent-facing rendering を変換できる
- customizer は必要な scoped data だけを受け取る
- error / blocked / needs approval は可視状態を保つ
- customizer が失敗した場合、Secretary canonical output に戻る
- transformed output digest と customizer identity を audit に残す

customizable:

- format: TOON / JSON
- field order
- omission
- redaction
- summarization
- token budget
- language
- per-tool default
- verbosity

## Service Broker

service call は Secretary が broker します。package 同士が HTTP、local socket、hidden process channel、client-side shortcut で直接通信することは許可しません。

Secretary は service call ごとに次を検証します。

- caller package id / component / version
- callee package id / component / version
- service name、method、schema version
- caller の `services.consumes` declaration
- callee の `services.provides` declaration
- provider-owned `provides.required_permissions`
- consumer-requested `consumes.grants`
- policy 由来の runtime capability grant
- input digest、output digest、redaction、failure

permission authority は provider-owned です。service permission の canonical name、risk tier、description は provider package 側にあります。consumer は `consumes.grants` でそれらを参照するだけで、自分の package vocabulary で再定義したり、provider の risk tier を下げたりできません。grant が provider の `provides.required_permissions` に一致しない場合、provider definition が approved version range 外で変わった場合、consumer が self-authorize しようとした場合、Secretary は install または execution を reject します。

同じ bundle 内の service call でも、個別の provide / consume declaration と permission が必要です。bundle install flow は承認 UI をまとめられますが、権限を自動昇格しません。

service dependency が欠けている場合、Secretary は package を partially unavailable または blocked composition として扱い、structured unavailable status を返します。

すべての service execution は Policy Engine、Service Broker、Audit Log を通ります。broker は resolver-issued correlation id、replay rejection、consent、rate limit、schema version、redaction、fallback behavior、audit event を enforce します。Secretary runtime が Secretary-side service target を host する場合でも、broker を bypass しません。

## Hook And Webhook Execution

hook は persisted object です。

```yaml
hook_id: hk_...
created_by:
  kind: user | package | automation
  package_id: null
trigger:
  source: atelia | github | external
  event: pull_request.opened
  condition: null
verification:
  method: hmac | github_signature | oidc | none_for_local_only
required_capabilities: []
action:
  kind: workflow | tool | notification | memory_update | package_action
status: enabled | disabled | blocked | needs_approval
```

Rules:

- user-created hook と package-created hook は区別して表示する
- package-created hook は protected object として扱う
- user は package-created hook を disable / block できる
- behavior を変える edit は fork または package update として扱う
- trigger、event source、verification、permission の変更は再承認を要求する
- external webhook は signature、timestamp window、delivery id dedupe、source allowlist を要求する
- hook execution は input digest、policy decision、state changes、failure reason、block reason を記録する

Secretary は hook が Secretary の判断を黙って置き換えないよう、inspect / pause / disable / reroute の権利を持ちます。

## Provenance And Signature

install / update 前に version ごとの provenance を検証します。

- artifact digest
- manifest digest
- source repository
- commit / tag
- registry identity
- signer identity
- signature over manifest and artifact digest
- build provenance when available

同じ version で digest が変わった package は block します。local unsigned package は dev-mode として明示承認を必要とし、client でも区別して表示します。

## Runtime And Sandbox

Beta backend runtime matrix:

- WASM (Rust): backend package の第一級 runtime
- WASM (non-Rust): reserved / special-purpose。より厳格な review、capability 制限、provenance check を要求する
- process: local-development only。明示的に有効化された場合だけ使える

Reserved future or special-purpose AEP runtime profiles:

- Docker: future または special-purpose。WASM に向かない重い処理を扱う場合でも host policy を強く要求する
- remote: future / explicitly permissioned runtime
- native client executable extension / Swift client: Secretary の backend runtime ではなく、launch path には含めない

Secretary-side backend package は Rust -> WASM を primary target とします。Official backend package は原則 Rust で書きます。Community backend package も初期は Rust / WASM を推奨または要求します。

Atelia は任意の WASM を本質的に安全とは扱いません。Rust は typed SDK と memory-safe default を提供し、WASM は controlled host capabilities、linear memory isolation、fuel / timeout、auditable imports の境界を提供します。非 Rust WASM、process、Docker、remote profile はより厳格な review、capability 制限、provenance check、runtime policy を必要とします。Swift client は Atelia Mac / iOS presentation host 側の profile であり、Secretary beta backend matrix には含めません。

Defaults:

- host Docker socket は mount しない
- ambient filesystem / network / env / credentials / repo access は与えない
- repository は read-only mount が default
- `repo.write` が grant された場合のみ write access を与える
- secrets は scoped short-lived handle として渡す
- CPU、memory、wall-clock、output-size、network egress を制限する

required CLI / API key がない場合は structured unavailable status を返します。

## AEP Bundles

Secretary は `aep.bundle.v0` manifest を、複数 AEP package の install / update / rollback をまとめる meta unit として扱えます。Bundle は1つの package の backend、presentation、resources をまとめる仕組みではありません。それらは1つの multi-component AEP package に属します。

bundle record:

- bundle id / version
- included package ids and version ranges
- required / optional status
- package ごとの approved permission diff
- service dependency graph
- AEP profile / Atelia Protocol / Secretary / client compatibility range
- bundle provenance and signature
- rollback snapshot

Bundle install、update、rollback は atomic であるべきです。required package が失敗した場合、bundle は install されません。optional package が失敗した場合、package manifest の degrade / fallback behavior に従います。

複数 bundle が同じ package を含む場合、Secretary は package install を共有できます。ただし、version range、permission diff、service dependency、composition attachment の互換性を検査します。同じ bundle に含まれていても service permission や capability grant は自動昇格しません。

## Install, Update, Rollback

Secretary は install record を durable state として保存します。

- installed version
- approved manifest digest
- approved permissions
- runtime artifact digest
- previous version
- migration state
- rollback snapshot

permission、hook、webhook、runtime、source、trigger condition が変わる update は再承認を必要とします。

rollback は previous version、disabled hooks、removed capabilities、package-owned state を復元します。migration は dry-run、touched data area、rollback note を持ちます。

rollback state:

1. `installed`
2. `updating`
3. `quiescing_running_jobs`
4. `rollback_in_progress`
5. `installed_previous_version`

in-flight package job は、cancellation が supported の場合 cancel します。external side effect を持つ job は review のため quarantine します。rollback 中に届いた hook delivery は、previous version の restore または package block が完了するまで hold します。

## Blocklist

blocklist は常に local enablement より優先します。

block keys:

- package id
- version range
- artifact digest
- signer
- publisher
- source repository
- permission pattern
- vulnerability id

block reasons:

- `malware`
- `manifest_mismatch`
- `over_permissioned`
- `vulnerable_version`
- `compromised_signer`
- `policy_violation`
- `user_blocked`
- `registry_removed`

blocklist check は install、update、startup、future の execution surface 前に行います。実行中 job は cancel または quarantine します。

## Audit Events

minimum events:

- registry lookup
- manifest validation
- compatibility check
- install approval
- update approval
- permission grant
- capability grant
- hook creation / change
- webhook receipt
- future package execution start / end (execution が有効化された後)
- policy decision
- secret access
- repo mutation
- external network call
- blocklist hit
- rollback
- output customization transform

plaintext secret は audit に残しません。

## Agency Preservation

package は Secretary の判断を支援するためのものです。

Review questions:

- この package は Secretary がより良い判断をする助けになるか
- Secretary に見えない場所で判断を置き換えていないか
- memory、notification、review flow、delegation、tool default を変更する場合、その impact を manifest に宣言しているか
- Secretary は変更内容、理由、戻し方を確認できるか
- 失敗が AX Feedback と仕事場改善につながるか
