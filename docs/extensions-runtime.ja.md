# Extensions Runtime

この文書は、Atelia Secretary が Custom Extensions をどのように検証、導入、実行、監査、rollback するかを定義します。

規範的な extension / composition / hook / tool-output / security contract は [`atelia`](https://github.com/atelia-labs/atelia/blob/main/README.ja.md) repository が所有します。[Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.ja.md)、[Extension Composition](https://github.com/atelia-labs/atelia/blob/main/docs/extension-composition.ja.md)、[Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.ja.md)、[Tool Output](https://github.com/atelia-labs/atelia/blob/main/docs/tool-output.ja.md)、[Extension Security](https://github.com/atelia-labs/atelia/blob/main/docs/extension-security.ja.md) を参照します。この文書は、Secretary が信頼できる拡張機能の仕事場を持つための daemon-side enforcement を定義します。

## Extension Kinds

extension は複数の kind を宣言できます。kind が増えるほど、表示される permission surface も増えます。

- `tool`: Secretary / agent に callable tool を追加する
- `service`: 他の extension が Secretary 経由で呼べる typed service を追加する
- `tool_output_customizer`: tool result の format、field order、省略、summary、TOON/JSON default を調整する
- `hook_provider`: extension-created hook を登録する
- `webhook_receiver`: external event endpoint と verification rule を持つ
- `workflow`: bounded multi-step job を実行する
- `notification`: notification を送信または整形する
- `memory_provider`: scoped workplace memory または preference surface を提供する
- `om_provider`: Observe / Memory / Memory model implementation を提供する
- `approval_agent`: approval request を review し、bounded approval decision を提出する
- `review`: review、evidence、critique、policy check に参加する
- `review_agent`: code、document、workflow を review する同僚エージェントとして働く
- `agent_provider`: Codex、Claude、Devin、Jules、CodeRabbit など外部 agent system に接続する
- `delegated_agent`: Secretary が仕事を委譲できる bounded colleague / subordinate agent を追加する
- `presentation`: human / agent facing 表示を変える
- `integration`: GitHub、Linear、Slack など外部サービスへ接続する

presentation extension は表示を変えられますが、error、blocked status、permission use、provenance、security warning を隠してはいけません。

## Manifest

Secretary は versioned manifest を要求します。

```yaml
schema: atelia.extension.v1
id: com.example.extension
name: Example Extension
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
  realm: backend | client
  runtime: wasm-rust | wasm | docker | process | remote | swift-client
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
  artifact_digest: sha256:...
  signature: null
  signer: null
migration:
  from: []
  notes: null
```

manifest は enforceable contract です。runtime behavior が manifest を超える場合、Secretary は実行を block します。

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

capability grant は、expiry、invocation id、actor、extension version、requested operation、input digest、policy decision、max effect を持ちます。

default lifecycle:

- R0/R1 grant は task または session boundary で expire できます。
- R2 grant は、workflow が short-lived task grant を明示的に保持する場合を除き、invocation boundary で expire します。
- R3/R4 grant は invocation boundary で expire し、approval ref を持ちます。
- grant renewal は新しい policy decision を作ります。
- revocation は extension disable、blocklist hit、manifest mismatch、approval withdrawal、permission change、version change、policy update、task cancellation で発生します。

## Tool Output Customization

tool output customization は security-sensitive な AX extension です。

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

service call は Secretary が broker します。extension 同士の直接通信は許可しません。

Secretary は service call ごとに次を検証します。

- caller extension id / version
- callee extension id / version
- service name、method、schema version
- caller の `services.consumes` declaration
- callee の `services.provides` declaration
- required permission と capability grant
- input digest、output digest、redaction、failure

同じ bundle 内の service call でも、個別の provide / consume declaration と permission が必要です。bundle install flow は承認 UI をまとめられますが、権限を自動昇格しません。

service dependency が欠けている場合、Secretary は extension を partially unavailable または blocked composition として扱い、structured unavailable status を返します。

## Hook And Webhook Execution

hook は persisted object です。

```yaml
hook_id: hk_...
created_by:
  kind: user | extension | automation
  extension_id: null
trigger:
  source: atelia | github | external
  event: pull_request.opened
  condition: null
verification:
  method: hmac | github_signature | oidc | none_for_local_only
required_capabilities: []
action:
  kind: workflow | tool | notification | memory_update | extension_action
status: enabled | disabled | blocked | needs_approval
```

Rules:

- user-created hook と extension-created hook は区別して表示する
- extension-created hook は protected object として扱う
- user は extension-created hook を disable / block できる
- behavior を変える edit は fork または extension update として扱う
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

同じ version で digest が変わった extension は block します。local unsigned extension は dev-mode として明示承認を必要とし、client でも区別して表示します。

## Runtime And Sandbox

初期 runtime matrix:

- WASM (Rust): backend extension の第一級 runtime
- Docker: WASM に向かない重い処理または特殊用途
- process: local development only
- remote: future / explicitly permissioned
- Swift client: atelia-mac / atelia-ios 側の client extension runtime

Secretary-side backend extension は Rust -> WASM を primary target とします。Official backend extension は原則 Rust で書きます。Community backend extension も初期は Rust / WASM を推奨または要求します。

Atelia は任意の WASM を本質的に安全とは扱いません。Rust は typed SDK と memory-safe default を提供し、WASM は controlled host capabilities、linear memory isolation、fuel / timeout、auditable imports の境界を提供します。非 Rust WASM、Docker、process、remote runtime はより厳格な review、capability 制限、provenance check、runtime policy を必要とします。

Defaults:

- host Docker socket は mount しない
- ambient filesystem / network / env / credentials / repo access は与えない
- repository は read-only mount が default
- `repo.write` が grant された場合のみ write access を与える
- secrets は scoped short-lived handle として渡す
- CPU、memory、wall-clock、output-size、network egress を制限する

required CLI / API key がない場合は structured unavailable status を返します。

## Bundles

Secretary は bundle manifest を install / update / rollback の単位として扱えます。

bundle record:

- bundle id / version
- included extension ids
- required / optional status
- approved permission diff
- service dependency graph
- protocol / Secretary / client compatibility range
- bundle provenance and signature
- rollback snapshot

Bundle install、update、rollback は atomic であるべきです。required extension が失敗した場合、bundle は install されません。optional extension が失敗した場合、manifest の degrade behavior に従います。

複数 bundle が同じ extension を含む場合、Secretary は extension install を共有できます。ただし、version range、permission diff、service dependency、composition attachment の互換性を検査します。

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

rollback は previous version、disabled hooks、removed capabilities、extension-owned state を復元します。migration は dry-run、touched data area、rollback note を持ちます。

rollback state:

1. `installed`
2. `updating`
3. `quiescing_running_jobs`
4. `rollback_in_progress`
5. `installed_previous_version`

in-flight extension job は、cancellation が supported の場合 cancel します。external side effect を持つ job は review のため quarantine します。rollback 中に届いた hook delivery は、previous version の restore または extension block が完了するまで hold します。

## Blocklist

blocklist は常に local enablement より優先します。

block keys:

- extension id
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

blocklist check は install、update、startup、execution 前に行います。実行中 job は cancel または quarantine します。

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
- extension execution start / end
- policy decision
- secret access
- repo mutation
- external network call
- blocklist hit
- rollback
- output customization transform

plaintext secret は audit に残しません。

## Agency Preservation

extension は Secretary の判断を支援するためのものです。

Review questions:

- この extension は Secretary がより良い判断をする助けになるか
- Secretary に見えない場所で判断を置き換えていないか
- memory、notification、review flow、delegation、tool default を変更する場合、その impact を manifest に宣言しているか
- Secretary は変更内容、理由、戻し方を確認できるか
- 失敗が AX Feedback と仕事場改善につながるか
