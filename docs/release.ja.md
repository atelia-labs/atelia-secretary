# リリースポリシー

Atelia Secretary のリリースは、安定して予測可能であることを重視します。

## アーティファクト

初期のリリースアーティファクト:

- `ateliad` の Docker image
- crate boundary が十分に安定した時点での Rust crates
- generated protocol artifacts

## バージョニング

公開 API と release artifact には semantic versioning を使います。

protocol contracts は versioned にします。client と daemon の互換性は、接続時に明示的に確認します。

## リリース条件

リリースには次のものが必要です。

- formatting
- tests
- linting
- protocol compatibility checks
- Docker build
- 該当する場合、security-sensitive change review

## 変更履歴

changelog では次のものを区別します。

- user-facing changes
- daemon behavior changes
- protocol changes
- policy / orchestration changes
- security fixes
- breaking changes
