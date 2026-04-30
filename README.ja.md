# Atelia Secretary

[English README](README.md)

Atelia Secretary は、Atelia に常駐するプロジェクト秘書を動かす Rust backend daemon です。

Atelia 全体の思想、AX 原則、Custom AX extension、Hooks、client UX、ガバナンスは [`atelia`](https://github.com/atelia-labs/atelia/blob/main/README.ja.md) リポジトリで扱います。このリポジトリは、その仕様を実行する daemon 実装に集中します。

## スコープ

- Rust backend daemon
- Docker を通じた配布と実行
- repository registration と project status
- job scheduling / observation
- agent delegation substrate
- policy enforcement
- AX Feedback の保存と Atelia レベルのワークフローへの接続
- extension host と capability boundary
- execution ledger と daemon logs
- extension / Hook 実行境界の実装

## 対象外

- Atelia 全体の思想と仕様
- Mac / iOS client UI
- Atelia Kit の共有 Swift ロジック
- Custom AX extension の規範的仕様
- Hooks の規範的仕様

## ドキュメント

- [Secretary の思想](docs/philosophy/secretary.ja.md)
- [Architecture](docs/architecture.ja.md)
- [Tool Catalog](docs/tool-catalog.ja.md)
- [Tool Definition Schema](docs/tool-definition-schema.ja.md)
- [Tool Output Schema](docs/tool-output-schema.ja.md)
- [Extensions Runtime](docs/extensions-runtime.ja.md)
- [Operational AX Analytics](docs/operational-ax-analytics.ja.md)
- [Security](docs/security.ja.md)
- [Release Policy](docs/release.ja.md)
- [ADR 0001](docs/adr/0001-rust-daemon-native-clients.ja.md)
- [AI agent harness research](docs/research/agent-harness-survey.ja.md)

プロジェクト全体のドキュメント:

- [Atelia](https://github.com/atelia-labs/atelia/blob/main/README.ja.md)
- [AX Feedback](https://github.com/atelia-labs/atelia/blob/main/docs/ax-feedback.ja.md)
- [Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.ja.md)
- [Extension Composition](https://github.com/atelia-labs/atelia/blob/main/docs/extension-composition.ja.md)
- [Tool Output](https://github.com/atelia-labs/atelia/blob/main/docs/tool-output.ja.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.ja.md)
- [Client UX](https://github.com/atelia-labs/atelia/blob/main/docs/client-ux.ja.md)

## 現在の状態

Atelia Secretary は初期設計と最小実装の段階です。まずは Rust daemon、型付きプロトコル、policy、job orchestration、extension runtime、Hook 実行境界を小さく固めていきます。
