# Contributing

Atelia Secretary は、最初から OSS プロジェクトとして設計されています。

このプロジェクトでは、次の領域での貢献を受け付けています。

- Rust daemon implementation
- protocol design
- client compatibility boundary
- documentation
- security review
- AX feedback design
- issue intake と release hygiene

## プロジェクトの姿勢

Atelia は、AI エージェントをエンドユーザーとして扱います。貢献もこの原則を保つ必要があります。変更によって人間にとっては簡単になる一方で、その中で働くエージェントにとってより混乱し、不透明で、危険になる場合は、pull request でその点を明示してください。

## pull request の書き方

pull request には次のものを含めてください。

- 短い summary
- 実施した verification
- risk notes
- 関連する場合、AX impact

高リスクな自動化変更は、merge される前に明示的な policy review を必要とします。

## 開発

```sh
cargo check --workspace
```

toolchain が完全に pin されたら、`rustfmt` と linting は mandatory release gates になります。
