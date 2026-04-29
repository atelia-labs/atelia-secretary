# Architecture

Atelia Secretary は、Atelia の backend daemon です。クライアント、extension、Hook の規範的仕様は [`atelia`](https://github.com/atelia-labs/atelia/blob/main/README.ja.md) リポジトリで扱い、この文書では daemon 実装の境界だけを扱います。

## 全体像

```text
Atelia clients and agents
          |
    Atelia Protocol
          |
   Atelia Secretary
   Rust daemon in Docker
          |
repositories, GitHub, policy,
jobs, events, AX Feedback,
audit logs, execution boundaries
```

## バックエンド

Secretary service のバックエンドは Rust のみで実装します。

初期バックエンド crate の設計上の境界:

- `atelia-core`: ドメインモデルとポリシープリミティブ
- `ateliad`: daemon binary と service runtime
- `atelia-protocol`: 生成または手書きの protocol bindings
- `atelia-github`: GitHub integration boundary
- `atelia-agents`: agent provider と execution abstraction

daemon は長時間動作するプロセスです。配布と実行の主なターゲットは Docker です。

ホスト側の daemon は Linux / macOS / Windows のいずれでも動くことを想定します。初期クライアントは Apple platform に制約されますが、daemon の conceptual model は Apple 固有にしません。

## プロトコル

デフォルトのプロトコル方針は、Protocol Buffers と型付き RPC transport です。最初の有力候補は gRPC です。Rust と Swift のサポートが成熟しており、streaming event surface も扱いやすいためです。

プロトコルは次のものをサポートする必要があります。

- daemon health
- repository registration
- project status
- job creation and observation
- event streaming
- AX Feedback submission
- policy status
- audit trails
- client capability discovery

transport の選択は変わっても構いません。ただし、ドメイン契約は安定させ、versioned にします。

## 実行境界

Atelia Secretary は、Atelia の project-level 仕様に従って extension と Hook の実行境界を実装します。

daemon 側の責務:

- manifest と compatibility contract の検証
- extension / Hook の実行権限チェック
- policy に基づく許可、拒否、承認要求
- audit log の記録
- GitHub、repository、secret、外部サービスへのアクセス境界
- 危険な実行経路の block

規範的な extension / Hook 仕様は Atelia 本体の文書を参照します。

- [Custom AX Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.ja.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.ja.md)
- [Extension Security](https://github.com/atelia-labs/atelia/blob/main/docs/extension-security.ja.md)
