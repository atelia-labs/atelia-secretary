# Policy And Approval Model

policy は Secretary の judgment と daemon execution の境界です。daemon は policy decision を記録する前に privileged action を実行してはいけません。

## Risk Tiers

| Tier | Meaning | Default |
| --- | --- | --- |
| R0 | informational, no workspace access | allowed |
| R1 | bounded read under registered scope | allowed or audited |
| R2 | local mutation under registered scope | audited |
| R3 | external effect, broad mutation, secret use, or approval-sensitive action | needs approval |
| R4 | blocked by policy or unsupported capability | blocked |

tier は default です。repository trust、user preference、extension provenance、current context により risk は上がり得ます。

## Policy Inputs

すべての decision は次を記録します。

- requester
- repository id
- requested capability
- resource scope
- tool id or provider id
- declared effect
- current trust state
- approval availability
- policy version

## Outcomes

| Outcome | Execution |
| --- | --- |
| `allowed` | may run |
| `audited` | may run and must create audit evidence |
| `needs_approval` | must not run until approved |
| `blocked` | must not run |

approval UI が unavailable の場合、R3 は `needs_approval` を返します。黙って `audited` に downgrade しません。

## Approval Requests

approval request は次を含みます。

- requested action
- actor
- repository
- resource scope
- risk tier
- reason
- expected effect
- proposed timeout
- audit ref

full human approval UI が存在する前でも、daemon は approval record を作れます。client はその state を表示し、後から approval decision を attach できます。

## Policy Defaults

初期 default:

- registered scope 内の filesystem read/list/search/stat/diff: R1
- registered scope 内の filesystem write/patch: R2
- explicit argv、cwd、timeout、env allowlist を持つ process execution: R2 or R3
- broad repository mutation: R3
- destructive repository action: explicit policy ができるまで R4
- external network or service call: extension-only、default R3
- secret access: R3

## Policy Versioning

policy decision は `policy_version` を含みます。default の変更は old decision を rewrite しません。job を new policy で retry する場合、新しい policy decision を受けます。

## Audit Coupling

すべての `audited`、`needs_approval`、`blocked` decision は audit record を作るか参照します。`allowed` decision は summarize しても構いませんが、job-level decision は inspectable にします。

## AX Check

agent は次の行動を理解できるべきです。

- `allowed`: continue
- `audited`: continue, but expect durable evidence
- `needs_approval`: ask the human or wait for a client approval state
- `blocked`: stop, explain the reason, and propose a narrower action if possible

policy は invisible trapdoor ではなく、見える coworker のように感じられるべきです。
