# Tool Definition Schema

この文書は、Atelia Secretary が tool を Secretary や支援エージェントへ callable にする前に、tool をどのように記述するかを定義します。

tool definition は仕事場の contract です。その tool が何をできるか、どの authority を必要とするか、どの evidence を返すか、どう失敗するか、AX のために結果をどう調整できるかを Secretary に伝えます。

## Minimum Definition

```yaml
schema: atelia.tool.v1
id: fs.search
name: File Search
version: 0.1.0
provider:
  kind: builtin | extension | remote | mcp | workflow
  id: atelia-secretary
description: Search files inside an allowed workspace scope
input_schema_ref: schema:fs.search.input.v1
output_schema_ref: schema:tool_result.v1
default_result_format: toon
supported_result_formats:
  - toon
  - json
risk: R1
permissions:
  - fs.read
effects:
  filesystem: read
  repository: none
  network: none
  secrets: none
idempotency: idempotent
streaming: false
cancellable: true
timeout_ms: 10000
artifact_policy:
  max_primary_tokens: 1200
  large_payload: artifact_ref
audit:
  required: true
  redaction: standard
failure:
  unavailable: structured
  retry: none
customization:
  format: secretary_preference
  field_order: allowed
  omission: allowed_with_required_fields
  language: allowed
  token_budget: allowed
```

## Identity

tool id は stable で namespaced にします。

- built-in: `fs.search`, `proc.run`, `job.status`
- extension: `extension.<extension_id>.<tool_id>`
- remote: `remote.<provider_id>.<tool_id>`
- workflow: `workflow.<workflow_id>.<tool_id>`

tool id の rename は breaking change です。display name は変更できます。

## Input Schema

input schema は typed field と explicit default を持ちます。

Rules:

- every field has a type
- optional field は default または absence behavior を宣言する
- path field は scope と normalization を宣言する
- network field は allowed host または capability reference を宣言する
- secret field は secret handle を使う
- destructive field は explicit boolean と policy check を要求する
- free-form string は max length と redaction class を宣言する

Example:

```yaml
schema: fs.search.input.v1
fields:
  root:
    type: workspace_path
    required: true
  query:
    type: string
    required: true
    max_length: 1024
  include:
    type: array<string>
    default: []
  max_hits:
    type: integer
    default: 50
    max: 500
```

## Built-In Tool Schemas

[Tool Catalog](tool-catalog.ja.md) で built-in surface boundary を定義します。

Secretary built-in は意図的に小さくします。すべての built-in tool は、実装前にこの schema に従う definition を持ちます。

初期 built-ins:

- `fs.read`
- `fs.list`
- `fs.search`
- `fs.stat`
- `fs.diff`
- `fs.write`
- `fs.patch`
- `proc.run`
- `proc.spawn`
- `proc.kill`
- `proc.status`
- `proc.stream`
- `search.files`
- `search.text`
- `search.symbols`
- `job.create`
- `job.status`
- `job.cancel`
- `job.events`
- `event.subscribe`
- `event.publish_internal`
- `event.ack`
- `policy.check`
- `approval.request`
- `approval.submit`
- `approval.status`
- `extension.install`
- `extension.update`
- `extension.remove`
- `extension.rollback`
- `extension.enable`
- `extension.disable`
- `extension.status`
- `bundle.install`
- `bundle.update`
- `bundle.remove`
- `bundle.rollback`
- `bundle.status`
- `service.call`
- `service.status`
- `service.schema`
- `hook.create`
- `hook.update`
- `hook.enable`
- `hook.disable`
- `webhook.receive`
- `schedule.create`
- `output.render`
- `output.negotiate`
- `output.preview`
- `output.schema`
- `agent.register`
- `agent.delegate`
- `agent.status`
- `agent.cancel`
- `agent.takeover`

Git helpers、GitHub、Linear、OM provider、memory、notification、review agent、approval agent は、同じ schema を使って extension 側で定義します。

`approval.*` built-ins は decision の submission / verification のための boundary tools です。approval judgment 自体は human または Approval Agent extension が担います。

## Output Schema

すべての tool は canonical `tool_result.v1` envelope を返します。tool-specific evidence は stable evidence record として入れます。

tool definition は次を宣言します。

- primary result schema
- evidence record schemas
- artifact types
- status codes
- `tool_code` values
- renderer と customizer が preserve する required fields
- default TOON field order
- integration / debug 用 JSON shape

## Effects And Risk

effects は permission と分けて宣言します。

permission は authority を表します。effect は何が変わり得るかを表します。

effect dimensions:

- filesystem: `none`, `read`, `write`, `delete`
- repository: `none`, `read`, `branch`, `commit`, `merge`, `destructive`
- network: `none`, `read`, `write`, `webhook_receive`
- secrets: `none`, `named_read`
- memory: `none`, `read`, `write`
- service: `none`, `provide`, `call`
- notification: `none`, `send`
- workflow: `none`, `run`, `delegate`
- browser: `none`, `use`
- computer: `none`, `use`
- client: `none`, `view`, `action`, `settings`

risk tier は、declared effect と permission の最大値に従います。

## Runtime Behavior

tool definition は runtime behavior を宣言し、Secretary が安全に仕事を組み立てられるようにします。

- idempotency: `idempotent`, `repeatable`, `non_idempotent`
- cancellation: supported / unsupported
- streaming: supported / unsupported
- expected duration
- timeout
- concurrency limit
- checkpoint requirement
- rollback availability
- offline availability
- dependency requirements

dependency が unavailable の場合、`status unavailable` と `reason`、`next_action` を返します。

## Customization Surface

tool definition は、customizable な result behavior を明示します。

customizable:

- default result format
- per-call format override
- field order
- verbosity
- optional field omission
- summarization
- language
- token budget
- artifact threshold

protected:

- `status`
- `tool`
- `schema_version`
- `policy`
- `needs_approval`
- `blocked_reason`
- `redactions`
- `critical_errors`
- `audit_ref`
- required evidence identifiers

persistent default は Secretary preference が所有します。支援エージェントと extension は、可視化された preference update として変更を提案できます。

## Extension Tools

extension が提供する tool も built-in と同じ schema を宣言します。extension manifest は各 tool definition と、その tool を expose するために必要な permission を参照します。

Secretary は次を検証します。

- manifest と tool definition compatibility
- declared permissions と effects の対応
- supported output schema range
- required audit fields
- runtime availability
- provenance と extension version

validation に失敗した tool は、structured reason 付きで unavailable になります。
