# Atelia Secretary

[日本語版 README](README.ja.md)

Atelia Secretary is the Rust backend daemon that runs the always-on project
secretary inside Atelia.

Project-wide philosophy, AX principles, Custom AX extensions, Hooks, client UX,
and governance live in the [`atelia`](https://github.com/atelia-labs/atelia) repository. This
repository focuses on the daemon implementation that executes those contracts.

## Scope

- Rust backend daemon
- distribution and runtime through Docker
- repository registration and project status
- job scheduling / observation
- agent delegation substrate
- policy enforcement
- AX Feedback storage and connection to Atelia-level workflows
- extension host and capability boundary
- execution ledger and daemon logs
- implementation of extension / Hook execution boundaries

## Out of Scope

- Atelia-wide philosophy and specifications
- Mac / iOS client UI
- shared Swift logic in Atelia Kit
- normative Custom AX extension specification
- normative Hooks specification

## Docs

- [Secretary Philosophy](docs/philosophy/secretary.md)
- [Architecture](docs/architecture.md)
- [MDP Runtime Contract](docs/mdp-runtime-contract.md)
- [Tool Catalog](docs/tool-catalog.md)
- [Tool Definition Schema](docs/tool-definition-schema.md)
- [Tool Output Schema](docs/tool-output-schema.md)
- [Extensions Runtime](docs/extensions-runtime.md)
- [Operational AX Analytics](docs/operational-ax-analytics.md)
- [Security](docs/security.md)
- [Release Policy](docs/release.md)
- [ADR 0001](docs/adr/0001-rust-daemon-native-clients.md)
- [AI Agent Harness Research](docs/research/agent-harness-survey.md)

Project-level docs:

- [Atelia](https://github.com/atelia-labs/atelia)
- [AX Feedback](https://github.com/atelia-labs/atelia/blob/main/docs/ax-feedback.md)
- [Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.md)
- [Extension Composition](https://github.com/atelia-labs/atelia/blob/main/docs/extension-composition.md)
- [Tool Output](https://github.com/atelia-labs/atelia/blob/main/docs/tool-output.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.md)
- [Client UX](https://github.com/atelia-labs/atelia/blob/main/docs/client-ux.md)

## Current Status

Atelia Secretary is in its early design and first-product implementation stage.
The first concrete work is to shape the Rust daemon, typed protocol, policy, job
orchestration, extension runtime, and Hook execution boundaries into a small but
trustworthy MDP.
