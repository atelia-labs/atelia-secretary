# Protocol Contract

This document defines the first durable protocol contract for Atelia Secretary.
It is the bridge between the daemon runtime architecture, `atelia-kit`, and
native clients.

The beta contract is transport-neutral at the Rust RPC boundary. The shipping
beta transport is HTTP/JSON, while proto/gRPC-generated client and server paths
are future work and are not shipped yet.

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

This table is the required service contract, not merely a description of the
current protobuf implementation. The current daemon exposes the health,
repository, job, policy, event replay, project status, tool-output settings,
`RenderToolOutput`, and beta package-management RPC groups, including read-only
AEP backend package manifest validation. Registry and
blocklist operations are currently exposed through the daemon HTTP/JSON beta
transport. The read-only package trust index beta surface is already shipped in
HTTP/JSON as `ListPackageTrustIndex`, and proto now reserves the same contract.
The Rust RPC boundary in `ateliad` stays transport-neutral so a future
proto/gRPC client path can bind to the same contract instead of redefining it.
`WatchEvents` is the live beta subscription surface, while `ReplayEvents` and
`/v1/events/replay` remain available for bounded replay and compatibility.

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
| `WatchEvents` | Stream live ordered events from a cursor |
| `ReplayEvents` | Replay ordered events from a cursor |
| `CheckPolicy` | Preview policy outcome for a requested action |
| `RenderToolOutput` | Render canonical tool result as TOON, JSON, or text |
| `ListPackageTrustIndex` | Read the package trust index with provenance and block markers |
| `PackageInspect` | Inspect one installed package with manifest, permissions, services, source, trust, block, and rollback detail |
| `ValidateExtension` | Validate an AEP backend package manifest without installing or mutating registry state. Beta RPC name is retained for compatibility. |
| `InstallExtension` | Install a new AEP backend package manifest. Beta RPC name is retained for compatibility. |
| `UpdateExtension` | Update an installed AEP backend package manifest |
| `ExtensionStatus` | Inspect one package installation and blocklist state |
| `ListExtensions` | List installed package statuses |
| `RollbackExtension` | Restore the previous version of a package |
| `DisableExtension` | Disable an installed package |
| `EnableExtension` | Enable a disabled package |
| `RemoveExtension` | Remove an installed package |
| `ApplyBlocklist` | Add a blocklist entry |
| `ListBlocklist` | Inspect the current blocklist |

`ListRepertoire` is the first beta protocol slice for
[Agent Repertoire](https://github.com/atelia-labs/atelia/blob/main/docs/agent-repertoire.md).
It returns the computed beta `RepertoireEntry` projection of the live tool
surface in the current context, along with its metadata, not a persisted
store.

Beta RPC groups:

| RPC | Purpose |
| --- | --- |
| `ListRepertoire` | Inspect the beta repertoire projection |

The first beta server surface is intentionally small and currently projects
only the built-in Secretary tools that are dispatchable in this beta slice:
`fs.delete`, `fs.diff`, `fs.list`, `fs.move`, `fs.patch`, `fs.read`,
`fs.search`, `fs.stat`, `fs.write`, and `secretary.echo` as beta repertoire
entries. `secretary.echo` is R0; `fs.delete`, `fs.move`, `fs.patch`, and
`fs.write` are R2; `fs.diff`, `fs.list`, `fs.read`, `fs.search`, and `fs.stat`
are R1. Broader built-ins may exist in future or runtime-backed slices, but
they are not claimed by `ListRepertoire` until dispatch exists. Package-backed
repertoire entries remain a future slice.

For the beta slice, package management APIs are operator-facing. The RPC names
still use `Extension*` because that is the current beta wire surface; docs and
new product language should treat them as AEP backend package management, not a
public package storefront. Package execution RPCs are not supported in beta.
Endpoints may exist, but they must return unsupported-capability when invoked.

## Core Messages

### Health

`HealthResponse` includes:

- daemon status: `starting`, `running`, `ready`, `degraded`, `stopping`
- daemon version
- protocol version
- storage version
- storage status: `ready`, `migrating`, `read_only`, `unavailable`
- optional beta state hint, field `beta_state`:
  - `scope`
  - `durability`
  - `restart_semantics`
  - `limits` stable code tokens describing the limits attached to this beta state
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
- goal (optional bounded-job intent/summary; empty or missing means no durable goal object is created)
- status
- policy summary
- created / started / completed timestamps
- latest event id (optional)
- cancellation details (`state` plus request/completion metadata when present)

Requested capability hints are provided on `SubmitJobRequest` and normalized for
policy and idempotency, but they are not echoed on `Job` yet.

`SubmitJobRequest.goal` is optional. Secretary accepts empty or missing goal
values, stores them as absent/`None`, and keeps the durable `Goal` lifecycle
and OM default package policy in separate future product lanes. This job
request still only carries an optional goal summary; a first-class goal
lifecycle contract is reserved for later.

`SubmitJobRequest.message` is an optional free-form requester message and is
not a fallback for `goal`. Secretary may preserve it for request validation and
idempotency semantics, but does not echo the raw message on `Job` or default
analytics records. `model_route_key` and `permission_mode_route_key` are
optional routing hints preserved with the same request-signature semantics.
When present, `message`, `model_route_key`, and `permission_mode_route_key`
participate in the `SubmitJob` request signature using raw string identity:
blank strings and whitespace-only values are distinct from omission and from
other whitespace spellings. Clients that do not intend a value should omit the
field rather than send a blank value.

`SubmitJob` must not execute work immediately before policy has been evaluated.
The first observable effect is a persisted `job` and `job_event`.
Successful submissions may be replayed by `idempotency_key`, including after a
durable restart; failed submissions are not currently cached as replay results.

`SubmitJobRequest.tool_args` is capability-specific and must match these shapes:

- `filesystem.search` / `fs.search` (read tool)
  - required: `pattern` (string, non-empty)
  - optional: `max` (u64)
  - unsupported: `comparison_path`, `max_bytes`, `max_chars`
- `filesystem.diff` / `fs.diff` (read tool)
  - required: `comparison_path` (string, non-empty)
  - optional: `max_bytes` (u64), `max_chars` (u64)
  - unsupported: `pattern`, `max`
- `filesystem.write` / `fs.write` (write tool)
  - required: `content` (string; empty string is valid for create/truncate)
  - optional: `allow_overwrite` (bool), `max_bytes` (u64)
  - unsupported: `pattern`, `max`, `comparison_path`, `destination_path`, `replacement_text`, `max_chars`
- `filesystem.patch` / `fs.patch` (write tool)
  - required: `pattern` (string, non-empty), `replacement_text` (string)
  - optional: `max_bytes` (u64)
  - unsupported: `max`, `comparison_path`, `destination_path`, `allow_overwrite`, `max_chars`
- `filesystem.move` / `fs.move` (write tool)
  - required: `destination_path` (string, non-empty)
  - optional: `allow_overwrite` (bool)
  - unsupported: `content`, `pattern`, `max`, `comparison_path`, `replacement_text`, `max_bytes`, `max_chars`
- other capabilities: `tool_args` must be omitted.

Examples:

```json
{ "tool_args": { "pattern": "needle", "max": 10 } }
{ "tool_args": { "comparison_path": "right.txt", "max_bytes": 4096, "max_chars": 120 } }
{ "tool_args": { "content": "hello\nworld\n", "allow_overwrite": false, "max_bytes": 4096 } }
{ "tool_args": { "pattern": "beta", "replacement_text": "delta", "max_bytes": 1024 } }
{ "tool_args": { "destination_path": "archive/note.txt", "allow_overwrite": true } }
```

Requests using unsupported fields are rejected before `SubmitJob` execution.

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
- `content_type` on tool result refs, so a `tool_result_recorded` event carries
  enough information to construct a `RenderToolOutput` `ToolResultRef`

`WatchEvents` accepts a cursor, returns a replay snapshot for that cursor, and
then keeps streaming new ordered events. The reconnect guarantee is
process-local: within the same daemon process, clients can resume the live
surface without losing job history. After a daemon restart, call
`GetProjectStatus` to discover the latest durable state, or `ReplayEvents` if
you need the bounded replay-only compatibility path.
Replay and query requests that name an unknown event id still return
`INVALID_REQUEST`. If the live watch stream loses continuity, `WatchEvents`
returns `CURSOR_EXPIRED` and clients should refresh status. Malformed page
tokens or replay/query cursor syntax still return `INVALID_REQUEST`.

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

### Package Trust Index

`ListPackageTrustIndex` is read-only. It returns a package projection with:

- `package_id`
- `version`
- `status`
- `boundary`
- `manifest_digest`
- `artifact_digest`
- source snapshot, including lineage and publication
- block marker

It preserves the installed source, provenance, and publication snapshot already
present in the record. It does not add install/update/execute flows, or
audit/quarantine history, and it intentionally omits mutable install-only
fields such as approved permissions and rollback snapshots.

## Event Ordering

Event ordering is per daemon store:

- each event receives a monotonically increasing sequence number;
- events are append-only;
- retries must not create duplicate semantic effects;
- clients resume with the last seen sequence number or event id.

If the daemon cannot guarantee continuity within the current process,
`WatchEvents` returns a `CURSOR_EXPIRED` recovery error and tells the client to
call `GetProjectStatus`. After a restart, clients should use
`GetProjectStatus` for the latest durable snapshot or `ReplayEvents` for the
bounded replay-only semantics that beta clients still depend on.
Replay and query requests that name an unknown event id still return
`INVALID_REQUEST`.

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
- `WatchEvents` gives a live stream for job, policy, audit, and repository
  changes, with `ReplayEvents` preserved for compatibility.
- `RenderToolOutput` avoids re-running tools only to get a different format.
- Policy errors include next states so the agent knows whether to ask the human,
  retry with narrower scope, or stop.
