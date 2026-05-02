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
4. [Tool Catalog](tool-catalog.md)
5. [Tool Definition Schema](tool-definition-schema.md)
6. [Tool Output Schema](tool-output-schema.md)
7. [Extensions Runtime](extensions-runtime.md)
8. [Security](security.md)

## Core Design

- [Architecture](architecture.md): daemon boundary, backend crates, protocol
  direction, and execution boundaries.
- [Secretary Runtime Architecture](runtime-architecture.md): durable runtime
  contract for domain records, protocol surface, state machines, policy, audit,
  tool execution, and implementation slices.
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
