# Secretary Runtime Architecture

This document defines the runtime architecture for Atelia Secretary. It is the
implementation anchor for the daemon: protocol surface, domain records, policy
boundary, audit model, tool execution, and extension seams.

The architecture is shaped by Atelia's MDP quality bar: Minimum Desirable /
Delightful Product. Desirable asks whether the product should exist in
someone's life and work. Delightful asks whether using it feels careful,
legible, and alive. The runtime should be small enough to build with discipline,
but complete enough to be trustworthy as the foundation of the product.

This is not a throwaway first-loop sketch. Later implementation slices may add
capabilities, storage engines, transports, extension hosts, and hosted sync, but
they should preserve the domain boundaries defined here.

## Design Commitments

The runtime architecture commits to:

- explicit domain records over implicit process state;
- append-friendly event and audit history;
- policy decisions before execution effects;
- canonical tool results before rendered output;
- typed protocol contracts before client-specific view state;
- provider boundaries before built-in service integrations;
- recoverable failure states before generic errors.

## Product Contract

The daemon sits between Atelia clients, agents, and local project workspaces. It
exposes typed state, accepts bounded work requests, enforces policy, records
what happened, and makes those boundaries legible to the person using it.

The runtime is not a generic automation platform. It is the product boundary
that makes Atelia possible:

- clients do not execute privileged work directly;
- Secretary-visible policy is checked before work runs;
- work creates structured records that can be inspected later;
- tool output has a canonical result independent of TOON / JSON rendering;
- status, errors, and approval states are understandable instead of merely
  technically correct;
- extension points are reserved without committing to the full extension host.

## Runtime Shape

The daemon is a long-running Rust process.

It owns:

- process lifecycle and health;
- repository registration;
- project and thread state snapshots;
- job submission, observation, cancellation, and completion;
- policy checks for each job and tool invocation;
- audit records for policy decisions and execution effects;
- tool result records and output rendering metadata;
- a small built-in tool surface.

It does not own:

- Mac or iOS UI state;
- long-term personal memory;
- arbitrary GitHub / Linear / external service integrations;
- delegated cloud agent orchestration;
- third-party extension installation.

Those surfaces are left to clients or extensions.

## Protocol Surface

The protocol grows from the current health endpoint into a versioned service
surface. These groups define the durable contract that clients and agents build
against.

Required RPC groups:

- `Health`: daemon status, daemon version, protocol version, storage state.
- `RegisterRepository`: declare a workspace root and return a repository id.
- `ListRepositories`: inspect registered repositories.
- `GetProjectStatus`: return current project, repository, job, and policy
  summary.
- `SubmitJob`: create a bounded job request.
- `GetJob`: inspect one job.
- `ListJobs`: inspect recent jobs with filters.
- `CancelJob`: request cancellation.
- `WatchEvents`: stream job, policy, audit, and repository events.
- `CheckPolicy`: preview whether a requested action is allowed, audited,
  approval-gated, or blocked.
- `RenderToolOutput`: render a canonical tool result as TOON, JSON, or text.

The initial transport may be gRPC or another typed RPC transport, but these
domain contracts should remain versioned and stable.

## Domain Records

The daemon persists explicit domain records. Storage can begin as a simple local
database or file-backed store, but record shape and append-friendly semantics are
part of the architecture.

| Record | Purpose | Notes |
| --- | --- | --- |
| repository | registered workspace root and trust settings | includes display name, root path, allowed path scope, timestamps |
| job | user or agent requested work | includes kind, goal, repository id, status, requester, created/started/completed timestamps |
| job_event | observable job lifecycle event | append-only; supports streaming and replay |
| policy_decision | allow / audit / approval / block result | includes risk tier, reason, policy version, requested capability |
| tool_invocation | one built-in or extension tool call | includes tool id, input digest, permission, status, output ref |
| tool_result | canonical structured result | independent of rendered format |
| audit_record | durable execution and policy record | append-only; redacted where needed |

## State Machines

The runtime should model state transitions explicitly rather than encoding them
as ad hoc strings in handlers.

Initial job states:

- `queued`: accepted but not started;
- `running`: execution has started;
- `cancel_requested`: cancellation has been requested;
- `succeeded`: completed without blocking errors;
- `failed`: completed with an execution or validation failure;
- `blocked`: policy or missing approval prevents execution;
- `canceled`: cancellation completed.

Initial policy outcomes:

- `allowed`: may run without extra audit beyond normal records;
- `audited`: may run and must produce audit evidence;
- `needs_approval`: must not run until an approval path is available;
- `blocked`: must not run.

Client-facing names should be stable, calm, and suitable for display. Internal
error detail can be richer, but every failure exposed to a client needs a
specific reason and a recoverable next state where recovery is possible.

## Built-In Tool Boundary

Built-ins stay small and dependable.

Include:

- filesystem read/list/search/stat/diff under registered repository scopes;
- filesystem write/patch only behind policy and audit;
- process execution for explicit argv commands with cwd, timeout, and env
  allowlist;
- policy check/request/status boundary;
- output render/negotiate/schema.

Defer:

- GitHub;
- Linear;
- cloud storage;
- browser / computer use;
- memory providers and memory strategies;
- notification providers;
- approval agents;
- delegated agent providers.

Git can start as shell/process usage through the bounded process tool. A richer
Git surface can become an official extension later.

## Policy Foundation

Every job and tool invocation receives a policy decision before execution.

The policy engine must support:

- R0 informational actions;
- R1 bounded read actions;
- R2 audited local changes;
- R3 approval-gated actions;
- R4 blocked actions.

R3 and R4 may initially be conservative. If the approval flow is not implemented
yet, R3 should return `needs_approval` rather than running.

Policy decisions must be inspectable by clients and recorded in audit records.

## Tool Output Foundation

The daemon stores canonical tool results and renders them separately.

The renderer must support:

- default TOON for agent-facing output;
- JSON for integration and debugging;
- temporary format override per request;
- per-tool default format metadata;
- truncation metadata and redaction markers.

The daemon should not compare output formats as "raw log vs structured record."
Each tool contract should define fields, ordering, omissions, references, and
redundancy intentionally.

## Extension Boundary

The runtime architecture reserves extension concepts even before full extension
installation exists.

It must reserve the following concepts in domain and protocol naming:

- tool provider;
- service provider;
- hook provider;
- surface attachment;
- permission request;
- output customizer;
- approval agent;
- delegated agent provider;
- memory provider;
- memory strategy.

The first implementation may expose only built-in providers. It should not bake
GitHub, Linear, long-term memory, observational memory, or external agents into
Secretary core.

## Deferred Product Surface

The runtime architecture explicitly defers:

- full Rust / WASM extension runtime;
- third-party extension registry;
- extension bundles;
- service-to-service extension calls;
- human approval UI beyond returning `needs_approval` state;
- long-term memory or preference storage;
- autonomous delegated agent scheduling;
- hosted synchronization;
- multi-user organization policy;
- production secret vaulting.

These are important, but they should build on the runtime records and policy
boundary instead of preceding them.

## Implementation Slices

The implementation should land in slices that preserve the architecture:

1. Daemon lifecycle and health with protocol/storage versions.
2. Repository registration and listing.
3. Job submission, listing, inspection, cancellation, and state transitions.
4. Event and audit persistence with replay.
5. Policy decision records before execution.
6. Bounded filesystem read/list/search/stat/diff.
7. Policy-gated filesystem write/patch.
8. Explicit argv process execution with cwd, timeout, and env allowlist.
9. Canonical tool results with TOON and JSON rendering.
10. Reserved provider identifiers for future extension boundaries.

## Acceptance Criteria

The runtime architecture is implemented when:

- the daemon starts and reports health with protocol/storage versions;
- a client can register a repository;
- a client can submit, list, inspect, and cancel jobs;
- job lifecycle events can be streamed or replayed;
- policy decisions are recorded before execution;
- bounded filesystem and process tools run under policy;
- tool results are stored canonically and rendered as TOON or JSON;
- audit records show requester, policy decision, tool effect, and output refs;
- R3/R4 actions do not silently execute;
- common failure states return specific, user-facing reasons and recoverable
  next states;
- event and job status names are calm, consistent, and suitable for native
  clients to display directly;
- docs and protocol definitions agree on names and status values.
