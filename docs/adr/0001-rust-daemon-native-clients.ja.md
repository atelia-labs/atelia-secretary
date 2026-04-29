# ADR 0001: Rust Daemon とネイティブクライアント

## ステータス

Proposed

## 背景

Atelia Secretary には、予測可能な performance、memory safety、clean deployment、そして backend orchestration と user-facing clients の境界が必要です。

## 決定

Secretary は、Docker で配布する Rust backend daemon にします。

first-party client は、macOS と iOS 向けのネイティブ Swift アプリケーションにします。

TUI は初期プロダクトサーフェスとしては構築しません。

client と daemon の間には typed protocol を使います。将来 Linux / Windows client を追加しても、backend の conceptual model は変えずに済む形にします。

## 結果

- backend implementation は長時間動作する reliability に集中できます。
- client は platform-native UX を使えます。
- shared logic は shared web frontend ではなく、protocol contracts と client SDK に置かれます。
- 初期実装の負荷は単一の TUI より高くなりますが、プロダクト方向は明確になります。
