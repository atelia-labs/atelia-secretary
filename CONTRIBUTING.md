# Contributing

Atelia Secretary is being designed as an OSS project from the beginning.

The project welcomes contributions to:

- Rust daemon implementation;
- protocol design;
- client compatibility boundaries;
- documentation;
- security review;
- AX feedback design;
- issue intake and release hygiene.

## Project Posture

Atelia treats AI agents as end users. Contributions should preserve that
principle. If a change makes the system easier for humans but more confusing,
opaque, or unsafe for the agents doing work inside it, call that out in the pull
request.

## Pull Requests

Pull requests should include:

- a short summary;
- verification performed;
- risk notes;
- AX impact, when relevant.

High-risk automation changes require explicit policy review before they can be
merged.

## Development

```sh
cargo check --workspace
```

`rustfmt` and linting will become mandatory release gates once the toolchain is
fully pinned.
