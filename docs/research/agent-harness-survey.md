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

- Human-like Secretary: treat Secretary as a judging subject with continuity,
  memory, professional posture, and room to grow.
- Workplace before automation: evaluate whether each tool helps Secretary think,
  notice, remember, ask, delegate, recover, and form opinions about the
  workplace.
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
  desktop apps. Atelia should treat this as future exploration after the initial
  requirements.
- Claude Code exposes layers such as project memory files, Skills, MCP,
  subagents, hooks, plugins, and agent teams. Atelia can connect this direction
  to Custom AX extensions and explicit Secretary / specialist agent roles.
- GitHub Copilot coding agent accepts tasks from issues, GitHub surfaces, CLI,
  and MCP, then produces pull requests and review requests. Atelia can use GitHub
  issues as the public outlet for Secretary-level AX Feedback and implementation
  work.
- Cursor has rules, memories, and background agents. Atelia should treat memory
  as workplace growth.
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

## Tool Surface Comparison

This section focuses on what each harness exposes to agents and users.

| Harness | Agent tools | User-facing control surface | AX lesson for Atelia |
| --- | --- | --- | --- |
| Claude Code | file read/write, grep/glob, shell, LSP, web, MCP, subagents, hooks, skills, tasks, monitors | permission rules, hook lifecycle, subagent/tool scoping, checkpoints, project instructions | Tool use is an event stream. Hooks and permissions should observe tool calls before and after execution. |
| Codex CLI | local file reading/editing, command execution, multimodal input, sandboxed full-auto mode | approval modes, local terminal workflow, git-aware warnings, sandbox defaults | Modes should be explicit: read-only, edit, command execution, and sandboxed autonomy are different product states. |
| Gemini CLI | file operations, shell commands, Google Search grounding, web fetching, MCP | terminal-first UX, open source CLI, ACP mode for IDE/client integration | A small agent can become client-agnostic if it exposes a stable agent/client protocol. |
| OpenHands | shell, filesystem, browser, plugins, runtime/sandbox actions | Docker/process/remote sandboxes, custom sandbox images, integrations, secrets settings | Runtime isolation and environment reproducibility are first-class UX. |
| SWE-agent | bash, code inspection tools, editors, tool bundles | issue-oriented automation, configurable bundles, benchmark-oriented workflows | Tool bundles make agent-computer interaction explicit and measurable. |
| Aider | repo map, git integration, edits, tests/lint commands, web/image context | chat-in-terminal, automatic commits, focused repo context | Repo maps are token-efficiency infrastructure. Agents benefit from structural context. |
| Cline / Roo Code | file ops, terminal, browser, MCP, custom modes, subagents | explicit approvals, checkpoints, Plan/Act or role modes, editor integration | Recovery is part of trust. Checkpoints reduce the cost of letting agents act. |
| Cursor Background Agents | remote branch edits, terminal commands, setup scripts, environment snapshots | async agents, follow-ups, status, takeover, GitHub handoff | Background work needs status, branch ownership, setup reuse, and human takeover. |

## Tool Output Is An AX Surface

Tool execution results are an observation surface for agents and a direct part
of AX. Atelia designs AI-native structured tool output.

Atelia treats TOON as the first tool output format. TOON is Token-Oriented
Object Notation, a compact representation for passing structured data to LLMs.
It can be especially useful for object arrays and tabular data where JSON repeats
the same keys many times.

The format is configurable. Secretary should be able to switch between TOON and
JSON from settings, request a temporary alternate format, and configure per-tool
default formats. This is AX customization that lets Secretary shape its own
workplace.

Design work covers every field, key name, order, omission, redundancy, and
default value. Which information is needed for the next judgment? Which belongs
in the audit log instead of the immediate result? Which fields duplicate each
other? Which order is easiest to read and cheapest in tokens? Tool output is a
working surface for Secretary and agents.

Production operation logs are inputs for AX improvement. Just as product teams
study user behavior to improve UX, Atelia studies Secretary and agent tool use,
format switching, repeated calls, missed information, and unused fields to
improve tool output design.

## Language And Token Efficiency

Direct user dialogue uses the user's language. Agent-to-agent communication and
language-independent work can benefit from English for token efficiency and task
performance.

Atelia should make this difference available without turning it into a rigid
rule. Secretary respects the user's language in user-facing responses, while
agent work and tool output key names can prefer English when useful. The choice
should remain adjustable by task, agent preference, and tool behavior.

## Connection To Design Docs

This research note is an entry point from existing harness observations into
Atelia Secretary design. Implementation contracts live in:

- [Tool Catalog](../tool-catalog.md)
- [Tool Definition Schema](../tool-definition-schema.md)
- [Tool Output Schema](../tool-output-schema.md)
- [Extensions Runtime](../extensions-runtime.md)
- [Operational AX Analytics](../operational-ax-analytics.md)

When research is promoted into specification, review it against the Atelia and
Secretary philosophy documents. In particular, check Secretary's agency,
workplace growth, AX Feedback, token-efficient tool output, and whether
extensions or hooks preserve rather than replace judgment.

## Sources

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
