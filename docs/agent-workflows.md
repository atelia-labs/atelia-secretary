# Agent Workflows And AX Review

This document walks through realistic Secretary runtime workflows from an
agent's point of view. It checks whether the protocol, tools, policy, output,
and ledger design give the agent enough context to work without guessing.

## Workflow 1: Orient In A Repository

Goal: an agent opens an Atelia workspace and needs to understand where it is.

Calls:

1. `Health`
2. `ListRepositories`
3. `RegisterRepository` if the workspace is not registered
4. `GetProjectStatus`
5. `WatchEvents` from latest known cursor

Good AX:

- one status call gives daemon, repository, jobs, policy, and storage summary;
- registration returns a stable repository id;
- event stream gives continuity after restart;
- errors say whether to register, refresh, or stop.

Bad AX to avoid:

- requiring the agent to scan local files before trust is established;
- returning only logs instead of typed repository status;
- making missing registration look like daemon failure.

## Workflow 2: Bounded Read Task

Goal: inspect docs for stale protocol references.

Calls:

1. `SubmitJob(kind: documentation_review, repository_id, goal)`
2. daemon records `job: queued`
3. `CheckPolicy(capability: filesystem.read, scope: docs)`
4. policy returns `allowed` or `audited`
5. tool invocation: `fs.search`
6. `RenderToolOutput(format: toon)`
7. `WatchEvents` observes job progress

Good AX:

- policy reason is visible even when allowed;
- search result includes truncation metadata;
- canonical result can be re-rendered as JSON for debugging;
- job events make it clear which files were inspected.

## Workflow 3: Local Patch Task

Goal: update a design document.

Calls:

1. `SubmitJob(kind: docs_patch, repository_id, goal)`
2. `CheckPolicy(capability: filesystem.write, scope: docs/runtime-architecture.md)`
3. policy returns `audited`
4. tool invocation: `fs.patch`
5. daemon records `tool_result`
6. daemon records `audit_record`
7. `WatchEvents` emits patch applied and job completed

Good AX:

- the agent sees that write is audited before patching;
- audit record links requester, policy, file path, and output ref;
- client can show a calm "changed under audit" state;
- failure suggests retrying with narrower scope or inspecting conflict.

## Workflow 4: Process Execution

Goal: run verification.

Calls:

1. `CheckPolicy(capability: process.run, argv: ["cargo", "test"], cwd)`
2. policy returns `audited` or `needs_approval` depending on repo trust
3. tool invocation: `proc.run`
4. daemon captures exit status, stdout/stderr refs, truncation metadata
5. `RenderToolOutput(format: toon)`

Good AX:

- argv is structured, not a shell string;
- timeout and env allowlist are visible;
- output truncation tells the agent what was omitted;
- nonzero exit maps to a stable error and next state.

## Workflow 5: Approval-Gated Action

Goal: run a broad mutation or external side effect.

Calls:

1. `CheckPolicy(capability: repository.merge or external.call)`
2. policy returns `needs_approval`
3. daemon records approval request and audit ref
4. client displays approval state
5. agent waits or asks the human with the user-facing reason

Good AX:

- the agent does not need to invent an approval message;
- the human sees scope, expected effect, and risk tier;
- if approval UI is unavailable, the runtime still returns a stable
  `needs_approval` state rather than running.

## Workflow 6: Recovery After Disconnect

Goal: an agent reconnects after losing event stream.

Calls:

1. `WatchEvents(cursor: last_seen)`
2. if cursor valid, daemon streams missed events
3. if cursor expired, daemon returns `CURSOR_EXPIRED`
4. agent calls `GetProjectStatus`
5. agent resumes from latest event id

Good AX:

- reconnect is routine, not scary;
- the agent can recover without asking the user what happened;
- event gaps are explicit.

## Current Skeleton Check

The current repository has:

- `SecretaryService.Health` only in `proto/atelia/v1/secretary.proto`;
- an `ateliad` skeleton that starts, logs policy state, and waits for Ctrl-C;
- `atelia-core` primitives for `ProjectId`, `PolicyState`, and AX feedback.

Local timed execution confirms the skeleton starts and reports that the RPC
server is not wired yet.

That is enough to validate the direction, but implementation should not expand
the proto ad hoc. Add the protocol contract, storage, policy, error, and
execution shapes first, then implement slices in order.

## AX Review Checklist

Before implementing or changing a runtime tool, ask:

- Can the agent tell what state it is in with one orienting call?
- Does every execution effect have a preceding policy decision?
- Does every output have a canonical result before rendering?
- Can the agent recover after interruption?
- Can the human see why approval is needed?
- Are failure states specific enough to choose the next action?
- Is the tool pleasant to call repeatedly, or does it force log archaeology?
