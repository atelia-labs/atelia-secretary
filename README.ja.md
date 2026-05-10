# Atelia Secretary

[English README](README.md)

Atelia Secretary は、Atelia に常駐するプロジェクト秘書を動かす Rust backend daemon です。

Atelia 全体の思想、AX 原則、AEP package 仕様、Surface Protocol、Hooks、client UX、ガバナンスは [`atelia`](https://github.com/atelia-labs/atelia/blob/main/README.ja.md) リポジトリで扱います。このリポジトリは、その仕様を実行する daemon 実装に集中します。

Atelia Secretary は [AEP](https://github.com/atelia-labs/atelia/blob/main/docs/aep.ja.md) の reference backend host です。Secretary-side runtime boundary、manifest validation slice、permission / capability enforcement、brokered services、Hook 実行境界、audit、registry / blocklist enforcement、install record、quarantine、revocation、rollback behavior を担当します。

## スコープ

- Rust backend daemon
- Docker を通じた配布と実行
- repository registration と project status
- job scheduling / observation
- agent delegation substrate
- policy enforcement
- AX Feedback の保存と Atelia レベルのワークフローへの接続
- AEP backend host と capability boundary
- execution ledger と daemon logs
- package / Hook 実行境界の実装

## 対象外

- Atelia 全体の思想と仕様
- Mac / iOS client UI
- Atelia Kit の共有 Swift ロジック
- AEP package の規範的仕様
- Hooks の規範的仕様

## ドキュメント

- [Docs index](docs/README.ja.md)

Core design:

- [Secretary の思想](docs/philosophy/secretary.ja.md)
- [Architecture](docs/architecture.ja.md)
- [Secretary Runtime Architecture](docs/runtime-architecture.ja.md)
- [Protocol Contract](docs/protocol-contract.ja.md)
- [Storage And Ledger Design](docs/storage-ledger.ja.md)
- [Policy And Approval Model](docs/policy-approval.ja.md)
- [Execution Semantics](docs/execution-semantics.ja.md)
- [Error And Recovery Taxonomy](docs/error-recovery.ja.md)
- [Agent Workflows And AX Review](docs/agent-workflows.ja.md)
- [Implementation Breakdown](docs/implementation-breakdown.ja.md)
- [Security](docs/security.ja.md)

Implementation contracts:

- [Tool Catalog](docs/tool-catalog.ja.md)
- [Tool Definition Schema](docs/tool-definition-schema.ja.md)
- [Tool Output Schema](docs/tool-output-schema.ja.md)
- [AEP Package Runtime](docs/extensions-runtime.ja.md)
- [Operational AX Analytics](docs/operational-ax-analytics.ja.md)

Release and research:

- [Release Policy](docs/release.ja.md)
- [ADR 0001](docs/adr/0001-rust-daemon-native-clients.ja.md)
- [AI agent harness research](docs/research/agent-harness-survey.ja.md)

プロジェクト全体のドキュメント:

- [Atelia](https://github.com/atelia-labs/atelia/blob/main/README.ja.md)
- [Package Authoring, Remix, and Discovery](https://github.com/atelia-labs/atelia/blob/main/docs/package-authoring-discovery.ja.md)
- [Package Sharing and Source Policy](https://github.com/atelia-labs/atelia/blob/main/docs/package-sharing-source-policy.ja.md)
- [AEP Manifest](https://github.com/atelia-labs/atelia/blob/main/docs/aep-manifest.ja.md)
- [AEP Services](https://github.com/atelia-labs/atelia/blob/main/docs/aep-services.ja.md)
- [Surface Protocol](https://github.com/atelia-labs/atelia/blob/main/docs/surface-protocol.ja.md)
- [AEP Registry](https://github.com/atelia-labs/atelia/blob/main/docs/aep-registry.ja.md)
- [Broker Boundary](https://github.com/atelia-labs/atelia/blob/main/docs/broker-boundary.ja.md)
- [AX Feedback](https://github.com/atelia-labs/atelia/blob/main/docs/ax-feedback.ja.md)
- [Tool Output](https://github.com/atelia-labs/atelia/blob/main/docs/tool-output.ja.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.ja.md)
- [Client UX](https://github.com/atelia-labs/atelia/blob/main/docs/client-ux.ja.md)

## 現在の状態

Atelia Secretary は初期設計と first product 実装の段階です。現在は Rust daemon architecture を、実装できる粒度まで具体化しています。対象は typed protocol、domain record、policy、job orchestration、execution ledger、tool execution、service brokering、AEP package boundary です。
beta protocol contract は `docs/protocol-contract.ja.md` で lock しており、shipping transport は HTTP/JSON です。generated proto/gRPC client / server path は future work です。
