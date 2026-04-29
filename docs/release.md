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
- Docker build;
- security-sensitive change review where applicable.

## Changelog

The changelog should distinguish:

- user-facing changes;
- daemon behavior changes;
- protocol changes;
- policy / orchestration changes;
- security fixes;
- breaking changes.
