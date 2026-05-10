# Atelia Secretary

[日本語版 README](README.ja.md)

Atelia Secretary is the Rust backend daemon that runs the always-on project
secretary inside Atelia.

Project-wide philosophy, AX principles, AEP package specifications, Surface
Protocol, Hooks, client UX, and governance live in the
[`atelia`](https://github.com/atelia-labs/atelia) repository. This repository
focuses on the daemon implementation that executes those contracts.

Atelia Secretary is the reference backend host for
[AEP](https://github.com/atelia-labs/atelia/blob/main/docs/aep.md). It owns
the Secretary-side runtime boundary, manifest validation slice, permission and
capability enforcement, brokered services, hook execution boundaries, audit,
registry / blocklist enforcement, install records, quarantine, revocation, and
rollback behavior.

## Scope

- Rust backend daemon
- distribution and runtime through Docker
- repository registration and project status
- job scheduling / observation
- agent delegation substrate
- policy enforcement
- AX Feedback storage and connection to Atelia-level workflows
- AEP backend host and capability boundary
- execution ledger and daemon logs
- implementation of package / Hook execution boundaries

## Out of Scope

- Atelia-wide philosophy and specifications
- Mac / iOS client UI
- shared Swift logic in Atelia Kit
- normative AEP package specification
- normative Hooks specification

## Docs

- [Docs index](docs/README.md)

Core design:

- [Secretary Philosophy](docs/philosophy/secretary.md)
- [Architecture](docs/architecture.md)
- [Secretary Runtime Architecture](docs/runtime-architecture.md)
- [Protocol Contract](docs/protocol-contract.md)
- [Storage And Ledger Design](docs/storage-ledger.md)
- [Policy And Approval Model](docs/policy-approval.md)
- [Execution Semantics](docs/execution-semantics.md)
- [Error And Recovery Taxonomy](docs/error-recovery.md)
- [Agent Workflows And AX Review](docs/agent-workflows.md)
- [Implementation Breakdown](docs/implementation-breakdown.md)
- [Security](docs/security.md)

Implementation contracts:

- [Tool Catalog](docs/tool-catalog.md)
- [Tool Definition Schema](docs/tool-definition-schema.md)
- [Tool Output Schema](docs/tool-output-schema.md)
- [AEP Package Runtime](docs/extensions-runtime.md)
- [Operational AX Analytics](docs/operational-ax-analytics.md)

Release and research:

- [Release Policy](docs/release.md)
- [ADR 0001](docs/adr/0001-rust-daemon-native-clients.md)
- [AI Agent Harness Research](docs/research/agent-harness-survey.md)

Project-level docs:

- [Atelia](https://github.com/atelia-labs/atelia)
- [Package Authoring, Remix, and Discovery](https://github.com/atelia-labs/atelia/blob/main/docs/package-authoring-discovery.md)
- [Package Sharing and Source Policy](https://github.com/atelia-labs/atelia/blob/main/docs/package-sharing-source-policy.md)
- [AEP Manifest](https://github.com/atelia-labs/atelia/blob/main/docs/aep-manifest.md)
- [AEP Services](https://github.com/atelia-labs/atelia/blob/main/docs/aep-services.md)
- [Surface Protocol](https://github.com/atelia-labs/atelia/blob/main/docs/surface-protocol.md)
- [AEP Registry](https://github.com/atelia-labs/atelia/blob/main/docs/aep-registry.md)
- [Broker Boundary](https://github.com/atelia-labs/atelia/blob/main/docs/broker-boundary.md)
- [AX Feedback](https://github.com/atelia-labs/atelia/blob/main/docs/ax-feedback.md)
- [Tool Output](https://github.com/atelia-labs/atelia/blob/main/docs/tool-output.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.md)
- [Client UX](https://github.com/atelia-labs/atelia/blob/main/docs/client-ux.md)

## Current Status

Atelia Secretary is in its early design and first-product implementation stage.
The current work is to make the Rust daemon architecture concrete enough for
implementation: typed protocol, domain records, policy, job orchestration,
execution ledger, tool execution, service brokering, and AEP package
boundaries. The beta protocol
contract is locked in `docs/protocol-contract.md`; the shipping transport is
HTTP/JSON, and generated proto/gRPC client and server paths remain future work.
