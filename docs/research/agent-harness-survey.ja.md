# AI agent harness research

このノートは、公開されている AI coding agent / harness の方向性を、Atelia Secretary の daemon / orchestration 設計への入力として整理したものです。

Client UX、Hooks、Custom AX extension の規範的仕様は Atelia 本体リポジトリに置きます。

- [Client UX](https://github.com/atelia-labs/atelia/blob/main/docs/client-ux.ja.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.ja.md)
- [Custom AX Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.ja.md)

## Secretary に取り込みたい方向性

- 非同期タスク実行: 課題を渡し、別環境で作業し、進捗と成果をあとから確認できる
- 並列実行: 複数の作業を同時に走らせ、Secretary が状態を把握する
- レビュー可能な成果物: diff、PR、テスト結果、判断ログをまとめて確認できる
- isolated runtime: 各作業を sandbox / VM / container で分離する
- 常駐の記憶: プロジェクト規則、ユーザーの好み、過去の判断を仕事場に残す
- 役割分担: Secretary、実装エージェント、レビューエージェント、AX 改善専任エージェントを分ける
- 権限モード: 読み取り、編集、コマンド実行、外部アクセスを段階的に許可する
- codebase map / retrieval: 大きなリポジトリでも迷わないための検索、要約、構造把握
- setup reuse: 依存関係の準備やビルド環境を再利用し、次の作業開始を速くする
- cost / token awareness: 体験を落とさず、不要なコンテキストやリクエストを減らす
- lessons loop: 人間から修正された内容を仕事場の学習メモへ残し、同じ失敗を減らす

## 調査元からのメモ

- OpenAI Codex / Codex App は、approval、sandbox、差分確認、複数ファイル、複数ターミナル、SSH devbox、アプリ内ブラウザ、会話、作業中の状態をひとつのインターフェースにまとめている
- Codex は computer use により、ブラウザやデスクトップアプリを横断して作業できる方向へ進んでいる。Atelia では初期要件に含めず、将来探索として扱う
- Claude Code は、`CLAUDE.md`、Skills、MCP、subagents、hooks、plugins、agent teams のような拡張層を持つ。Atelia では Custom AX extension と Secretary / 専任エージェントの分業に接続できる
- GitHub Copilot coding agent は、issue、GitHub UI、CLI、MCP などからタスクを渡し、PR と review request に落とす。Atelia では GitHub issue を Secretary 本体への AX Feedback と実装タスクの出口にできる
- Cursor は rules、memories、background agents を持つ。Atelia では記憶を単なるプロンプトではなく、仕事場の成長として扱う
- Jules は、非同期実行、Cloud VM、plan、diff、PR、issue label、並列タスク、setup reuse を重視している。Atelia Secretary の「寝ている間も進む」体験と相性がよい
- OpenHands は、CLI、local GUI、REST API、SDK、Docker / Kubernetes、Slack / Jira / Linear、RBAC、auditability を持つ。Atelia Secretary は API-first にしつつ、AX-first の仕事場を支える daemon であるべき
- Aider は codebase map、git integration、自動 commit、IDE 連携、画像や web page の取り込みが強い。Atelia Secretary でも codebase map と git 成果物の扱いは重要になる
- Cline / Roo Code は、Plan / Act、custom modes、explicit approval、browser / terminal / MCP などを持つ。Atelia Secretary では Secretary の判断主体性を保ったまま、作業モードと権限を明示する
- SWE-agent は、agent-computer interface という視点を強く持つ。Atelia では、この視点を仕事場全体の設計へ広げる
- Reddit の Claude Code workflow 事例では、CLAUDE.md 的なプロジェクト記憶、subagent による context 分離、Research → Plan → Execute → Review、ユーザー修正を lessons として蓄積する self-improvement loop が繰り返し話題になっている

## 出典

- OpenAI Codex CLI: https://help.openai.com/en/articles/11096431-openai-codex-ci-getting-started
- OpenAI Codex: https://openai.com/index/introducing-codex/
- OpenAI Codex App: https://openai.com/index/introducing-the-codex-app/
- OpenAI Codex for almost everything: https://openai.com/index/codex-for-almost-everything/
- OpenAI Academy / Plugins and skills: https://openai.com/academy/codex-plugins-and-skills/
- Codex with ChatGPT plan: https://help.openai.com/en/articles/11369540-codex-in-chatgpt
- OpenAI Codex product page: https://openai.com/codex/
- Claude Code extensions: https://code.claude.com/docs/en/features-overview
- GitHub Copilot coding agent: https://docs.github.com/en/copilot/concepts/about-assigning-tasks-to-copilot
- GitHub Copilot task assignment: https://docs.github.com/en/copilot/how-tos/use-copilot-agents/coding-agent/assign-copilot-to-an-issue
- Cursor rules: https://docs.cursor.com/context/rules
- Cursor memories: https://docs.cursor.com/en/context/memories
- Cursor background agents: https://docs.cursor.com/en/background-agents
- Jules: https://jules.google/
- Google Jules announcement: https://blog.google/technology/google-labs/jules
- OpenHands: https://openhands.dev/
- OpenHands GitHub: https://github.com/OpenHands/OpenHands
- Aider GitHub: https://github.com/Aider-AI/aider
- Cline overview: https://docs.cline.bot/introduction/overview
- Roo Code custom modes: https://deepwiki.com/qpd-v/Roo-Code/5.2-custom-modes
- Continue roles: https://docs.continue.dev/setup/overview
- Continue autocomplete: https://docs.continue.dev/ide-extensions/autocomplete/how-it-works
- SWE-agent GitHub: https://github.com/SWE-agent/SWE-agent
- Reddit / Claude Code visual notes: https://www.reddit.com/r/aiagents/comments/1sfjbwj/claude_code_visual_hooks_subagents_mcp_claudemd/
- Reddit / Claude Code workflow: https://www.reddit.com/r/Anthropic/comments/1rqvjgj/my_workflow_for_claude_code/
