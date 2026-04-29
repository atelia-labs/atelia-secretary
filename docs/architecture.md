# Architecture

Atelia Secretary is the backend daemon for Atelia. Normative client, extension,
and Hook specifications live in the [`atelia`](https://github.com/atelia-labs/atelia)
repository. This document covers only the daemon implementation boundary.

## Shape

```text
Atelia clients and agents
          |
    Atelia Protocol
          |
   Atelia Secretary
   Rust daemon in Docker
          |
repositories, GitHub, policy,
jobs, events, AX Feedback,
audit logs, execution boundaries
```

## Backend

The backend is Rust-only for the Secretary service.

Initial backend crate boundaries:

- `atelia-core`: domain model and policy primitives.
- `ateliad`: daemon binary and service runtime.
- `atelia-protocol`: generated or hand-authored protocol bindings.
- `atelia-github`: GitHub integration boundary.
- `atelia-agents`: agent provider and execution abstractions.

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

The transport choice can evolve, but the domain contracts should remain stable
and versioned.

## Execution Boundaries

Atelia Secretary implements extension and Hook execution boundaries according to
project-level Atelia specifications.

Daemon responsibilities:

- validating manifests and compatibility contracts;
- checking extension / Hook execution permissions;
- allowing, denying, or requesting approval according to policy;
- recording audit logs;
- enforcing access boundaries for GitHub, repositories, secrets, and external
  services;
- blocking dangerous execution paths.

See the project-level Atelia documents for normative extension and Hook specs.

- [Custom AX Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.md)
- [Extension Security](https://github.com/atelia-labs/atelia/blob/main/docs/extension-security.md)
