# ADR 0001: Rust Daemon With Native Clients

## Status

Proposed

## Context

Atelia Secretary needs predictable performance, memory safety, clean deployment,
and a boundary between backend orchestration and user-facing clients.

## Decision

Build Secretary as a Rust backend daemon distributed through Docker.

Build first-party clients as native Swift applications for macOS and iOS.

Do not build a TUI as an initial product surface.

Use a typed protocol between clients and daemon so future Linux and Windows
clients can be added without changing the backend's conceptual model.

## Consequences

- Backend implementation can focus on long-running reliability.
- Clients can use platform-native UX.
- Shared logic lives in protocol contracts and client SDKs, not in a shared web
  frontend.
- Early implementation effort is higher than a single TUI, but the product
  direction is clearer.
