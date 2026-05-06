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

## Beta Network Boundary

During beta, Secretary is a same-host daemon by default. It listens on
`127.0.0.1:8080` unless `ATELIA_DAEMON_LISTEN_ADDR` is set to another loopback
address. Bind attempts to non-loopback addresses are rejected unless the
explicit unsafe escape hatch `ATELIA_DAEMON_UNSAFE_ALLOW_NON_LOOPBACK_LISTEN=1`
is configured. That override is for controlled local testing only and should
not be treated as a normal deployment mode.

Secretary also requires a local bearer token by default. On startup it creates
or reuses `<storage_dir>/daemon-auth.token` and expects every request to carry
`Authorization: Bearer <token>`. A deliberate opt-out,
`ATELIA_DAEMON_AUTH_DISABLED=1`, exists for controlled local testing only and
should not be used as a normal deployment mode. `ATELIA_DAEMON_AUTH_TOKEN`
may be set to pin a specific token for automation, but it should still be
treated as a local secret. Existing token files are normalized to restrictive
permissions before reuse, and the auth-disabled opt-out is rejected if it is
paired with an unsafe non-loopback listener override.

Token generation follows these requirements:

- use the system CSPRNG;
- generate at least 32 bytes of raw entropy;
- encode the token as hex or base64url without padding;
- harden `<storage_dir>/daemon-auth.token` to owner-only access before reuse.

## Local Token Lifecycle

- The local bearer token does not expire on its own. The generated
  `<storage_dir>/daemon-auth.token` is reused until it is deleted or replaced,
  and `ATELIA_DAEMON_AUTH_TOKEN` stays active for as long as the process reads
  that value.
- There is no automatic rotation. To rotate a generated token safely, stop or
  quiesce dependent clients, delete `<storage_dir>/daemon-auth.token`, restart
  Secretary, and then update clients with the newly generated token.
- To rotate a pinned token, set a new `ATELIA_DAEMON_AUTH_TOKEN` value, update
  every client to the same value, and restart Secretary and the clients in a
  controlled cutover. Do not reuse the old value after cutover.
- If a token is exposed or suspected compromised, treat it as revoked: replace
  the daemon token, invalidate the stored file or pinned environment value,
  restart the daemon, update every dependent automation, and review the access
  path that leaked it.
- For automation, prefer a pinned token only when the secret can be stored in a
  proper secret manager or other access-controlled runtime secret store.
  Never commit the token to version control, shared config, or CI variables.
  Backups and shared machine images must not capture the token in plaintext, and
  any automation that reads it should run with the minimum required access.
- On Unix, the token file is created and normalized with restrictive
  permissions so the owning user can read it. If the file is copied, restored,
  or mounted from elsewhere, verify that the running user still owns it and can
  read it before relying on reuse. If the environment can only provide
  `0400`, that is still acceptable as long as the file remains owner-only
  readable.

## Replay Protection

Accepted beta limitation: Secretary does not yet provide built-in replay
protection or token expiry for local auth. A captured authorized request can be
replayed until the token changes or the local boundary is removed.

Secretary currently relies on two local-boundary mitigations:

- the daemon binds to loopback only by default, so traffic is limited to the
  local host unless the unsafe non-loopback override is enabled;
- the daemon requires a bearer token on each request unless the local auth
  opt-out is enabled.

Recommended mitigations:

- keep the daemon loopback-only and leave local auth enabled;
- use a unique token per host or per daemon instance;
- rotate the token on a regular cadence, around every 30 days, and
  immediately after exposure or suspected interception;
- monitor token usage and keep an audit trail for access and rotation;
- avoid putting the daemon behind a shared listener, reverse proxy, or other
  boundary that would make captured requests easier to reuse.

Roadmap for stronger replay protection:

- short-lived tokens;
- refresh flow for token renewal;
- request signing with HMAC plus nonce and timestamp;
- server-side nonce tracking;
- token rotation support as a first-class operation.

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
