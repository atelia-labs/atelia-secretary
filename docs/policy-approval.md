# Policy And Approval Model

Policy is the boundary between Secretary's judgment and daemon execution. The
daemon must never perform a privileged action before recording a policy
decision.

## Risk Tiers

| Tier | Meaning | Default |
| --- | --- | --- |
| R0 | informational, no workspace access | allowed |
| R1 | bounded read under registered scope | allowed or audited |
| R2 | local mutation under registered scope | audited |
| R3 | external effect, broad mutation, secret use, or approval-sensitive action | needs approval |
| R4 | blocked by policy or unsupported capability | blocked |

Tiers are defaults. Repository trust, user preferences, extension provenance,
and current context can raise risk.

## Policy Inputs

Every decision records:

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

If approval UI is unavailable, R3 returns `needs_approval`. It does not silently
downgrade to `audited`.

## Approval Requests

An approval request includes:

- requested action
- actor
- repository
- resource scope
- risk tier
- reason
- expected effect
- proposed timeout
- audit ref

The daemon may create approval records before a full human approval UI exists.
Clients can display the state and later attach approval decisions.

## Policy Defaults

Initial defaults:

- filesystem read/list/search/stat/diff inside registered scope: R1
- filesystem write/patch inside registered scope: R2
- process execution with explicit argv, cwd, timeout, env allowlist: R2 or R3
- broad repository mutation: R3
- destructive repository action: R4 until explicit policy exists
- external network or service call: extension-only, R3 by default
- secret access: R3

## Policy Versioning

Policy decisions include `policy_version`. Changing defaults does not rewrite
old decisions. If a job is retried under a new policy, it receives a new policy
decision.

## Audit Coupling

Every `audited`, `needs_approval`, or `blocked` decision creates or references
an audit record. `allowed` decisions may be summarized, but job-level decisions
are still inspectable.

## AX Check

Agents should be able to understand what to do next:

- `allowed`: continue.
- `audited`: continue, but expect durable evidence.
- `needs_approval`: ask the human or wait for a client approval state.
- `blocked`: stop, explain the reason, and propose a narrower action if
  possible.

Policy should feel like a visible coworker, not an invisible trapdoor.
