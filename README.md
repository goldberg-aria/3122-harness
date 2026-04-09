# Coding Agent Harness

Terminal-first coding agent harness scaffold.

## Goal

Build a local coding agent runtime with:

- a terminal REPL and one-shot command surface
- a permission-gated tool loop
- skill discovery
- MCP server discovery
- session storage and resumability
- BYOK model providers
- local Ollama support
- optional authenticated adapters for external CLIs such as Claude Code and Codex

## Product stance

This project is not trying to clone another agent byte-for-byte.
It is building a clean-room harness with similar strengths:

- model independence
- strong permissions
- explicit tool execution
- session durability
- local memory continuity
- provider portability
- terminal-native workflows

## Current status

This repository currently contains:

- an architecture document in `docs/ARCHITECTURE.md`
- a Rust workspace scaffold
- a project-level `harness.toml`
- a small `harness` CLI with `repl`, `doctor`, `config`, `providers`, `blueprint`, `skills`, `mcp`, `session`, and `tool`
- a provider and runtime blueprint in `crates/runtime`
- provider clients for Anthropic BYOK, OpenAI-compatible BYOK, and Ollama
- external adapter clients for `claude` and `codex`
- JSONL session files under `.harness/sessions/`
- built-in tool surfaces for `read`, `write`, `edit`, `grep`, `glob`, and `exec`
- skill discovery across project and user skill folders
- skill resolution and prompt-packet generation through `skills show/run` and `/skill`
- MCP discovery plus stdio tool listing/calls through `mcp tools/call`
- a tested agent loop that can request built-in tools, skills, and MCP calls through a provider-agnostic tool-call block
- approval gating for model-requested tools with risk-aware `prompt` and `auto` policies
- saved BYOK provider profiles with key detection and preset-based registration
- verification policy with `off`, `annotate`, and `require`
- prompt context with recent history, local memory recall, relevant conversation recall, and a context budget cap
- routing-friendly skill summaries with a short summary budget
- read-only `parallel_read` batching for one-turn discovery
- provider-native tool calling for Anthropic, OpenAI-compatible, and Ollama with text-tool fallback
- task-aware verification that requires checks after the last code mutation while exempting docs-only edits
- runtime unit tests covering config, permissions, providers, skills, MCP, and the harness loop

Planning is fixed in [docs/ROADMAP.md](/Users/paul_k/Documents/p-23/3122/docs/ROADMAP.md).

## Commands

```bash
cargo run -p cli -- doctor
cargo run -p cli -- config
cargo run -p cli -- model show
cargo run -p cli -- model set-primary openai/gpt-4.1-mini
cargo run -p cli -- memory
cargo run -p cli -- memory save
cargo run -p cli -- memory search provider
cargo run -p cli -- resume
cargo run -p cli -- handoff
cargo run -p cli -- why-context
cargo run -p cli -- tool parallel-read '[{"tool":"read","path":"README.md"},{"tool":"glob","pattern":"src/*.rs"}]'
cargo run -p cli -- prompt "say hello"
cargo run -p cli -- providers
cargo run -p cli -- providers presets
cargo run -p cli -- providers detect-key <api-key>
cargo run -p cli -- providers add router --api-key <api-key>
cargo run -p cli -- providers saved
cargo run -p cli -- blueprint
cargo run -p cli -- skills
cargo run -p cli -- skills show project-bootstrap
cargo run -p cli -- skills run project-bootstrap "start the first runtime slice"
cargo run -p cli -- mcp
cargo run -p cli -- mcp tools mock-echo
cargo run -p cli -- mcp call mock-echo echo '{"text":"hello"}'
cargo run -p cli -- session latest
cargo run -p cli -- tool read README.md
cargo run -p cli -- repl
```

## Tool-call protocol

The current loop uses a provider-agnostic text contract.

If the model needs a tool, it must answer with only:

```xml
<tool_call>
{"tool":"read","arguments":{"path":"README.md"}}
</tool_call>
```

Supported loop actions:

- built-in tools: `read`, `write`, `edit`, `grep`, `glob`, `exec`, `parallel_read`
- `skill`
- `mcp_list_tools`
- `mcp_call`

Provider-native tool calling:

- Anthropic, OpenAI-compatible providers, and Ollama now expose the built-in tool set as native tools when supported
- if native tool calling is unsupported or rejected, the harness falls back to the text `<tool_call>` path
- external CLI adapters still use the text tool path for now

Approval behavior:

- default policy is `[approvals].policy = "prompt"`
- `prompt` mode auto-approves low-risk tools, prompts for medium/high-risk tools, and blocks critical tools
- `auto` mode auto-approves low/medium/high-risk tools and still blocks critical tools
- REPL supports `/approval`, `/approval prompt`, `/approval auto`
- one-shot `prompt` only fails when a request would have prompted or been denied

Verification behavior:

- default verification policy comes from `[verification].policy`
- verification must happen after the last workspace mutation to count
- docs-only edits do not force a verification step
- `require` rejects completion after relevant workspace mutations unless a verification step was recorded or the model explicitly says `Not verified`
- `annotate` prefixes the final answer with `Not verified` plus task-aware guidance
- `off` disables verification enforcement

REPL shape:

- the base flow is closer to Codex/Claude-style slash commands
- primary session commands are `/status`, `/model`, `/login`, `/memory`, `/resume`, `/handoff`, `/why-context`, `/approval`, `/doctor`
- `/model <spec>` switches the active model for the current session
- `/parallel-read <json-array>` batches read-only discovery work in one turn

Local-Lite memory:

- `memory save` promotes the latest session into local memory
- `memory` lists recent memory records
- `memory search <query>` searches saved memory locally
- `resume` and `handoff` render that memory back into operator-friendly text
- `why-context` prints the exact runtime context injected before a model turn
- REPL exit autosaves new local memory records for the current session
- prompt context now includes recent working history, Local-Lite memory recall, and relevant conversation recall from older sessions
- prompt context is budgeted so long sessions degrade conversation recall first, then memory, recent history, and finally instructions

Skill summaries:

- discovered skills now expose a routing-friendly summary capped to about 250 characters
- frontmatter `description:` is preferred when present

Saved provider profiles:

- `providers add` stores BYOK profiles in `.harness/providers.json`
- auto-detection currently recognizes some key formats such as OpenRouter and OpenAI
- manual registration supports presets such as `deepseek`, `dashscope-cn`, `dashscope-intl`, and `siliconflow`
- saved profiles can be used as `profile/<alias>/<model>`

## Immediate next steps

1. Add richer model-switch handoff and session resume flows.
2. Add env-gated provider integration tests beyond unit coverage.
3. Introduce a dedicated verifier abstraction beyond heuristics.
4. Tighten runtime UX around status, memory, approvals, and verification feedback.
5. Add provider-specific prompt shaping for weaker local models.
