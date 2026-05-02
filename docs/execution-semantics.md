# Execution Semantics

Execution semantics define what it means for the daemon to run work. This keeps
jobs, tools, cancellation, timeouts, and effects predictable for clients and
agents.

## Job Lifecycle

Initial lifecycle:

1. `SubmitJob` validates request shape.
2. Persist `job: queued`.
3. Record initial `job_event`.
4. Evaluate policy.
5. Record `policy_decision`.
6. If allowed, transition job to `running`.
7. Tool invocations create `tool_invocation` records.
8. Tool outputs create `tool_result` records.
9. Effects create audit records.
10. Job transitions to `succeeded`, `failed`, `blocked`, or `canceled`.

Policy evaluation happens before execution effects.

Implementation requirements are defined by the storage ledger. In short,
persisting `job: queued` and the initial `job_event` must be one atomic store
commit, and a `policy_decision` must be durably persisted before any
`tool_invocation`, `tool_result`, or audit effect record exists.

## Cancellation

`CancelJob` requests cancellation; it does not guarantee immediate stop.

Cancellation states:

- `not_requested`
- `requested`
- `cooperative_stop`
- `force_stop`
- `completed`

Tools must declare whether they support cooperative cancellation. Process tools
receive a graceful shutdown window before force termination.

## Timeouts

Every job and tool invocation has a timeout:

- default timeout from daemon policy;
- optional narrower timeout from request;
- maximum timeout enforced by policy.

Timeouts produce `failed` or `canceled` states depending on whether the daemon
initiated cancellation.

Precedence is explicit and uses the daemon store event ordering from
[Protocol Contract](protocol-contract.md#event-ordering). A logical instant is
determined by the monotonic event sequence number assigned by the daemon store.
Cancellation and timeout share the same logical instant only when their events
are adjacent in that sequence with cancellation appearing first. If timeout is
detected first, the terminal state is `failed`; a later cancellation request
must not overwrite an emitted terminal state. A cooperative stop that began
before timeout stays `canceled`; a `force_stop` triggered strictly by timeout is
`failed`.

| Scenario | Precedence | Terminal state |
| --- | --- | --- |
| cancellation requested, then timeout fires | cancellation has an earlier event sequence | `canceled` |
| cancellation and timeout share the same logical instant | adjacent event sequence, cancellation first | `canceled` |
| timeout fires, then cancellation is requested | timeout was detected first | `failed` |
| cooperative stop begins, then timeout fires during grace period | cooperative stop began before timeout | `canceled` |
| timeout triggers `force_stop` | force stop is caused strictly by timeout | `failed` |

## Concurrency

The daemon should start conservative:

- one mutating job per repository;
- multiple bounded read jobs allowed if store and policy permit;
- process execution limited by daemon concurrency budget;
- extension/provider concurrency explicit later.

Concurrent jobs must not interleave writes without a recorded policy and
`lock_decision`. A `lock_decision` records `id`, `repository_id`,
`policy_decision_id`, owner job/process id, locked path or repository scope,
`locked_at`, `expires_at`, and status. The ledger persists the lock decision
before execution. Active locks are unique by `(repository_id, locked_scope,
active status)` so only one mutating job can hold a scope; `policy_decision_id`
is linkage metadata, not part of the ownership key.
Reclaim is idempotent: repeated reclaim attempts for the same
`lock_decision.id` and owner job/process id return no-op success after the first
persisted reclaim. Restart recovery treats an expired lock as reclaimed only
after appending a reclaim record with `lock_decision.id`, owner id, and
`reclaimed_at`; no execution state transition for that job can begin before this
record exists. After reclaim persistence, the daemon re-evaluates the policy
rule referenced by `policy_decision_id` to determine the next action.

## Filesystem Scope

Filesystem tools must resolve paths before execution:

- normalize path;
- resolve the path with the authoritative algorithm below;
- reject traversal outside registered scope;
- reject symlink escapes unless explicitly allowed;
- record resolved path and redacted display path where needed.

The authoritative path algorithm is canonicalization with verification. The
daemon resolves the requested path and repository root to absolute canonical
paths, rejects the request unless the resolved path is under the canonical
allowed root, then records the resolved path and redacted display path in the
tool invocation / audit records described by [Storage And Ledger
Design](storage-ledger.md). To mitigate TOCTOU, mutating tools must open or
stat the target immediately before use, record the final inode or platform
equivalent when available, and fail if that final target no longer matches the
validated resolved path. A future platform-specific implementation may replace
this with `openat` / dirfd chaining, but it must preserve the same visible
records and rejection semantics.

Writes and patches require audit.

## Process Execution

Process execution requires:

- explicit argv array;
- cwd under registered repository scope;
- env allowlist;
- timeout;
- stdout/stderr capture policy;
- maximum output bytes;
- exit status capture.

Stdout and stderr artifacts are treated as tool results. They use
`tool_results` truncation metadata and redaction markers, and may point to
larger payloads through output refs / evidence refs. Exit status and captured
output are both subject to redaction rules; rendered output must not leak data
that the canonical result or audit policy has hidden.

Shell-string execution is not the default contract. If shell execution is ever
allowed, it is a separate capability with higher risk.

## Tool Output

Every tool invocation creates:

- invocation record;
- canonical result;
- optional artifact refs;
- audit record for effects.

Large output is truncated with metadata. Truncation must be visible to agents.

## Target Architecture AX Check

This section describes the target architecture for persisted runtime records.
The current daemon skeleton may not yet implement these concrete Rust schemas;
implementation slices must add them before clients or agents rely on these
answers.

The execution model should let an agent answer:

- Did the job start?
- What policy allowed or blocked it?
- Which tool ran?
- What did it change?
- Where is the canonical result?
- Can I retry, narrow scope, ask for approval, or stop?

Record mapping:

- policy allowed/blocked: `policy_decisions`;
- which tool ran and where the canonical result lives: `tool_invocations` and
  `tool_results`;
- what changed: `audit_records` with effect summary and output refs;
- retry, scope reduction, approval, or stop: `policy_decisions` plus runtime
  failure taxonomy.

If an agent cannot answer those from records, the runtime is not inspectable
enough.
