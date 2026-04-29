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
- agent provider integration
- policy enforcement
- AX Feedback storage and connection to Atelia-level workflows
- GitHub integration boundary
- audit trails and daemon logs
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
- [Security](docs/security.md)
- [Release Policy](docs/release.md)
- [ADR 0001](docs/adr/0001-rust-daemon-native-clients.md)
- [AI Agent Harness Research](docs/research/agent-harness-survey.md)

Project-level docs:

- [Atelia](https://github.com/atelia-labs/atelia)
- [AX Feedback](https://github.com/atelia-labs/atelia/blob/main/docs/ax-feedback.md)
- [Custom AX Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.md)
- [Client UX](https://github.com/atelia-labs/atelia/blob/main/docs/client-ux.md)

## Current Status

Atelia Secretary is in its early design and minimal implementation stage. The
first concrete work is to shape the Rust daemon, typed protocol, policy, job
orchestration, GitHub integration, and extension / Hook execution boundaries.
