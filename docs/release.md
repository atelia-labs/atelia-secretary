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

A release should require:

- formatting;
- tests;
- linting;
- protocol compatibility checks;
- previous-minor protocol compatibility checks;
- manifest schema fixture validation;
- extension runtime compatibility checks;
- tool-output TOON / JSON golden fixtures;
- tool-output customizer compatibility fixtures;
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
