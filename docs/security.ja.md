# Security

Atelia Secretary は、リポジトリ、外部サービス、自動化エージェントへのアクセスを制御します。そのため、セキュリティ上重要な daemon として設計されなければなりません。

AEP package、Surface Protocol、service、broker boundary、Hook の規範的な安全モデルは次を参照します。

- [`atelia/docs/aep-manifest.ja.md`](https://github.com/atelia-labs/atelia/blob/main/docs/aep-manifest.ja.md)
- [`atelia/docs/aep-services.ja.md`](https://github.com/atelia-labs/atelia/blob/main/docs/aep-services.ja.md)
- [`atelia/docs/surface-protocol.ja.md`](https://github.com/atelia-labs/atelia/blob/main/docs/surface-protocol.ja.md)
- [`atelia/docs/broker-boundary.ja.md`](https://github.com/atelia-labs/atelia/blob/main/docs/broker-boundary.ja.md)

この文書では Secretary daemon 固有の security boundary を扱います。

## 基本ルール

- 外部サービスツールは、必要な CLI または API key が検出された場合にのみ実行します
- tool が利用できない場合は、推測せず structured unavailable status を返します
- 破壊的なリポジトリ操作には明示的な policy support が必要です
- auto-merge は、実際の policy check が接続されるまでブロックされます
- secret を plaintext で log に出してはいけません
- client には、その役割に必要な最小限の状態だけを渡すべきです
- Secretary や人間が誤る前提で、確認、監査、復旧の経路を daemon 側にも持たせます

## Beta の network boundary

beta では、Secretary は原則として同一ホスト内で使う daemon です。
`ATELIA_DAEMON_LISTEN_ADDR` を別の loopback アドレスに設定しない限り、
`127.0.0.1:8080` で listen します。non-loopback への bind は、
明示的な unsafe escape hatch である
`ATELIA_DAEMON_UNSAFE_ALLOW_NON_LOOPBACK_LISTEN=1` が設定されている場合
にのみ許可されます。この override は制御された local test 向けであり、
通常の deployment mode として扱うべきではありません。

Secretary は beta では local bearer token も要求します。startup 時に
`<storage_dir>/daemon-auth.token` を作成または再利用し、すべての request
に `Authorization: Bearer <token>` を求めます。制御された local test
向けの明示的な opt-out として `ATELIA_DAEMON_AUTH_DISABLED=1` があり、
通常の deployment mode として扱うべきではありません。
`ATELIA_DAEMON_AUTH_TOKEN` を設定すると token を固定できますが、
それでも local secret として扱う必要があります。設定された
`ATELIA_DAEMON_AUTH_TOKEN` は強い token でなければならず、64 文字の
hexadecimal 文字列または padding なしで長さ 43 文字以上の base64url token
だけが受け入れられます。既存の token file は再利用前に restrictive
permissions に正規化され、auth-disabled の opt-out は unsafe な
non-loopback listener override と組み合わせると拒否されます。

token file の自動作成と再利用は Unix のみです。non-Unix では
`ATELIA_DAEMON_AUTH_TOKEN` を使って auth を有効に保ち、auth を無効化する
場合も controlled な local testing に限ってください。

token 生成の要件は次のとおりです。

- system CSPRNG を使う
- raw entropy を 32 bytes 以上にする
- hex か padding なしの base64url で encode する
- `<storage_dir>/daemon-auth.token` の permission を owner-only に harden してから再利用する

## Local token の lifecycle

- local bearer token は自動では期限切れになりません。
  `<storage_dir>/daemon-auth.token` は削除または置換されるまで再利用され、
  `ATELIA_DAEMON_AUTH_TOKEN` も process がその値を読む限り有効です。
- 自動 rotation はありません。生成済み token を安全に rotate するには、
  依存 client を停止または静止させ、`<storage_dir>/daemon-auth.token` を削除し、
  Secretary を再起動してから、新しく生成された token を client に配布します。
- pinned token を rotate する場合は、新しい `ATELIA_DAEMON_AUTH_TOKEN` に切り替え、
  すべての client を同じ値へ更新し、Secretary と client を段階的に切り替えます。
  cutover 後は旧 token を再利用しません。
- token が露出した、または漏えいした疑いがある場合は revoke 済みとみなし、
  daemon 側の token を差し替え、保存済み file か pinned env value を無効化し、
  daemon を再起動して、依存する automation もすべて更新し、漏えい経路を確認します。
- automation で pinned token を使うのは、secret manager などの access-controlled な
  runtime secret store に安全に保持できる場合に限ります。token を version control、
  shared config、CI の variable / secret に commit してはいけません。backup や shared
  machine image には token を plaintext のまま含めてはいけません。読み取り側の
  automation は最小権限で実行します。
- Unix では token file は owning user が読めるように作成され、再利用前に
  `0600` に正規化されます。別の場所から copy, restore, mount した file を使う場合は、
  Secretary を実行する user が ownership を持ち、実際に read できることを確認してから
  再利用します。環境により `0400` しか使えない場合でも、少なくとも owner-only read に
  してください。
- non-Unix では Secretary は token file を自動作成・再利用しません。
  auth を有効に保つには `ATELIA_DAEMON_AUTH_TOKEN` を設定し、`ATELIA_DAEMON_AUTH_DISABLED=1`
  は controlled な local testing にのみ使ってください。

## Replay protection

beta の既知の limitation として、Secretary は local auth に built-in の
replay protection や token expiry をまだ持ちません。認証済み request は、
token を rotation するまで、または local boundary が失われるまで replay
できます。loopback 上にいる attacker でも、token が rotation されるまで
再利用できます。

Secretary が現在提供している local boundary の mitigation は次の 2 つです。

- daemon は default で loopback のみに bind し、unsafe な non-loopback override を
  有効にしない限り traffic を local host に限定します。
- local auth opt-out を有効にしない限り、各 request で bearer token を要求します。

推奨 mitigation:

- daemon は loopback-only のまま使い、local auth は有効のままにする
- host ごと、または daemon instance ごとに一意の token を使う
- token は 30 days 程度を目安に定期 rotation し、compromise の疑いがある
  場合は直ちに rotate する
- token の使用状況を monitoring し、audit を残す
- shared listener, reverse proxy, それ以外の replay しやすい boundary の背後に daemon を置かない

将来の roadmap:

- short-lived token
- refresh flow
- request signing with HMAC + nonce + timestamp
- server-side nonce tracking
- token rotation の first-class support

## threat model の種

初期の threat model work では次のものを扱うべきです。

- 悪意のある、または混乱したエージェント
- compromise された外部ツール credentials
- repository content や issue text を通じた prompt injection
- unsafe auto-fix loops
- 偽造された AX Feedback
- replay された client requests
- local daemon exposure
- Docker socket と host filesystem boundaries
- daemon 経由の GitHub / repository abuse

## 関連する enforcement contract

- [Tool Catalog](tool-catalog.ja.md): capability area と default risk tier を定義します。
- [Tool Definition Schema](tool-definition-schema.ja.md): tool identity、input schema、effects、runtime behavior、customization surface を定義します。
- [Tool Output Schema](tool-output-schema.ja.md): agent-facing output、audit separation、redaction、TOON / JSON format selection を定義します。
- [AEP Package Runtime](extensions-runtime.ja.md): manifest enforcement、package sandbox、provenance、rollback、blocklist behavior を定義します。
- [Operational AX Analytics](operational-ax-analytics.ja.md): AX 改善のための privacy-preserving analytics を定義します。

## 報告

正式な security address が存在するまでは、security report は `atelia-labs` organization の maintainer が private に扱います。
