# Tool Output Schema

この文書は、Atelia Secretary が tool 実行結果を Secretary と支援エージェントへ返すときの設計方針を定義します。

AI エージェントにとって、tool output は仕事場の手触りそのものです。見づらい出力、冗長な出力、意味の薄い key、順序の悪い field は、そのまま AX の悪化になります。

## 基本方針

Atelia の agent-facing tool output は、TOON を第一の形式として扱います。

TOON は Token-Oriented Object Notation です。LLM に構造化データを渡すための compact な表現であり、特に同じ key を繰り返す object array や tabular な data で JSON より token を削減しやすい性質があります。

ただし、Secretary は設定から TOON と JSON を切り替えられるべきです。

- global default format
- tool ごとの default format
- 1回の tool call だけに適用する temporary override
- client capability に応じた fallback
- debug / audit / artifact 用の別 format

この設定可能性は、Secretary が自分の仕事場を育てるための AX customization です。

## Canonical Result Model

内部の canonical result は、rendering format から独立します。TOON / JSON / text は、同じ domain model に対する rendering です。

最小 model:

- `id`: tool result id
- `tool`: logical tool name
- `call_id`: tool call id
- `run_id`: job / task / session execution id
- `schema_version`
- `format`: rendered format
- `status`: `ok`, `partial`, `failed`, `blocked`, `needs_approval`, `cancelled`, `timeout`, `unavailable`
- `summary`: short agent-facing summary
- `reason`: unavailable / blocked / failed state の短い structured reason
- `tool_code`: tool-specific な stable code
- `evidence`: evidence refs or compact evidence records
- `artifacts`: large payload refs
- `actions`: possible next actions と recovery affordances
- `policy`: policy state, blocked reason, approval refs
- `diagnostics`: parser failures, truncation, retry hints
- `cost`: token / byte / duration estimates when available
- `audit_ref`: audit record ref

`summary` は、Secretary が次に何を見ればよいかを案内するための短い入口です。

## Result Channels

tool execution は目的別 channel へ分けます。

| Channel | 目的 | 主な消費者 | 形式 |
| --- | --- | --- | --- |
| primary | Secretary / agent の直近の判断材料 | Secretary, agents | TOON default, JSON configurable |
| evidence | 判断に使う根拠 | Secretary, agents, clients | canonical structured data |
| artifact | 大きい payload、diff、log、trace、report | clients, storage, later inspection | original or stored format |
| audit | 完全な実行記録、policy、approval、redaction | daemon, security, review | JSON / JSONL |
| diagnostic | tool health、parser error、retry hint | Secretary, maintainers | structured |
| analytics | AX 改善用の低 cardinality event | AX improvement flow | privacy-filtered events |

大きな stdout / stderr / diff / trace は artifact に置き、primary には参照、要約、欠落範囲、次の取得方法を返します。

## Field Design

各 field は、次の問いを通して採用します。

- Secretary の次の判断に必要か
- audit channel に置く方が適切か
- artifact ref で足りるか
- 他の field と重複していないか
- 省略されたときに意味が曖昧になるか
- token cost に見合う情報量があるか
- agent が繰り返し問い合わせている情報か

field order も AX です。高 signal な field を先に置きます。

標準順序:

1. `status`
2. `summary`
3. identity / scope: `tool`, `repo`, `path`, `issue`, `branch`
4. policy / approval: `policy`, `needs_approval`, `blocked_reason`
5. evidence / changed state
6. artifacts
7. diagnostics
8. cost / timing
9. audit refs

## Key Naming

agent-facing keys は原則として英語の `snake_case` を使います。

- durable id: `_id`
- retrievable handle: `_ref`
- timestamp: `_at`
- duration: `_ms`
- byte count: `_bytes`
- token estimate: `_tokens`

避ける key:

- `data`
- `result`
- `info`
- `content`
- `misc`

これらは意味が広すぎて、agent が schema を学びにくくなります。generic な payload が必要な場合も、なぜ generic なのかを schema に残します。

## TOON Rendering

TOON rendering は、canonical result の primary / evidence 部分から生成します。

Rules:

- `schema_version` と `format toon` を先頭近くに置く
- `status` を最初に置く
- repeated homogeneous objects は table-like にする
- empty / default / null field は省略する
- long text は artifact ref に逃がす
- error / blocked / needs approval は省略しない
- policy state と approval requirement は truncate しない
- redaction が入った場合は `redactions` を残す

Example:

```toon
schema_version tool_result.v1
format toon
status partial
tool git_status
repo atelia-secretary
branch main
summary 2 modified docs, 1 untracked file
changed_files[3]{path,status}
  docs/tool-output-schema.ja.md,modified
  docs/research/agent-harness-survey.ja.md,modified
  docs/extensions-runtime.ja.md,untracked
artifacts
  diff_ref artifact:diff:8f13
policy
  state allowed_with_audit
audit_ref audit:tool-call:01h
```

## JSON Rendering

JSON は Secretary が選べる別形式です。

JSON が有用な場面:

- external integration
- debug
- client / protocol compatibility
- deeply nested irregular data
- machine-to-machine handoff
- TOON renderer が安全に表現できない payload

Secretary は settings で JSON を default にできます。tool ごとの default も変更できます。

## Schema Compatibility

`tool_result.v1` は、extension、client、golden fixture が依存した時点で public implementation contract として扱います。

compatibility rules:

- additive optional field は compatible
- field の削除または rename は新しい schema version を要求する
- field meaning の変更は新しい schema version を要求する
- status と `tool_code` の追加には documented fallback behavior を持たせる
- deprecated field は少なくとも1つの minor release line で維持する
- tool-output customizer は supported schema range を宣言する
- unsupported customizer version では canonical Secretary output に戻す
- TOON renderer と JSON renderer は同じ canonical schema version を共有する
- default format の変更は release note に出る behavior change として扱う

## Schema Negotiation

schema resolution order:

1. tool-output customizer または client が宣言する supported schema range
2. daemon が安全に生成できる最新 schema
3. compatible fallback schema
4. customizer を介さない canonical Secretary output

fallback でも `status`、`reason`、`tool_code`、`policy`、`blocked_reason`、`needs_approval`、`redactions`、`critical_errors`、`audit_ref` は維持します。

## Format Negotiation

format は次の順で決まります。

1. per-call override
2. per-tool default
3. Secretary global default
4. client capability
5. safe fallback

requested format が使えない場合、tool result は safe fallback を返します。

```toon
status partial
format json
requested_format toon
format_fallback_reason deeply_nested_payload
artifact_ref artifact:full-result:91a
```

## Secretary Preferences

Secretary は tool output に関する preference を持てます。

- `default_result_format`: `toon` / `json`
- `per_tool_format`
- `verbosity`: `minimal`, `normal`, `expanded`, `debug`
- `language_mode`: `user`, `english_agent`, `mixed`
- `include_policy`
- `include_cost`
- `include_diagnostics`
- `artifact_threshold`
- `truncation_strategy`
- `evidence_order`
- `risk_sensitivity`

preference は Secretary の仕事場の好みであり、繰り返しの修正や AX Feedback から改善される対象です。

これらの preference は、Secretary が自分の仕事場に対して持つ professional judgment を表します。client、extension、支援エージェントは変更を提案できます。永続 preference は Secretary が所有します。

## Recovery Contract

`reason`、`tool_code`、`actions` は recovery を案内します。

- `INVALID_PARAMS`: input を修正して retry
- `UNAVAILABLE`: `reason` を確認し、authentication、installation、dependency action へ進む
- `PERMISSION`: capability を request するか scope を調整する
- `POLICY_BLOCKED`: policy と approval path を確認する
- `NEEDS_APPROVAL`: approval request を surface する
- `TIMEOUT`: policy が許す場合、scope を狭めるか timeout を伸ばして retry
- `CANCELLED`: partial artifact と task state を維持する
- `NOT_FOUND`: scope、id、repository selection を確認する
- `CONFLICT`: state を refresh してから retry
- `PARSE_FAILED`: canonical output を返し、renderer / customizer issue を記録する
- `INTERNAL`: audit ref と diagnostic artifact を維持する

recovery hint は `actions` に置き、詳細な retry diagnostic は diagnostic channel に置きます。

## Language And Token Efficiency

ユーザーとの直接対話では、ユーザーの言語を使います。

agent 間のやり取り、tool output key、language-independent な作業では、英語を優先する選択肢を持ちます。多くの LLM では英語の方が token 効率や task performance に有利な場面があるためです。

Secretary は仕事の内容、agent の好み、tool の性質に応じて調整できるべきです。

## Truncation

primary result が大きすぎる場合、Secretary は自動で truncation します。

必ず残すもの:

- `status`
- `summary`
- `policy`
- `needs_approval`
- `blocked_reason`
- `critical_errors`
- `artifact_ref`
- `truncated true`
- `shown_count` / `total_count`
- 追加取得方法

Example:

```toon
status partial
summary 4 failures shown from 128 failed tests
truncated true
shown_count 4
total_count 128
failures[4]{name,file,line}
  test_policy_blocks_merge,crates/policy/tests.rs,42
  test_redacts_secret,crates/audit/tests.rs,88
  test_hook_replay_window,crates/hooks/tests.rs,117
  test_tool_format_override,crates/tools/tests.rs,203
artifact_ref artifact:test-log:77b
next_action fetch_artifact artifact:test-log:77b
```

## Error Output

error output は短く、actionable にします。stack trace は primary に出しません。

```toon
status failed
tool cargo_test
summary tests failed
tool_code TEST_FAILED
failed_count 4
artifact_ref artifact:test-log:77b
audit_ref audit:tool-call:01k
```

標準 `tool_code`:

- `INVALID_PARAMS`
- `UNAVAILABLE`
- `PERMISSION`
- `POLICY_BLOCKED`
- `NEEDS_APPROVAL`
- `TIMEOUT`
- `CANCELLED`
- `NOT_FOUND`
- `CONFLICT`
- `PARSE_FAILED`
- `INTERNAL`

## Analytics Feedback

tool output schema は運用ログから改善します。

観測するもの:

- format selected
- format override
- format fallback
- repeated tool calls
- parse failures
- missing field follow-up
- unused fields
- artifact opens
- truncation rate
- token estimates
- human corrections
- AX Feedback links

目的は、Secretary と agent がより働きやすい tool output を作ることです。
