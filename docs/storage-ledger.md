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

TOON (Token-Oriented Object Notation; see
[Tool Output Schema](tool-output-schema.md)), JSON, or text renderings are
derived views.

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
The record contains leader id, started timestamp, safe flag, and timeout.
Acquisition uses a unique key plus compare-and-set or transactional insert; it
fails if an unexpired `migration_lock` row already exists. All daemons interact
with the `migration_lock` record in `schema_migrations` to coordinate
create/update/read, but only one daemon may hold the lock at a time through
compare-and-set or transactional insert. While
`storage_status: migrating`, the leader must not accept or execute new external
mutating requests regardless of `safe_flag`. The leader may only drain existing
running jobs and record internal housekeeping in `schema_migrations`,
`migration_lock`, or the ledger, such as enqueueing retries or cleanup for
already-running jobs. Enqueued retries are persisted only as ledger entries and
are not executed until the migration lock is released or expires, unless
`safe_flag: true` and the operation is explicitly marked-safe. Running jobs are
drained until the configured timeout. If they cannot drain, retriable jobs are
enqueued for retry with backoff, while non-retriable work is marked `failed`
with cleanup steps recorded in the ledger. Non-leader daemons report
`storage_status: migrating` and do not accept new mutating work until the
migration lock is released or expires.

If a migration fails, the daemon records the transition from `storage_status:
migrating` to `degraded` or `read_only`, plus the failure reason, in
`schema_migrations` and the ledger before starting rather than silently
accepting new work. `running` jobs either continue draining until the configured
timeout or are recorded as paused when storage cannot safely continue. Retriable
work waiting on a missing `tool_result` is enqueued for retry with backoff;
non-retriable work records cleanup steps and is marked `failed`. `queued` jobs
remain pending, and no new tool invocation or mutation work starts while
`degraded` or `read_only`. `read_only` may allow bounded read, status, and
replay; `degraded` may allow only recovery housekeeping that has `safe_flag:
true` and is explicitly marked-safe.

## Retention

Initial retention is conservative:

- keep repository, job, policy, lock, and audit metadata indefinitely by default;
- keep recent event history for the configured full-event retention window;
- keep a minimal replay spine indefinitely by default: key job, policy, lock,
  audit, output-ref, digest, redaction, and terminal-state events needed to
  reconstruct what happened after the full event window expires;
- allow large tool artifacts to expire after the configured artifact retention
  window only when an output ref, digest, truncation metadata, and metadata
  tombstone remain;
- keep security-relevant audit evidence indefinitely unless an explicit
  compliance policy replaces sensitive fields with redaction markers;
- make retention configurable per data class with policy-versioned overrides.

PII deletion requests should prefer redaction over physical deletion when audit
continuity matters. Redaction records preserve id, timestamp, reason, actor, and
legal basis; only designated vault-backed processes may reverse reversible
redaction.

When audit continuity is not needed, a designated vault-backed deletion process
must perform physical deletion or crypto-shredding within the configured
deletion window after the PII deletion request is approved. Reversible redaction
is not allowed for data that a policy classifies as non-continuity PII,
user-requested hard deletion, revoked consent, or secret material. After
physical deletion, a minimal deletion record can persist for the configured
deletion-proof retention window, or a policy-versioned override, and should
contain only id, timestamp, actor, legal basis, and non-sensitive proof of
deletion, not deleted PII or reversible redaction material. Only a designated
vault-backed process may reverse reversible redaction, and that path is
unavailable for physical deletion or crypto-shredded data.

## AX Check

The ledger should help agents orient quickly:

- one job id links to policy, retained event spine, tool calls, outputs, and
  audit records;
- full event replay lets an agent recover within the configured full-event
  retention window; after that window, the minimal replay spine still lets the
  agent answer what happened without asking the user;
- redaction markers tell the agent that data existed but was intentionally
  hidden;
- canonical tool results let agents change output format without rerunning work.
