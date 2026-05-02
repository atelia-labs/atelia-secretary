# Atelia Secretary Docs

この directory は、Atelia Secretary の daemon 側 design と implementation contract を扱います。

Atelia 全体の product philosophy、client UX、extension 仕様、Hook、governance は [`atelia`](https://github.com/atelia-labs/atelia/blob/main/README.ja.md) に置きます。この repository では、それらの contract を Secretary daemon がどう実装するかを定義します。

## 読む順番

1. [Secretary の思想](philosophy/secretary.ja.md)
2. [Architecture](architecture.ja.md)
3. [Secretary Runtime Architecture](runtime-architecture.ja.md)
4. [Tool Catalog](tool-catalog.ja.md)
5. [Tool Definition Schema](tool-definition-schema.ja.md)
6. [Tool Output Schema](tool-output-schema.ja.md)
7. [Extensions Runtime](extensions-runtime.ja.md)
8. [Security](security.ja.md)

## Core Design

- [Architecture](architecture.ja.md): daemon boundary、backend crate、protocol 方針、execution boundary
- [Secretary Runtime Architecture](runtime-architecture.ja.md): domain record、protocol surface、state machine、policy、audit、tool execution、implementation slice の durable runtime contract
- [Security](security.ja.md): baseline security rule と threat model の種
- [ADR 0001](adr/0001-rust-daemon-native-clients.ja.md): Rust daemon と native client の decision

## Tools And Output

- [Tool Catalog](tool-catalog.ja.md)
- [Tool Definition Schema](tool-definition-schema.ja.md)
- [Tool Output Schema](tool-output-schema.ja.md)
- [Operational AX Analytics](operational-ax-analytics.ja.md)

## Extensions

- [Extensions Runtime](extensions-runtime.ja.md)

規範的な extension、Hook、extension composition 仕様は project repository に置きます。

- [Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.ja.md)
- [Extension Composition](https://github.com/atelia-labs/atelia/blob/main/docs/extension-composition.ja.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.ja.md)

## Release And Research

- [Release Policy](release.ja.md)
- [AI agent harness research](research/agent-harness-survey.ja.md)
