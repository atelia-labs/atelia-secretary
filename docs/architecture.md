# Architecture

Atelia Secretary is the backend daemon for Atelia. Normative client, AEP
package, Surface Protocol, and Hook specifications live in the
[`atelia`](https://github.com/atelia-labs/atelia) repository. This document
covers only the daemon implementation boundary.

Within AEP, Secretary is the reference backend host. It implements backend
runtime boundaries, permission and capability enforcement, service brokering,
hook execution boundaries, audit, registry / blocklist enforcement, install
records, quarantine, revocation, and rollback.

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
repositories, AEP backend host, policy,
jobs, events, AX Feedback,
execution ledger, execution boundaries
```

## Backend

The backend is Rust-only for the Secretary service.

Initial backend crate boundaries:

- `atelia-core`: domain model and policy primitives.
- `ateliad`: daemon binary and service runtime.
- `atelia-protocol`: generated or hand-authored protocol bindings.
- `atelia-extensions`: current beta crate for AEP backend package manifests,
  host runtime, and capability boundaries.
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

The durable implementation target for this surface is defined in
[Secretary Runtime Architecture](runtime-architecture.md).

The transport choice can evolve, but the domain contracts should remain stable
and versioned.

Protocol message definitions live in the `atelia-protocol` crate once the first
wire contract is introduced. Until then, docs in this repository define domain
contracts and compatibility expectations.

## Execution Boundaries

Atelia Secretary implements AEP package, service broker, and Hook execution
boundaries according to project-level Atelia specifications.

Daemon responsibilities:

- validating manifests, source policy, registry metadata, and compatibility
  contracts;
- checking package / Hook execution permissions;
- allowing, denying, or requesting approval according to policy;
- recording audit logs;
- enforcing access boundaries for repositories, secrets, packages, brokered
  services, and external services;
- blocking dangerous execution paths.

Secretary does not gate raw GitHub publication. A user can create a repository,
fork, branch, release, or pull request outside Atelia. Secretary gates what
Atelia can resolve, search, install, mount, quarantine, revoke, and execute
through registry submission, source-policy checks, manifest validation,
permission analysis, service broker policy, and audit.

R0/R1 capabilities can be granted automatically by daemon policy when the
contract permits it. R2 capabilities require audit and checkpoint behavior where
applicable. R3/R4 capabilities require visible Secretary judgment and, when the
policy requires it, human approval.

See the project-level Atelia documents for normative AEP package, Surface
Protocol, registry, service, broker, and Hook specs.

- [Package Authoring, Remix, and Discovery](https://github.com/atelia-labs/atelia/blob/main/docs/package-authoring-discovery.md)
- [Package Sharing and Source Policy](https://github.com/atelia-labs/atelia/blob/main/docs/package-sharing-source-policy.md)
- [AEP Manifest](https://github.com/atelia-labs/atelia/blob/main/docs/aep-manifest.md)
- [AEP Services](https://github.com/atelia-labs/atelia/blob/main/docs/aep-services.md)
- [Surface Protocol](https://github.com/atelia-labs/atelia/blob/main/docs/surface-protocol.md)
- [AEP Registry](https://github.com/atelia-labs/atelia/blob/main/docs/aep-registry.md)
- [Broker Boundary](https://github.com/atelia-labs/atelia/blob/main/docs/broker-boundary.md)
- [Tool Output](https://github.com/atelia-labs/atelia/blob/main/docs/tool-output.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.md)

## Tool And Extension Implementation Notes

Secretary's implementation contracts for tool categories, tool output rendering,
AEP backend package runtime behavior, and operational AX analytics live in:

- [Tool Catalog](tool-catalog.md)
- [Tool Definition Schema](tool-definition-schema.md)
- [Tool Output Schema](tool-output-schema.md)
- [Extensions Runtime](extensions-runtime.md)
- [Operational AX Analytics](operational-ax-analytics.md)
- [Secretary Runtime Architecture](runtime-architecture.md)
