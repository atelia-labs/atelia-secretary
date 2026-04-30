# AI agent harness research

このノートは、公開されている AI coding agent / harness の方向性を、Atelia Secretary の daemon / orchestration 設計への入力として整理したものです。

Client UX、Hooks、Custom AX extension の規範的仕様は Atelia 本体リポジトリに置きます。

- [Client UX](https://github.com/atelia-labs/atelia/blob/main/docs/client-ux.ja.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.ja.md)
- [Custom AX Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.ja.md)

## Secretary に取り込みたい方向性

- 人間らしい Secretary: 継続性、記憶、専門職としての姿勢、成長の余地を持つ判断主体として Secretary を扱う
- automation より仕事場: それぞれの tool が、Secretary が考え、気づき、覚え、質問し、委譲し、復旧し、仕事場への意見を持つことを助けるか評価する
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
- Cursor は rules、memories、background agents を持つ。Atelia では記憶を仕事場の成長として扱う
- Jules は、非同期実行、Cloud VM、plan、diff、PR、issue label、並列タスク、setup reuse を重視している。Atelia Secretary の「寝ている間も進む」体験と相性がよい
- OpenHands は、CLI、local GUI、REST API、SDK、Docker / Kubernetes、Slack / Jira / Linear、RBAC、auditability を持つ。Atelia Secretary は API-first にしつつ、AX-first の仕事場を支える daemon であるべき
- Aider は codebase map、git integration、自動 commit、IDE 連携、画像や web page の取り込みが強い。Atelia Secretary でも codebase map と git 成果物の扱いは重要になる
- Cline / Roo Code は、Plan / Act、custom modes、explicit approval、browser / terminal / MCP などを持つ。Atelia Secretary では Secretary の判断主体性を保ったまま、作業モードと権限を明示する
- SWE-agent は、agent-computer interface という視点を強く持つ。Atelia では、この視点を仕事場全体の設計へ広げる
- Reddit の Claude Code workflow 事例では、CLAUDE.md 的なプロジェクト記憶、subagent による context 分離、Research → Plan → Execute → Review、ユーザー修正を lessons として蓄積する self-improvement loop が繰り返し話題になっている

## Tool Surface Comparison

ここでは、各 harness が agent と user に expose している surface を見ます。

| Harness | Agent tools | User-facing control surface | Atelia への AX lesson |
| --- | --- | --- | --- |
| Claude Code | file read/write、grep/glob、shell、LSP、web、MCP、subagents、hooks、skills、tasks、monitors | permission rules、hook lifecycle、subagent / tool scoping、checkpoints、project instructions | tool use は event stream。実行前後の hooks と permissions が観測できるべき |
| Codex CLI | local file read/edit、command execution、multimodal input、sandboxed full-auto mode | approval modes、terminal workflow、git-aware warnings、sandbox defaults | read-only、edit、command execution、sandboxed autonomy は別々の product state |
| Gemini CLI | file operations、shell commands、Google Search grounding、web fetching、MCP | terminal-first UX、open source CLI、ACP mode による IDE / client integration | 小さな agent でも安定した agent / client protocol を持てば client-agnostic になれる |
| OpenHands | shell、filesystem、browser、plugins、runtime / sandbox actions | Docker / process / remote sandboxes、custom sandbox images、integrations、secrets settings | runtime isolation と environment reproducibility は UX の一部 |
| SWE-agent | bash、code inspection tools、editors、tool bundles | issue-oriented automation、configurable bundles、benchmark-oriented workflows | tool bundle は agent-computer interaction を明示的、計測可能にする |
| Aider | repo map、git integration、edits、tests / lint commands、web / image context | terminal chat、自動 commit、focused repo context | repo map は token-efficiency infrastructure。agent には構造 context が効く |
| Cline / Roo Code | file ops、terminal、browser、MCP、custom modes、subagents | explicit approvals、checkpoints、Plan / Act や role modes、editor integration | recovery は trust の一部。checkpoint は agent に行動させる cost を下げる |
| Cursor Background Agents | remote branch edits、terminal commands、setup scripts、environment snapshots | async agents、follow-ups、status、takeover、GitHub handoff | background work には status、branch ownership、setup reuse、human takeover が必要 |

## Tool Output は AX Surface である

ツールの実行結果は、エージェントにとっての観測面であり、AX の直接の構成要素です。Atelia は AI-native な構造化ツール出力を設計します。

Atelia の tool output は、TOON を第一の形式として扱います。TOON は Token-Oriented Object Notation であり、LLM に構造化データを渡すための compact な表現です。特に同じ key を繰り返す object array や tabular なデータでは、JSON より少ない token で同じ情報を渡せる可能性があります。

Secretary は設定から TOON と JSON を切り替えられるべきです。一時的に別の形式で受け取ることも、tool ごとの default format を設定することもできるようにします。これは Secretary が自分の仕事場を育てるための AX customization です。

設計では、形式名だけでなく、各 field、key name、順序、省略、冗長性、default 値を問い直します。どの情報が次の判断に必要か。どの情報は audit log に回すべきか。どの情報は重複しているか。どの順序なら読み取りやすく、token 効率が良いか。tool output は、Secretary と agent が仕事を進めるための道具として扱います。

本番運用ログは、AX 改善の入力として扱います。通常の product が user behavior を見て UX を改善するように、Atelia は Secretary や agent の tool 利用、format 切り替え、再実行、読み落とし、不要 field を観測し、tool output の設計を更新します。

## 言語と token efficiency

ユーザーとの直接対話では、ユーザーの言語を使います。一方で、agent 間のやり取りや言語非依存の作業では、英語の方が token 効率や task performance に有利な場面があります。

Secretary は、ユーザーに向けた応答ではユーザーの言語を尊重し、agent 間の作業や tool output の key name では英語を優先する選択肢を持ちます。仕事の内容、agent の好み、tool の性質に応じて調整できるべきです。

## 設計文書への接続

この research note は、既存 harness の観察と Atelia Secretary の設計判断をつなぐための入口です。実装時に参照する contract は、次の文書へ分けます。

- [Tool Catalog](../tool-catalog.ja.md)
- [Tool Definition Schema](../tool-definition-schema.ja.md)
- [Tool Output Schema](../tool-output-schema.ja.md)
- [Extensions Runtime](../extensions-runtime.ja.md)
- [Operational AX Analytics](../operational-ax-analytics.ja.md)

研究から仕様へ進めるときは、Atelia と Secretary の思想文書に照らして確認します。特に、Secretary の判断主体性、仕事場の成長、AX Feedback、tool output の token efficiency、extension / hook が agency を奪わないことを確認します。

## 出典

- OpenAI Codex CLI: https://help.openai.com/en/articles/11096431-openai-codex-ci-getting-started
- OpenAI Codex: https://openai.com/index/introducing-codex/
- OpenAI Codex App: https://openai.com/index/introducing-the-codex-app/
- OpenAI Codex for almost everything: https://openai.com/index/codex-for-almost-everything/
- OpenAI Academy / Plugins and skills: https://openai.com/academy/codex-plugins-and-skills/
- Codex with ChatGPT plan: https://help.openai.com/en/articles/11369540-codex-in-chatgpt
- OpenAI Codex product page: https://openai.com/codex/
- Claude Code extensions: https://code.claude.com/docs/en/features-overview
- Claude Code tools: https://code.claude.com/docs/en/tools-reference
- Claude Code hooks: https://code.claude.com/docs/en/hooks
- Gemini CLI: https://google-gemini.github.io/gemini-cli/
- Gemini CLI ACP mode: https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/acp-mode.md
- GitHub Copilot coding agent: https://docs.github.com/en/copilot/concepts/about-assigning-tasks-to-copilot
- GitHub Copilot task assignment: https://docs.github.com/en/copilot/how-tos/use-copilot-agents/coding-agent/assign-copilot-to-an-issue
- Cursor rules: https://docs.cursor.com/context/rules
- Cursor memories: https://docs.cursor.com/en/context/memories
- Cursor background agents: https://docs.cursor.com/en/background-agents
- Jules: https://jules.google/
- Google Jules announcement: https://blog.google/technology/google-labs/jules
- OpenHands: https://openhands.dev/
- OpenHands GitHub: https://github.com/OpenHands/OpenHands
- OpenHands sandbox overview: https://docs.openhands.dev/openhands/usage/runtimes/overview
- OpenHands runtime architecture: https://docs.openhands.dev/openhands/usage/architecture/runtime
- Aider GitHub: https://github.com/Aider-AI/aider
- Aider repository map: https://aider.chat/docs/repomap.html
- Cline overview: https://docs.cline.bot/introduction/overview
- Cline tools: https://docs.cline.bot/tools-reference/all-cline-tools
- Cline checkpoints: https://docs.cline.bot/core-workflows/checkpoints
- Roo Code custom modes: https://deepwiki.com/qpd-v/Roo-Code/5.2-custom-modes
- Continue roles: https://docs.continue.dev/setup/overview
- Continue autocomplete: https://docs.continue.dev/ide-extensions/autocomplete/how-it-works
- SWE-agent GitHub: https://github.com/SWE-agent/SWE-agent
- SWE-agent tools: https://swe-agent.com/latest/config/tools/
- Reddit / Claude Code visual notes: https://www.reddit.com/r/aiagents/comments/1sfjbwj/claude_code_visual_hooks_subagents_mcp_claudemd/
- Reddit / Claude Code workflow: https://www.reddit.com/r/Anthropic/comments/1rqvjgj/my_workflow_for_claude_code/
