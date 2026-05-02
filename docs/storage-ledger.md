# Storage And Ledger Design

Atelia Secretary needs a local ledger before it needs a complex database. The
store must make work inspectable, replayable, redacted where needed, and stable
enough for clients and agents to trust.

## Principles

- Domain records are explicit.
- Events and audit records are append-friendly.
- Rendered tool output is not the source of truth.
- Redaction must not destroy the existence of an event.
- Schema migrations are versioned and recorded.

## Store Shape

The first store may be SQLite, another embedded database, or a file-backed
append log. The architecture requires these logical collections:

| Collection | Record |
| --- | --- |
| `repositories` | trusted workspace roots |
| `jobs` | requested work |
| `job_events` | ordered lifecycle and observation events |
| `policy_decisions` | policy outcome before execution |
| `tool_invocations` | attempted built-in or extension tool calls |
| `tool_results` | canonical structured outputs |
| `audit_records` | durable policy and execution evidence |
| `lock_decisions` | durable repository/path mutual-exclusion decisions |
| `schema_migrations` | applied storage migrations |

## Record Requirements

Every record includes:

- id
- schema version
- created timestamp
- updated timestamp where mutation is allowed
- redaction state

Append-only records use superseding events instead of in-place mutation.

## Atomicity And Ordering

Backends must provide one of these minimal storage primitives:

- a single multi-row transaction; or
- an atomic append / write-ahead-log commit with fsync-equivalent durability.

The first lifecycle boundary persists `job: queued` and the initial `job_event`
in one atomic commit. A `policy_decision` must be durably committed before any
`tool_invocation`, `tool_result`, or audit effect record can be created. On
restart, the daemon uses these durable boundaries to recover deterministically:
if `policy_decision` is missing, re-run policy evaluation; if a
`tool_invocation` exists without a `tool_result`, mark it for retry or cleanup
before accepting new work for that job.

## Repository Records

Repository records include:

- display name
- root path
- allowed path scope
- trust state
- owner hint
- last observed metadata

Blocked repositories remain in the store so audit records and job history still
resolve.

## Job Records

Job records include:

- requester
- repository id
- kind
- goal
- status
- policy summary
- cancellation state
- timestamps
- latest event id

Job status changes must create `job_event` records.

## Event Records

Events are the timeline clients and agents use to understand work.

Event records include:

- sequence number
- event kind
- subject type and id
- severity
- public message
- referenced ids
- redaction markers

Sequence numbers are strictly monotonic per daemon store and give a total order
for replay within that store.

Events should be compact enough to stream but complete enough to replay.

## Audit Records

Audit records answer: who or what asked for an action, what policy decided, what
ran, and what effect occurred.

Audit records include:

- actor / requester
- repository id
- requested capability
- policy decision id
- tool invocation id, if any
- effect summary
- output refs
- redactions

Audit records are append-only. If detail must be redacted later, preserve a
redacted record with the original id and redaction reason.

## Tool Results

Tool results store canonical structured data:

- tool id
- invocation id
- status
- schema ref
- typed fields
- evidence refs
- truncation metadata
- redaction metadata

TOON, JSON, or text renderings are derived views.

## Lock Decisions

`lock_decisions` record write exclusion before execution:

- repository id
- policy decision id
- owner job/process id
- locked path or repository scope
- `locked_at`
- `expires_at`
- `reclaimed_at`, when reclaimed
- status: `held`, `released`, `expired`, `reclaimed`

The daemon writes the lock decision before the protected effect. Active locks
are unique by `(repository_id, locked_scope, active status)`; `policy_decision_id`
is retained as linkage metadata and must not allow two active locks on the same
scope. Reclaim is safe-repeatable for the same lock decision and owner:
duplicate reclaim attempts return no-op success after the first persisted
reclaim record. On restart, the daemon reclaims expired locks by appending a
reclaim event with `lock_decision.id`, owner id, and `reclaimed_at`; only after
that durable record exists may it treat the lock as reclaimed. It then
re-evaluates the policy rule referenced by `policy_decision_id` before
continuing the job.

## Migration Policy

Storage migrations must be:

- ordered;
- idempotent where possible;
- recorded in `schema_migrations`;
- able to report `storage_status: migrating`;
- blocked from running while jobs are executing unless explicitly safe.

Before migration, the daemon must acquire a single migration lock stored as a
well-known `migration_lock` record inside the `schema_migrations` collection.
The record contains leader id, started timestamp, safe flag, and timeout. All
daemons create, update, or read that record to acquire or release the lock.
Running jobs are drained until the configured timeout; if they cannot drain, the
daemon enters `degraded` or `read_only` and records the failure. Non-leader
daemons report `storage_status: migrating` and do not accept new mutating work
until the migration lock is released or expires.

If a migration fails, the daemon should start in `degraded` or `read_only`
state rather than silently accepting new work.

## Retention

Initial retention is conservative:

- keep repository, job, policy, lock, and audit metadata indefinitely by default;
- keep recent event history for at least 90 days by default;
- keep a minimal replay spine indefinitely by default: key job, policy, lock,
  audit, output-ref, digest, redaction, and terminal-state events needed to
  reconstruct what happened after the full event window expires;
- allow large tool artifacts to expire after 30 days only when an output ref,
  digest, truncation metadata, and metadata tombstone remain;
- keep security-relevant audit evidence indefinitely unless an explicit
  compliance policy replaces sensitive fields with redaction markers;
- make retention configurable per data class with policy-versioned overrides.

PII deletion requests should prefer redaction over physical deletion when audit
continuity matters. Redaction records preserve id, timestamp, reason, actor, and
legal basis; only designated vault-backed processes may reverse reversible
redaction.

## AX Check

The ledger should help agents orient quickly:

- one job id links to policy, retained event spine, tool calls, outputs, and
  audit records;
- full event replay lets an agent recover within the configured retention
  window; after that window, the minimal replay spine still lets the agent
  answer what happened without asking the user;
- redaction markers tell the agent that data existed but was intentionally
  hidden;
- canonical tool results let agents change output format without rerunning work.
