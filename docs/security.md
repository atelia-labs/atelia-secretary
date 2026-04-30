# Security

Atelia Secretary controls access to repositories, external services, and
automation agents. It must be designed as a security-sensitive daemon.

The normative security model for extensions and Hooks lives in
[`atelia/docs/extension-security.md`](https://github.com/atelia-labs/atelia/blob/main/docs/extension-security.md).
This document covers Secretary daemon-specific security boundaries.

## Baseline Rules

- External service tools execute only when the required CLI or API key is
  detected.
- If a tool is unavailable, return a structured unavailable status.
- Destructive repository actions require explicit policy support.
- Auto-merge is blocked until live policy checks are wired.
- Secrets must never be logged in plaintext.
- Clients should receive the minimum state needed for their role.
- The daemon should provide confirmation, audit, and recovery paths on the
  assumption that Secretary or humans can make mistakes.

## Threat Model Seeds

Initial threat model work should cover:

- malicious or confused agents;
- compromised external tool credentials;
- prompt injection through repository content or issue text;
- unsafe auto-fix loops;
- forged AX Feedback;
- replayed client requests;
- local daemon exposure;
- Docker socket and host filesystem boundaries;
- GitHub / repository abuse through the daemon.

## Related Enforcement Contracts

- [Tool Catalog](tool-catalog.md) defines capability areas and default risk
  tiers.
- [Tool Definition Schema](tool-definition-schema.md) defines tool identity,
  input schemas, effects, runtime behavior, and customization surfaces.
- [Tool Output Schema](tool-output-schema.md) defines agent-facing output,
  audit separation, redaction, and TOON/JSON format selection.
- [Extensions Runtime](extensions-runtime.md) defines manifest enforcement,
  extension sandboxing, provenance, rollback, and blocklist behavior.
- [Operational AX Analytics](operational-ax-analytics.md) defines
  privacy-preserving analytics for AX improvement.

## Disclosure

Until a formal security address exists, security reports should be handled
privately by maintainers in the `atelia-labs` organization.
