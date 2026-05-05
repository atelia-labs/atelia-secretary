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

beta release では、ローカルと CI で次の gate を通します。

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `docker build --file Dockerfile .`
- `docker run` の smoke。`ATELIA_DAEMON_LISTEN_ADDR=0.0.0.0:8080` と `ATELIA_DAEMON_UNSAFE_ALLOW_NON_LOOPBACK_LISTEN=1` を付けて `ateliad` を起動し、`/v1/health` を確認します

CI では `.github/workflows/ci.yml` の `Beta Release Gates` job が、smoke
を含む同じ gate 群を実行します。将来、shared workflow が Rust の
checks を吸収しても、この job は packaging check として残すか、
該当 step を manual として docs と CI の両方で明記してください。

明示的な non-loopback の `ATELIA_DAEMON_LISTEN_ADDR` は、
`ATELIA_DAEMON_UNSAFE_ALLOW_NON_LOOPBACK_LISTEN` が `1` や `true` の
ような truthy 値でない限り、起動時に失敗させます。loopback と
default の local bind は unsafe opt-in なしで許可します。

beta gate に加えて、release には次のものが必要です。

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
