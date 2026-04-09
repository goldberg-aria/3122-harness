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
- a dedicated verifier module for task-aware verification that requires checks after the last code mutation while exempting docs-only edits
- project-aware verifier suggestions that inspect workspace manifests such as `Cargo.toml`
- automatic model-switch handoff snapshots with first-turn context boost
- runtime unit tests covering config, permissions, providers, skills, MCP, the harness loop, and verifier behavior
- env-gated live provider tests for Anthropic, OpenAI-compatible, and Ollama
- more readable CLI status, memory, approval, and verification feedback
- compact prompt shaping for weaker local and open-weight model families such as Ollama, Qwen, Llama, Gemma, Mistral, Phi, and DeepSeek
- model-aware context budget profiles with compact recall limits for smaller model families
- runtime `api/auth/auto` target resolution for interactive and one-shot modes

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
cargo run -p cli -- providers sync-env
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

Provider connection policy:

- `[providers].default_connection_mode = "api"` is the fixed default
- `[providers].interactive_connection_mode = "auto"` is the fixed interactive default
- `api` means use BYOK/API lanes first
- `auth` means prefer authenticated adapters such as `claude-code/...` and `codex/...`
- `auto` means prefer API when a configured key/profile exists, otherwise use an auth adapter when the route supports it
- runtime resolution now honors this policy
- current route policy:
  - Claude: `api` and `auth`
  - OpenAI/Codex: `api` and `auth`
  - Z.AI: `api` first
  - MiniMax: `api` first
  - Groq / Qwen API / other OpenAI-compatible routes: `api` first
  - Ollama: local API only
- the CLI now auto-loads `.env` and `.env.local` from the workspace root

Verification behavior:

- default verification policy comes from `[verification].policy`
- verification must happen after the last workspace mutation to count
- docs-only edits do not force a verification step
- verification logic is centralized in a dedicated verifier module
- `require` rejects completion after relevant workspace mutations unless a verification step was recorded or the model explicitly says `Not verified`
- `annotate` prefixes the final answer with `Not verified` plus task-aware guidance
- `off` disables verification enforcement

REPL shape:

- the base flow is closer to Codex/Claude-style slash commands
- primary session commands are `/status`, `/model`, `/login`, `/memory`, `/resume`, `/handoff`, `/why-context`, `/approval`, `/doctor`
- `/model <spec>` switches the active model, stores a handoff snapshot, and prints a short active/previous/next summary
- `/parallel-read <json-array>` batches read-only discovery work in one turn
- `/status` shows the current connection mode and behavior

Local-Lite memory:

- `memory save` promotes the latest session into local memory
- `memory` lists recent memory records
- `memory search <query>` searches saved memory locally
- `resume` and `handoff` render session and handoff state back into operator-friendly text
- `why-context` prints the exact runtime context injected before a model turn
- REPL exit autosaves new local memory records for the current session
- prompt context now includes recent working history, Local-Lite memory recall, and relevant conversation recall from older sessions
- the first prompt after `/model <spec>` gets a temporary handoff boost in prompt context
- prompt context is budgeted so long sessions degrade conversation recall first, then memory, recent history, and finally instructions
- `/status` shows approval and verification behavior alongside provider and memory state
- `/memory` shows per-kind counts plus the most recent records

Skill summaries:

- discovered skills now expose a routing-friendly summary capped to about 250 characters
- frontmatter `description:` is preferred when present

Saved provider profiles:

- `providers add` stores BYOK profiles in `.harness/providers.json`
- auto-detection currently recognizes some key formats such as OpenRouter and OpenAI
- manual registration supports presets such as `deepseek`, `dashscope-cn`, `dashscope-intl`, `siliconflow`, `groq`, `minimax`, `deepinfra`, and `zai-coding`
- saved profiles can be used as `profile/<alias>/<model>`
- `providers sync-env` saves env-backed profiles such as `anthropic-api`, `openai-api`, `groq`, `qwen-api`, `zai`, `minimax`, and `deepinfra`

What to prepare:

- Claude API key if you want the API lane
- OpenAI API key if you want the API lane
- Z.AI API key and the base URL you want to standardize on
- MiniMax API key and the compatibility mode you want to use first
- DeepInfra API key if you want its OpenAI-compatible lane in the matrix
- Groq and Qwen API keys if you want them in the matrix
- local Ollama models pulled in advance, at least one Gemma and one Qwen model
- authenticated CLI login for `claude` and `codex` if you want auth-lane smoke tests
- short aliases for saved profiles such as `zai`, `minimax`, `groq`, `qwen-api`

Live provider tests:

- `HARNESS_RUN_LIVE_PROVIDER_TESTS=1 cargo test --workspace`
- `OPENAI_API_KEY` enables the OpenAI-compatible live test
- `ANTHROPIC_API_KEY` enables the Anthropic live test
- `HARNESS_TEST_OLLAMA_MODEL` or `OLLAMA_HOST` enables the Ollama live test
- `HARNESS_TEST_SAVED_PROFILE_BASE_URL` and `HARNESS_TEST_SAVED_PROFILE_API_KEY` enable saved-profile live tests
- `HARNESS_RUN_AUTH_ADAPTER_TESTS=1` enables `claude-code` and `codex` auth-adapter smoke tests
- optional model overrides:
  - `HARNESS_TEST_OPENAI_MODEL`
  - `HARNESS_TEST_ANTHROPIC_MODEL`
  - `HARNESS_TEST_OLLAMA_MODEL`
  - `HARNESS_TEST_SAVED_PROFILE_ROUTE`
  - `HARNESS_TEST_SAVED_PROFILE_MODEL`
  - `HARNESS_TEST_SAVED_PROFILE_ALIAS`
  - `HARNESS_TEST_CLAUDE_CODE_MODEL`
  - `HARNESS_TEST_CODEX_MODEL`
- `scripts/run_provider_matrix.sh` syncs env-backed profiles, chooses local Ollama defaults, and runs the live smoke test suite
- current saved-profile defaults in the matrix script:
  - Z.AI: `5.1`
  - MiniMax: `2.7`
  - Groq: `openai/gpt-oss-20b`
  - Qwen API via OpenRouter: `qwen/qwen3.6-plus`
  - DeepInfra: `nvidia/Nemotron-3-Nano-30B-A3B`

## Immediate next steps

1. Add stronger REPL-level tests for status and handoff presentation.
2. Add provider-specific output shaping for external CLI adapters.
3. Add more explicit memory and handoff debugging commands.
4. Expand saved-profile presets for Z.AI, MiniMax, and Groq.
5. Add matrix scripts for the representative model suite.
