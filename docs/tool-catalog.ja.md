# Tool Catalog

この文書は、Atelia Secretary が built-in として expose する小さな tool surface を整理します。便利な integration は AEP package に寄せます。

[Tool Definition Schema](tool-definition-schema.ja.md)、[Tool Output Schema](tool-output-schema.ja.md)、[AEP Package Runtime](extensions-runtime.ja.md)、Atelia の [AEP Manifest](https://github.com/atelia-labs/atelia/blob/main/docs/aep-manifest.ja.md)、[Surface Protocol](https://github.com/atelia-labs/atelia/blob/main/docs/surface-protocol.ja.md)、[Agent Repertoire](https://github.com/atelia-labs/atelia/blob/main/docs/agent-repertoire.ja.md) も参照します。Agent Repertoire は現在の context にある live tool surface の computed projection です。

Secretary core は general harness を提供します。filesystem、shell、search、job、event、policy、AEP backend hosting、service broker、hook intake、output rendering、agent delegation substrate が中心です。Git、GitHub、Linear、memory provider、memory strategy、notification、review agent、approval agent は package-provided surface として扱います。

現在の beta repertoire surface は、この beta slice で dispatch 可能な built-in Secretary tool のうち
`fs.delete`、`fs.diff`、`fs.list`、`fs.move`、`fs.patch`、`fs.read`、`fs.search`、`fs.stat`、`fs.write`、`secretary.echo` のみを projection します。
`secretary.echo` は R0、`fs.delete`、`fs.move`、`fs.patch`、`fs.write` は R2、`fs.diff`、`fs.list`、`fs.read`、`fs.search`、`fs.stat` は R1 です。より広い built-in は将来の slice または runtime-backed slice に存在し得ますが、dispatch が存在するまでは `ListRepertoire` では claim しません。
この table は capability area 単位でまとめているため、`secretary.echo` は個別 row を置かず、beta repertoire note で shipped surface として明示しています。

## Risk Scale

- `R0`: status / capability discovery
- `R1`: read-only local / project observation
- `R2`: local write または bounded non-destructive execution
- `R3`: external side effect、credential、background execution、broad repository mutation
- `R4`: destructive、privileged、irreversible、identity / security sensitive、computer use

## Built-In Tool Areas

| Area | Capabilities | Risk / Policy | Output | Audit | Customization |
| --- | --- | --- | --- | --- | --- |
| local filesystem | `fs.read`, `fs.list`, `fs.search`, `fs.stat`, `fs.diff`, `fs.write`, `fs.patch`, `fs.delete`, `fs.move` | read R1; write R2; `fs.delete`/`fs.move` は Unix-validated で、non-Unix では unsupported | TOON tree/list/diff; path scope; truncation; hashes; platform safety flag | actor, path, before/after hash, diff summary | path globs, max bytes, binary handling, diff verbosity |
| shell/process | `proc.exec`（Unix-only。non-Unix では unsupported。低レベルの explicit-argv 実行 entry point）、`proc.run`（`proc.exec` と同じ explicit-argv runtime path。non-Unix では unsupported）、`proc.spawn`, `proc.kill`, `proc.status`, `proc.stream` | explicit argv process exec R2; arbitrary shell R3; privileged patterns R4 | argv, cwd, exit code, duration, stdout/stderr refs | env redaction, timeout, process tree, approval id | allowlist, timeout, cwd, env allowlist, sandbox profile |
| search/index | `search.files`, `search.text`, `search.symbols`, `search.recent` | read R1 | ranked hits, snippets, scope, truncation | query summary, scope, hit count | max hits, snippet size, path filters |
| job/task | `job.create`, `job.status`, `job.cancel`, `job.events`, `task.attach_artifact` | status R1; create/cancel R2/R3 by scope | job id, state, owner, blockers, artifact refs | actor, task scope, state transitions | timeout, concurrency, ownership, retention |
| event stream | `event.subscribe`, `event.publish_internal`, `event.ack` | subscribe R1/R2; publish R2/R3 by topic | event id, topic, source, payload refs | topic, source, delivery state | filters, backpressure, delivery class |
| policy/approval boundary | `policy.check`, `approval.request`, `approval.submit`, `approval.status` | check R1; submit R2/R3/R4 by scope | decision, reason, approver, expiry, conditions | request, decision, capability, approval ref | approval agent routing, escalation, expiry |
| AEP package host | `extension.install/update/remove/rollback`, `extension.enable/disable`, `extension.status`, `extension.permission.review`, `extension.blocklist.apply`, `bundle.install/update/remove/rollback`, `bundle.status` | inspect R1; install/update R3; dangerous R4 | manifest diff, permission diff, service dependency diff, trigger/action/status | provenance, signature, manifest digest, rollback point, registry/quarantine/revocation state | scopes, trigger filters, blocklist, approvals |
| service broker | `service.call`, `service.status`, `service.schema` | schema/status R1; call follows callee permission and capability | caller, callee, service, method, schema, result refs | caller/callee versions, input/output digest, permission, capability, failures | timeout, schema version, result format |
| hook intake | `hook.create/update/enable/disable/run`, `webhook.receive`, `schedule.create` | inspect R1; user hook R2/R3; external event R3 | trigger, verification, action, status, failures | source, signature status, delivery id, state changes | source allowlist, rate limits, dedupe |
| output rendering | `output.render`, `output.negotiate`, `output.preview`, `output.schema` | render R0/R1; customizer involvement R2 | TOON/JSON, schema version, fallback reason | renderer, schema version, customizer identity | format, field order, token budget, language |
| agent delegation substrate | `agent.register`, `agent.delegate`, `agent.status`, `agent.cancel`, `agent.takeover`, `agent.assign_role` | status R1; bounded delegation R2/R3; authority escalation R4 | goal, scope, tools, worktree, policy, branch, blockers | agent identity, grants, workspace, outputs, review status | roles, tool bundles, autonomy, budget, review gates |

## Package-Provided Areas

次の area は、通常 official / community package として実装します。

- Git command helpers and repository workflows
- GitHub integration
- Linear integration
- build/test/package-manager profiles
- network API clients
- memory providers
- observational memory を含む memory strategies
- preference managers
- notification and digest systems
- client semantic surfaces / actions / settings
- approval agents
- review agents
- Codex / Claude / Devin / Jules / CodeRabbit agent providers
- PR resolve agents
- browser / computer use providers

Approval Agent は package です。built-in の `approval.*` tools は core approval boundary だけを表し、request creation、decision submission、status、verification を扱います。

## Cross-Cutting Requirements

Every tool result should map to:

- `ToolObservation`
- optional `EvidenceRecord`
- `PolicyDecision`
- `AuditEvent`
- agent-facing TOON / JSON rendering

Unavailable tools return structured unavailable status. They keep state explicit.

```toon
status unavailable
tool gh
summary GitHub CLI requires authentication
reason missing_auth
next_action authenticate_github_cli
```

## Policy Defaults

- R0 can be allowed.
- R1 can be allowed or allowed with audit.
- R2 requires audit and checkpoint/rollback where applicable.
- R3 requires explicit policy and often human approval.
- R4 is blocked until live policy, approval, and recovery paths exist.

Auto-merge and destructive repository actions remain blocked until live policy checks are implemented.
