# Protocol Contract

This document defines the first durable protocol contract for Atelia Secretary.
It is the bridge between the daemon runtime architecture, `atelia-kit`, and
native clients.

The protocol should be boring, typed, versioned, and event-friendly. It should
not expose implementation storage details, but it must preserve enough identity
and audit references for clients and agents to understand what happened.

## Versioning

Every response that represents daemon state includes:

- `protocol_version`: semantic protocol contract version.
- `daemon_version`: daemon implementation version.
- `storage_version`: local store schema version.
- `capabilities`: named protocol capabilities available in this daemon.

Clients must treat unknown enum values and capabilities as recoverable
compatibility events, not fatal crashes.

## Identity

Protocol ids are opaque strings with stable prefixes:

| Prefix | Entity |
| --- | --- |
| `repo_` | repository |
| `job_` | job |
| `evt_` | event |
| `pol_` | policy decision |
| `lock_` | lock decision |
| `tool_` | tool invocation |
| `res_` | tool result |
| `aud_` | audit record |

Ids are not user-facing copy. Clients may display shortened ids only as
diagnostic metadata.

## Service Surface

This table is the required service contract, not a description of the current
protobuf implementation. The current proto may expose only
`SecretaryService.Health`; the other RPC groups are planned contract surface and
must be added before clients or agents depend on them.

Required RPC groups:

| RPC | Purpose |
| --- | --- |
| `Health` | Inspect daemon availability and versions |
| `RegisterRepository` | Add or update a trusted workspace root |
| `ListRepositories` | Inspect registered repositories |
| `GetProjectStatus` | Summarize repository, job, policy, and event state |
| `SubmitJob` | Create a bounded unit of work |
| `GetJob` | Inspect one job |
| `ListJobs` | Inspect recent jobs with filters |
| `CancelJob` | Request cancellation |
| `WatchEvents` | Stream ordered events from a cursor |
| `CheckPolicy` | Preview policy outcome for a requested action |
| `RenderToolOutput` | Render canonical tool result as TOON, JSON, or text |
| `InstallExtension` | Install or update an extension manifest |
| `ExtensionStatus` | Inspect one extension installation and blocklist state |
| `ListExtensions` | List installed extension statuses |
| `RollbackExtension` | Restore the previous version of an extension |
| `ApplyBlocklist` | Add a blocklist entry |
| `ListBlocklist` | Inspect the current blocklist |

## Core Messages

### Health

`HealthResponse` includes:

- daemon status: `starting`, `running`, `ready`, `degraded`, `stopping`
- daemon version
- protocol version
- storage version
- storage status: `ready`, `migrating`, `read_only`, `unavailable`
- capability names

### Repository

`Repository` includes:

- repository id
- display name
- root path
- allowed path scope
- trust state: `trusted`, `read_only`, `blocked`
- created / updated timestamps

`RegisterRepository` must validate that the requested root exists, is inside an
allowed local scope, and is not already blocked by policy.

### Job

`Job` includes:

- job id
- repository id
- requester
- kind
- goal
- status
- policy summary
- created / started / completed timestamps
- latest event id (optional)
- cancellation details (`state` plus request/completion metadata when present)

`SubmitJob` must not execute work immediately before policy has been evaluated.
The first observable effect is a persisted `job` and `job_event`.

### Event

`Event` includes:

- event id
- sequence number
- timestamp
- subject type and id
- event kind
- severity
- public message
- refs to job, policy decision, lock decision, tool invocation, tool result, or
  audit record

`WatchEvents` accepts a cursor and returns events after that cursor. Clients can
replay events after reconnect without losing job history.

### Policy Decision

`PolicyDecision` includes:

- decision id
- outcome: `allowed`, `audited`, `needs_approval`, `blocked`
- risk tier: `R0`, `R1`, `R2`, `R3`, `R4`
- requested capability
- reason code
- user-facing reason
- approval request ref, if any
- audit ref

### Tool Result

`ToolResultRef` points to canonical structured output. The protocol returns refs
by default and renders output through `RenderToolOutput` so clients can choose
TOON, JSON, or text without changing the underlying result.

## Event Ordering

Event ordering is per daemon store:

- each event receives a monotonically increasing sequence number;
- events are append-only;
- retries must not create duplicate semantic effects;
- clients resume with the last seen sequence number or event id.

If the daemon cannot guarantee continuity, `WatchEvents` returns a
`CURSOR_EXPIRED` recovery error and tells the client to call `GetProjectStatus`
and then resume from the returned latest event.

## Error Shape

Every RPC error should map to the error taxonomy:

- stable `code`
- user-facing `reason`
- `recoverable` boolean
- required `next_state`
- optional `retry_after`
- optional `audit_ref`

Transport errors should be wrapped into the same shape when they reach clients,
including `next_state`, so recovery logic stays consistent.

## AX Check

For agents, the protocol must reduce repeated guessing:

- `GetProjectStatus` gives one compact orienting call.
- `SubmitJob` returns a job id and initial policy summary immediately.
- `WatchEvents` gives a single stream for job, policy, audit, and repository
  changes.
- `RenderToolOutput` avoids re-running tools only to get a different format.
- Policy errors include next states so the agent knows whether to ask the human,
  retry with narrower scope, or stop.
