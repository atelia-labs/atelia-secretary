# AI Agent Harness Research

This note organizes directions from public AI coding agent and harness
landscape research as input to Atelia Secretary's daemon and orchestration
design.

Normative Client UX, Hooks, and Custom AX extension specifications live in the
Atelia project repository.

- [Client UX](https://github.com/atelia-labs/atelia/blob/main/docs/client-ux.md)
- [Hooks](https://github.com/atelia-labs/atelia/blob/main/docs/hooks.md)
- [Custom AX Extensions](https://github.com/atelia-labs/atelia/blob/main/docs/extensions.md)

## Directions Worth Adopting For Secretary

- Asynchronous tasks: assign work, let it run elsewhere, and review progress and
  results later.
- Parallel execution: run multiple tasks while Secretary keeps the project state
  coherent.
- Reviewable outputs: collect diffs, PRs, tests, and decision logs.
- Isolated runtime: separate tasks through sandboxes, VMs, or containers.
- Durable memory: keep project rules, user preferences, and prior decisions in
  the workplace.
- Role separation: split Secretary, implementation agents, review agents, and
  dedicated AX improvement agents.
- Permission modes: separate reading, editing, command execution, and external
  access.
- Codebase map and retrieval: help agents navigate large repositories.
- Setup reuse: make dependencies and build environments reusable between tasks.
- Cost and token awareness: reduce context and requests without harming AX.
- Lessons loop: record human corrections as workplace learning so repeated
  mistakes decline.

## Notes From Surveyed Tools

- OpenAI Codex / Codex App brings approvals, sandboxing, diffs, terminal,
  multiple files, multiple terminals, SSH devboxes, an in-app browser,
  conversation, and in-progress work state into one control surface.
- Codex is moving toward computer use, where agents can work across browsers and
  desktop apps. Atelia should treat this as future exploration, not an initial
  requirement.
- Claude Code exposes layers such as project memory files, Skills, MCP,
  subagents, hooks, plugins, and agent teams. Atelia can connect this direction
  to Custom AX extensions and explicit Secretary / specialist agent roles.
- GitHub Copilot coding agent accepts tasks from issues, GitHub surfaces, CLI,
  and MCP, then produces pull requests and review requests. Atelia can use GitHub
  issues as the public outlet for Secretary-level AX Feedback and implementation
  work.
- Cursor has rules, memories, and background agents. Atelia should treat memory
  as workplace growth, not merely prompt stuffing.
- Jules emphasizes async execution, cloud VMs, planning, diffs, PRs, issue
  labels, parallel tasks, and setup reuse. This fits Secretary's "work continues
  while humans are away" experience.
- OpenHands exposes CLI, local GUI, REST API, SDK, Docker / Kubernetes, Slack /
  Jira / Linear, RBAC, and auditability. Atelia Secretary should be API-first
  while supporting an AX-first workplace.
- Aider has strong codebase maps, git integration, automatic commits, IDE
  integration, and image / web page context. Atelia Secretary should take
  codebase maps and git-shaped outputs seriously.
- Cline and Roo Code use Plan / Act, custom modes, explicit approval, browser /
  terminal access, and MCP. Atelia Secretary should keep Secretary's judgment
  intact while making modes and permissions visible.
- SWE-agent strongly frames the agent-computer interface. Atelia extends that
  idea into product design for the whole workplace.
- Reddit Claude Code workflow discussions repeatedly mention project memory
  files, context separation through subagents, Research -> Plan -> Execute ->
  Review, and self-improvement loops that turn user corrections into lessons.

## Sources

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
