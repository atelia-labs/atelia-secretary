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
- previous-minor protocol compatibility checks
- manifest schema fixture validation
- extension runtime compatibility checks
- tool-output TOON / JSON golden fixtures
- tool-output customizer compatibility fixtures
- extension compatibility matrix updates
- tool-output default、permission model、extension contract、Hook behavior が変わる場合の AX impact review
- permission name または risk tier が変わる場合の permission migration tests
- extension enforcement が変わる場合の blocklist / rollback behavior tests
- Docker build
- 該当する場合、security-sensitive change review

Atelia extension、hook、permission、tool-output contract の enforcement を変更する Secretary release は、実装対象の Atelia specification version または commit を明記します。

## 変更履歴

changelog では次のものを区別します。

- user-facing changes
- daemon behavior changes
- protocol changes
- policy / orchestration changes
- extension / hook compatibility changes
- tool-output schema and default-format changes
- security fixes
- breaking changes
