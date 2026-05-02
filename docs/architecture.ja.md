# Architecture

Atelia Secretary は、Atelia の backend daemon です。クライアント、extension、Hook の規範的仕様は [`atelia`](https://github.com/atelia-labs/atelia/blob/main/README.ja.md) リポジトリで扱い、この文書では daemon 実装の境界だけを扱います。

## 全体像

Secretary は protocol boundary を通して判断します。daemon は policy、persistence、execution limit を enforce し、Secretary はその仕事が何を意味するか、仕事場をどう育てるかを判断します。Secretary が daemon の近くで動く場合も、この分離を維持します。

```text
Atelia clients and agents
          |
    Atelia Protocol
          |
   Atelia Secretary
   Rust daemon in Docker
          |
repositories, extension host, policy,
jobs, events, AX Feedback,
execution ledger, execution boundaries
```

## バックエンド

Secretary service のバックエンドは Rust のみで実装します。

初期バックエンド crate の設計上の境界:

- `atelia-core`: ドメインモデルとポリシープリミティブ
- `ateliad`: daemon binary と service runtime
- `atelia-protocol`: 生成または手書きの protocol bindings
- `atelia-extensions`: extension host、manifest、capability boundary
- `atelia-agents`: agent delegation substrate と provider abstraction

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

この surface の最初の実装目標は [MVP Runtime Contract](mvp-runtime-contract.ja.md) で定義します。

transport の選択は変わっても構いません。ただし、ドメイン契約は安定させ、versioned にします。

protocol message definition は、最初の wire contract を導入する時点で `atelia-protocol` crate に置きます。それまでは、この repository の docs が domain contract と compatibility expectation を定義します。

## 実行境界

Atelia Secretary は、Atelia の project-level 仕様に従って extension と Hook の実行境界を実装します。

daemon 側の責務:

- manifest と compatibility contract の検証
- extension / Hook の実行権限チェック
- policy に基づく許可、拒否、承認要求
- audit log の記録
- repository、secret、extension、外部サービスへのアクセス境界
- 危険な実行経路の block

R0/R1 capability は contract が許す範囲で daemon policy により自動 grant できます。R2 capability は audit と必要に応じた checkpoint behavior を要求します。R3/R4 capability は、Secretary の可視化された判断と、policy が要求する場合の human approval を経て grant します。

規範的な extension / Hook 仕様は Atelia 本体の文書を参照します。

- [Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.ja.md)
- [Extension Composition](https://github.com/atelia-labs/atelia/blob/main/docs/extension-composition.ja.md)
- [Tool Output](https://github.com/atelia-labs/atelia/blob/main/docs/tool-output.ja.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.ja.md)
- [Extension Security](https://github.com/atelia-labs/atelia/blob/main/docs/extension-security.ja.md)

## Tool / Extension 実装メモ

Secretary 側の tool category、tool output rendering、extension runtime、operational AX analytics の実装契約は次の文書で扱います。

- [Tool Catalog](tool-catalog.ja.md)
- [Tool Definition Schema](tool-definition-schema.ja.md)
- [Tool Output Schema](tool-output-schema.ja.md)
- [Extensions Runtime](extensions-runtime.ja.md)
- [Operational AX Analytics](operational-ax-analytics.ja.md)
- [MVP Runtime Contract](mvp-runtime-contract.ja.md)
