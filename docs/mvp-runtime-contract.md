# MVP Runtime Contract

This document defines the first concrete runtime contract for Atelia Secretary.
It is intentionally smaller than the full Atelia architecture. The goal is to
give maintainers and agents a stable implementation target for the first usable
daemon loop.

## Goal

The MVP daemon must be able to sit between an Atelia client and a local project
workspace, expose typed state, accept bounded work requests, enforce policy, and
record what happened.

The MVP is not a general automation platform yet. It is the smallest runtime
that proves the Atelia boundary:

- clients do not execute privileged work directly;
- Secretary-visible policy is checked before work runs;
- work creates structured records that can be inspected later;
- tool output has a canonical result independent of TOON / JSON rendering;
- extension points are reserved without committing to the full extension host.

## Runtime Shape

The MVP daemon is a long-running Rust process.

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

## First Protocol Surface

The first protocol should grow from the current health endpoint into a small,
versioned service surface.

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

## Core Domain Records

The daemon should persist these records first.

| Record | Purpose | Notes |
| --- | --- | --- |
| repository | registered workspace root and trust settings | includes display name, root path, allowed path scope, timestamps |
| job | user or agent requested work | includes kind, goal, repository id, status, requester, created/started/completed timestamps |
| job_event | observable job lifecycle event | append-only; supports streaming and replay |
| policy_decision | allow / audit / approval / block result | includes risk tier, reason, policy version, requested capability |
| tool_invocation | one built-in or extension tool call | includes tool id, input digest, permission, status, output ref |
| tool_result | canonical structured result | independent of rendered format |
| audit_record | durable execution and policy record | append-only; redacted where needed |

The MVP can use a simple local database or file-backed store. The storage
choice is less important than keeping these records explicit and append-friendly.

## Built-In Tool Boundary

The MVP built-ins should stay small.

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
- memory providers;
- notification providers;
- approval agents;
- delegated agent providers.

Git can start as shell/process usage through the bounded process tool. A richer
Git surface can become an official extension later.

## Policy Minimum

Every job and tool invocation receives a policy decision before execution.

The MVP policy engine must support:

- R0 informational actions;
- R1 bounded read actions;
- R2 audited local changes;
- R3 approval-gated actions;
- R4 blocked actions.

R3 and R4 may initially be conservative. If the approval flow is not implemented
yet, R3 should return `needs_approval` rather than running.

Policy decisions must be inspectable by clients and recorded in audit records.

## Tool Output Minimum

The daemon stores canonical tool results and renders them separately.

The MVP renderer must support:

- default TOON for agent-facing output;
- JSON for integration and debugging;
- temporary format override per request;
- per-tool default format metadata;
- truncation metadata and redaction markers.

The daemon should not compare output formats as "raw log vs structured record."
Each tool contract should define fields, ordering, omissions, references, and
redundancy intentionally.

## Extension Boundary Reserved For MVP

The MVP does not need full extension installation.

It must reserve the following concepts in domain and protocol naming:

- tool provider;
- service provider;
- hook provider;
- surface attachment;
- permission request;
- output customizer;
- approval agent;
- delegated agent provider;
- OM / memory provider.

The first implementation may expose only built-in providers. It should not bake
GitHub, Linear, memory, or external agents into Secretary core.

## Out Of Scope

The MVP explicitly does not include:

- full Rust / WASM extension runtime;
- third-party extension registry;
- extension bundles;
- service-to-service extension calls;
- human approval UI beyond returning approval-needed state;
- long-term memory or preference storage;
- autonomous delegated agent scheduling;
- hosted synchronization;
- multi-user organization policy;
- production secret vaulting.

These are important, but they should build on the MVP records and policy
boundary instead of preceding them.

## Acceptance Criteria

The first MVP implementation is complete when:

- the daemon starts and reports health with protocol/storage versions;
- a client can register a repository;
- a client can submit, list, inspect, and cancel jobs;
- job lifecycle events can be streamed or replayed;
- policy decisions are recorded before execution;
- bounded filesystem and process tools run under policy;
- tool results are stored canonically and rendered as TOON or JSON;
- audit records show requester, policy decision, tool effect, and output refs;
- R3/R4 actions do not silently execute;
- docs and protocol definitions agree on names and status values.
