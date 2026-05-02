# Atelia Secretary Docs

This directory contains the daemon-side design and implementation contracts for
Atelia Secretary.

Project-wide product philosophy, client UX, extension specifications, hooks, and
governance live in [`atelia`](https://github.com/atelia-labs/atelia). This
repository explains how the Secretary daemon implements those contracts.

## Reading Order

1. [Secretary Philosophy](philosophy/secretary.md)
2. [Architecture](architecture.md)
3. [Secretary Runtime Architecture](runtime-architecture.md)
4. [Protocol Contract](protocol-contract.md)
5. [Storage And Ledger Design](storage-ledger.md)
6. [Policy And Approval Model](policy-approval.md)
7. [Execution Semantics](execution-semantics.md)
8. [Error And Recovery Taxonomy](error-recovery.md)
9. [Agent Workflows And AX Review](agent-workflows.md)
10. [Implementation Breakdown](implementation-breakdown.md)
11. [Tool Catalog](tool-catalog.md)
12. [Tool Definition Schema](tool-definition-schema.md)
13. [Tool Output Schema](tool-output-schema.md)
14. [Extensions Runtime](extensions-runtime.md)
15. [Security](security.md)

## Core Design

- [Architecture](architecture.md): daemon boundary, backend crates, protocol
  direction, and execution boundaries.
- [Secretary Runtime Architecture](runtime-architecture.md): durable runtime
  contract for domain records, protocol surface, state machines, policy, audit,
  tool execution, and implementation slices.
- [Protocol Contract](protocol-contract.md): RPC groups, message shape,
  identity, event ordering, compatibility, and protocol AX.
- [Storage And Ledger Design](storage-ledger.md): logical store, records,
  migrations, redaction, retention, and replay.
- [Policy And Approval Model](policy-approval.md): risk tiers, outcomes,
  approval requests, policy defaults, and audit coupling.
- [Execution Semantics](execution-semantics.md): job lifecycle, cancellation,
  timeouts, concurrency, filesystem scope, process execution, and tool output.
- [Error And Recovery Taxonomy](error-recovery.md): stable error shape, codes,
  next states, client display, and agent behavior.
- [Agent Workflows And AX Review](agent-workflows.md): realistic agent call
  sequences and runtime AX checklist.
- [Implementation Breakdown](implementation-breakdown.md): implementation
  slices suitable for agent-ready issues.
- [Security](security.md): baseline security rules and threat model seeds.
- [ADR 0001](adr/0001-rust-daemon-native-clients.md): Rust daemon with native
  clients decision.

## Tools And Output

- [Tool Catalog](tool-catalog.md)
- [Tool Definition Schema](tool-definition-schema.md)
- [Tool Output Schema](tool-output-schema.md)
- [Operational AX Analytics](operational-ax-analytics.md)

## Extensions

- [Extensions Runtime](extensions-runtime.md)

Normative extension, hook, and extension composition specs live in the project
repository:

- [Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.md)
- [Extension Composition](https://github.com/atelia-labs/atelia/blob/main/docs/extension-composition.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.md)

## Release And Research

- [Release Policy](release.md)
- [AI Agent Harness Research](research/agent-harness-survey.md)
