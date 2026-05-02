# Execution Semantics

Execution semantics define what it means for the daemon to run work. This keeps
jobs, tools, cancellation, timeouts, and effects predictable for clients and
agents.

## Job Lifecycle

Initial lifecycle:

1. `SubmitJob` validates request shape.
2. Daemon persists `job: queued`.
3. Daemon records initial `job_event`.
4. Daemon evaluates policy.
5. Daemon records `policy_decision`.
6. If allowed, daemon transitions job to `running`.
7. Tool invocations create `tool_invocation` records.
8. Tool outputs create `tool_result` records.
9. Effects create audit records.
10. Job transitions to `succeeded`, `failed`, `blocked`, or `canceled`.

Policy evaluation happens before execution effects.

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

## Concurrency

The daemon should start conservative:

- one mutating job per repository;
- multiple bounded read jobs allowed if store and policy permit;
- process execution limited by daemon concurrency budget;
- extension/provider concurrency explicit later.

Concurrent jobs must not interleave writes without a recorded policy and lock
decision.

## Filesystem Scope

Filesystem tools must resolve paths before execution:

- normalize path;
- reject traversal outside registered scope;
- reject symlink escapes unless explicitly allowed;
- record resolved path and redacted display path where needed.

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

Shell-string execution is not the default contract. If shell execution is ever
allowed, it is a separate capability with higher risk.

## Tool Output

Every tool invocation creates:

- invocation record;
- canonical result;
- optional artifact refs;
- audit record for effects.

Large output is truncated with metadata. Truncation must be visible to agents.

## AX Check

The execution model should let an agent answer:

- Did the job start?
- What policy allowed or blocked it?
- Which tool ran?
- What did it change?
- Where is the canonical result?
- Can I retry, narrow scope, ask for approval, or stop?

If an agent cannot answer those from records, the runtime is not inspectable
enough.
