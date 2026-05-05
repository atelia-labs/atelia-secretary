# Release Policy

Atelia Secretary should release boringly and predictably.

## Artifacts

Initial release artifacts:

- Docker image for `ateliad`;
- Rust crates when the crate boundary is stable enough;
- generated protocol artifacts;

## Versioning

Use semantic versioning for public APIs and release artifacts.

Protocol contracts must be versioned. Client and daemon compatibility should be
checked explicitly at connection time.

## Release Gates

Beta releases should require the following local and CI gates:

- `cargo fmt --all -- --check`
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
- `cargo test --locked --workspace --all-features`
- `docker build --file Dockerfile .`
- `docker run` smoke that starts `ateliad` with `ATELIA_DAEMON_LISTEN_ADDR=0.0.0.0:8080` and `ATELIA_DAEMON_UNSAFE_ALLOW_NON_LOOPBACK_LISTEN=1`, then probes `/v1/health`

The CI workflow runs the same gate set in `.github/workflows/ci.yml` as the
`Beta Release Gates` job, including the container smoke. If a future shared
workflow absorbs the Rust checks, keep this job as the packaging check or mark
the relevant step manual in both CI and this document.

An explicit non-loopback `ATELIA_DAEMON_LISTEN_ADDR` must fail at startup
unless `ATELIA_DAEMON_UNSAFE_ALLOW_NON_LOOPBACK_LISTEN` is set to a truthy
value such as `1` or `true`. Loopback and default-local binds stay allowed
without the unsafe opt-in.

In addition to the beta gate, a release should require:

- formatting;
- tests;
- linting;
- protocol compatibility checks;
- previous-minor protocol compatibility checks;
- manifest schema fixture validation;
- extension runtime compatibility checks;
- tool-output TOON / JSON golden fixtures;
- tool-output customizer compatibility fixtures;
- tool-output compatibility fixture checks under `crates/atelia-core/tests/fixtures/tool_output/`;
- extension compatibility matrix updates;
- AX impact review when tool-output defaults, permission model, extension
  contracts, or Hook behavior change;
- permission migration tests when permission names or risk tiers change;
- blocklist and rollback behavior tests when extension enforcement changes;
- Docker build;
- security-sensitive change review where applicable.

Any Secretary release that changes enforcement of Atelia extension, hook,
permission, or tool-output contracts must cite the Atelia specification version
or commit it implements.

## Changelog

The changelog should distinguish:

- user-facing changes;
- daemon behavior changes;
- protocol changes;
- policy / orchestration changes;
- extension / hook compatibility changes;
- tool-output schema and default-format changes;
- security fixes;
- breaking changes.
