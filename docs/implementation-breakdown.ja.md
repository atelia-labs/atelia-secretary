# Implementation Breakdown

この文書は、Secretary runtime design を implementation slice に変換します。各 slice は coding agent が ownership を持ち、verify できる大きさにします。

## Slice 1: Runtime Domain Models

Repository: `atelia-secretary`

Owns:

- ids
- repository record
- job record
- event record
- policy decision
- lock decision
- tool invocation
- tool result
- audit record

Verification:

- serialization tests
- enum round-trip tests
- status transition tests

## Slice 2: Store Abstraction

Owns:

- storage trait
- in-memory test store
- file-backed or embedded store decision
- schema version reporting
- lock decision persistence

Verification:

- create/list/get records
- append events
- replay from cursor
- redaction marker preservation

## Slice 3: Protocol Expansion

Owns:

- Health、repository、job、event、policy、tool output の proto messages
- codegen を導入する場合は generated bindings
- compatibility notes

Verification:

- proto lint/build where available
- golden message examples

## Slice 4: Daemon Service Skeleton

Owns:

- RPC server wiring
- health endpoint
- repository registration/listing
- project status summary

Verification:

- daemon starts
- health returns protocol/storage versions
- register/list repository round trip

## Slice 5: Job Lifecycle

Owns:

- submit job
- list/get job
- cancel job
- state transitions
- job events

Verification:

- queued -> running -> succeeded
- queued -> blocked
- running -> cancel_requested -> canceled
- event replay

## Slice 6: Policy Engine Stub

Owns:

- risk tier model
- policy inputs
- default outcomes
- audit coupling

Verification:

- R1 read allowed/audited
- R2 write audited
- R3 returns `needs_approval`
- R4 blocked

## Slice 7: Built-In Read Tools

Owns:

- repository scope 内の fs list/search/stat/diff
- path normalization
- symlink escape rejection

Verification:

- scope checks
- result records
- truncation metadata

## Slice 8: Built-In Mutation And Process Tools

Owns:

- policy 背後の fs patch/write
- mutation 前の repository/path lock decision
- explicit argv process execution
- cwd validation
- env allowlist
- timeout and cancellation

Verification:

- audited write
- blocked out-of-scope write
- process success/failure
- timeout
- restart 時の stale lock reclaim

## Slice 9: Tool Output Rendering

Owns:

- canonical tool result envelope
- TOON rendering
- JSON rendering
- render format override

Verification:

- same result renders to TOON and JSON
- redaction/truncation markers preserved
- schema version included

## Slice 10: Client Contract Handoff

Owns:

- `atelia-kit` への mapping document
- example client calls
- known deferred fields

Verification:

- `atelia-kit` issue が missing state を invent せず shared model を実装できる

## Sequencing

slices 1-5 が realistic state を提供できる程度に安定する前に、client surface implementation を始めません。client design は mockup として parallel に進めても構いませんが、shared model は protocol と domain record に従います。
