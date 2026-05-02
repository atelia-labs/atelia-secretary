# Implementation Breakdown

This document turns the Secretary runtime design into implementation slices.
Each slice should be small enough for a coding agent to own and verify.

## Slice 1: Runtime Domain Models

Repository: `atelia-secretary`

Owns:

- ids
- repository record
- job record
- event record
- policy decision
- tool invocation
- tool result
- audit record

Verification:

- serialization tests;
- enum round-trip tests;
- status transition tests.

## Slice 2: Store Abstraction

Owns:

- storage trait
- in-memory test store
- file-backed or embedded store decision
- schema version reporting

Verification:

- create/list/get records;
- append events;
- replay from cursor;
- redaction marker preservation.

## Slice 3: Protocol Expansion

Owns:

- proto messages for Health, repositories, jobs, events, policy, tool output;
- generated bindings if codegen is introduced;
- compatibility notes.

Verification:

- proto lint/build where available;
- golden message examples.

## Slice 4: Daemon Service Skeleton

Owns:

- RPC server wiring;
- health endpoint;
- repository registration/listing;
- project status summary.

Verification:

- daemon starts;
- health returns protocol/storage versions;
- register/list repository round trip.

## Slice 5: Job Lifecycle

Owns:

- submit job;
- list/get job;
- cancel job;
- state transitions;
- job events.

Verification:

- queued -> running -> succeeded;
- queued -> blocked;
- running -> cancel_requested -> canceled;
- event replay.

## Slice 6: Policy Engine Stub

Owns:

- risk tier model;
- policy inputs;
- default outcomes;
- audit coupling.

Verification:

- R1 read allowed/audited;
- R2 write audited;
- R3 returns `needs_approval`;
- R4 blocked.

## Slice 7: Built-In Read Tools

Owns:

- fs list/search/stat/diff under repository scope;
- path normalization;
- symlink escape rejection.

Verification:

- scope checks;
- result records;
- truncation metadata.

## Slice 8: Built-In Mutation And Process Tools

Owns:

- fs patch/write behind policy;
- explicit argv process execution;
- cwd validation;
- env allowlist;
- timeout and cancellation.

Verification:

- audited write;
- blocked out-of-scope write;
- process success/failure;
- timeout.

## Slice 9: Tool Output Rendering

Owns:

- canonical tool result envelope;
- TOON rendering;
- JSON rendering;
- render format override.

Verification:

- same result renders to TOON and JSON;
- redaction/truncation markers preserved;
- schema version included.

## Slice 10: Client Contract Handoff

Owns:

- document mapping to `atelia-kit`;
- example client calls;
- known deferred fields.

Verification:

- `atelia-kit` issue can implement shared models from the contract without
  inventing missing states.

## Sequencing

Do not begin client surface implementation before slices 1-5 are stable enough
to provide realistic state. Client design can proceed in parallel as mockups,
but shared models should follow the protocol and domain records.
