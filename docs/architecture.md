# Architecture

Atelia Secretary is the backend daemon for Atelia. Normative client, extension,
and Hook specifications live in the [`atelia`](https://github.com/atelia-labs/atelia)
repository. This document covers only the daemon implementation boundary.

## Shape

Secretary exercises judgment through the protocol boundary. The daemon enforces
policy, persistence, and execution limits; Secretary decides what the work means
and how the workplace should evolve. When Secretary runs close to the daemon,
the same separation still applies.

```text
Atelia clients and agents
          |
    Atelia Protocol
          |
   Atelia Secretary
   Rust daemon in Docker
          |
repositories, extension host, policy,
jobs, events, AX Feedback,
execution ledger, execution boundaries
```

## Backend

The backend is Rust-only for the Secretary service.

Initial backend crate boundaries:

- `atelia-core`: domain model and policy primitives.
- `ateliad`: daemon binary and service runtime.
- `atelia-protocol`: generated or hand-authored protocol bindings.
- `atelia-extensions`: extension host, manifest, and capability boundaries.
- `atelia-agents`: agent delegation substrate and provider abstractions.

The daemon should be designed as a long-running process. Docker is the primary
distribution and runtime target.

The host daemon should be able to run on Linux, macOS, or Windows. Initial
clients are constrained to Apple platforms, but the daemon's conceptual model
should not be Apple-specific.

## Protocol

The default protocol direction is Protocol Buffers with a typed RPC transport.
The first serious candidate is gRPC because it has mature Rust and Swift
support and handles streaming event surfaces well.

The protocol must support:

- daemon health;
- repository registration;
- project status;
- job creation and observation;
- event streaming;
- AX Feedback submission;
- policy status;
- audit trails;
- client capability discovery.

The first implementation target for this surface is defined in
[MDP Runtime Contract](mdp-runtime-contract.md).

The transport choice can evolve, but the domain contracts should remain stable
and versioned.

Protocol message definitions live in the `atelia-protocol` crate once the first
wire contract is introduced. Until then, docs in this repository define domain
contracts and compatibility expectations.

## Execution Boundaries

Atelia Secretary implements extension and Hook execution boundaries according to
project-level Atelia specifications.

Daemon responsibilities:

- validating manifests and compatibility contracts;
- checking extension / Hook execution permissions;
- allowing, denying, or requesting approval according to policy;
- recording audit logs;
- enforcing access boundaries for repositories, secrets, extensions, and
  external services;
- blocking dangerous execution paths.

R0/R1 capabilities can be granted automatically by daemon policy when the
contract permits it. R2 capabilities require audit and checkpoint behavior where
applicable. R3/R4 capabilities require visible Secretary judgment and, when the
policy requires it, human approval.

See the project-level Atelia documents for normative extension and Hook specs.

- [Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.md)
- [Extension Composition](https://github.com/atelia-labs/atelia/blob/main/docs/extension-composition.md)
- [Tool Output](https://github.com/atelia-labs/atelia/blob/main/docs/tool-output.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.md)
- [Extension Security](https://github.com/atelia-labs/atelia/blob/main/docs/extension-security.md)

## Tool And Extension Implementation Notes

Secretary's implementation contracts for tool categories, tool output rendering,
extension runtime behavior, and operational AX analytics live in:

- [Tool Catalog](tool-catalog.md)
- [Tool Definition Schema](tool-definition-schema.md)
- [Tool Output Schema](tool-output-schema.md)
- [Extensions Runtime](extensions-runtime.md)
- [Operational AX Analytics](operational-ax-analytics.md)
- [MDP Runtime Contract](mdp-runtime-contract.md)
