# Security

Atelia Secretary は、リポジトリ、外部サービス、自動化エージェントへのアクセスを制御します。そのため、セキュリティ上重要な daemon として設計されなければなりません。

Extension / Hook の規範的な安全モデルは [`atelia/docs/extension-security.ja.md`](https://github.com/atelia-labs/atelia/blob/main/docs/extension-security.ja.md) を参照します。この文書では Secretary daemon 固有の security boundary を扱います。

## 基本ルール

- 外部サービスツールは、必要な CLI または API key が検出された場合にのみ実行します
- tool が利用できない場合は、推測せず structured unavailable status を返します
- 破壊的なリポジトリ操作には明示的な policy support が必要です
- auto-merge は、実際の policy check が接続されるまでブロックされます
- secret を plaintext で log に出してはいけません
- client には、その役割に必要な最小限の状態だけを渡すべきです
- Secretary や人間が誤る前提で、確認、監査、復旧の経路を daemon 側にも持たせます

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

## 報告

正式な security address が存在するまでは、security report は `atelia-labs` organization の maintainer が private に扱います。
