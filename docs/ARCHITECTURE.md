# Architecture

## Product brief

We are building a terminal-based coding agent harness.

The harness is the product.
The model is replaceable.

The harness owns:

- the conversation loop
- permission checks
- tool execution
- skill discovery and invocation
- MCP server discovery and invocation
- session persistence
- provider selection
- external adapter integration

The runtime should be able to wrap different models while keeping one stable operational surface.

## Why this shape

The core value is not a specific model.
The core value is a durable local runtime that can:

- run safely inside a real workspace
- switch between remote and local models
- switch models without losing continuity
- preserve session state
- preserve memory locally across runs
- preserve active task trajectories across runs and model switches
- support tool-driven coding flows

The system should maximize the effective quality of each model by compensating for provider differences inside the harness.

Prompt discipline is part of the runtime, not an afterthought.
The harness should encode operational rules that keep weaker models useful and stronger models consistent.

## Primary goals

1. Terminal-first UX
2. Strong permission boundaries
3. BYOK by default
4. Ollama support for local models
5. External adapter lane for authenticated CLIs such as Claude Code and Codex
6. First-class skills and MCP surfaces
7. Local-Lite memory by default with no signup requirement
8. Clean-room architecture with no dependency on copied private internals

## Explicit non-goals for v1

- parity with every Claude Code feature
- full TUI polish
- multi-agent orchestration
- hooks and plugins
- full MCP lifecycle management
- Norfolk or MaaS direct hosted backend support

## Memory spine

Local memory now has three layers:

- JSONL transcripts under `.harness/sessions/`
- SQLite trajectory memory under `.harness/memory.db`
- AMCP-native portable memory backends behind one runtime interface

The transcript is the raw event log.
The trajectory store is the compressed operational memory used for continuity.
The AMCP backend is the portable memory layer used for durable recall, export/import, and backend migration.

Trajectory rows capture:

- current goal
- active model and previous model
- active files
- latest attempt
- latest failure
- last verification
- next step

File memory is derived from trajectory activity.
When a query or active trajectory points at concrete files, the prompt context includes a small file-level recall block with the latest goal, failure, and verification attached to those files.

This is the main continuity layer for `/resume`, `/handoff`, `/why-context`, and post-switch handoff boost.

The same store also tracks repeated tool sequences as `skill_candidates`.
Candidates are suggested first and only become reusable slash commands when promoted.

Portable memory records are serialized in one AMCP core shape across all backends:

- `id`
- `content`
- `type`
- `scope`
- `origin`
- `visibility`
- `retention`
- `tags`
- `metadata`
- `source_refs`
- `energy`
- `created_at`
- `updated_at`

The local backend is the default.
`nexus-cloud` and `third-party-amcp` share the same external item schema and can be swapped in through config without rewriting prompt assembly.

Current hosted implementation:

- `nexus-cloud` is wired to the Nexus `/v1/amcp` contract
- the harness sends one AMCP item shape in local and hosted modes
- hosted sessions come from `GET /v1/amcp/sessions` and `GET /v1/amcp/sessions/:id`
- hosted chain items may include continuity fields, but the harness only requires AMCP core plus optional profile fields

## Core loop

The runtime should follow this loop:

1. Read user input
2. Build request context
3. Send request to selected provider or adapter
4. Parse assistant output for tool intents
5. Validate and authorize each tool call
6. Execute tool
7. Feed results back into the loop
8. Persist transcript after each step
9. Render final answer

Current implementation note:

- v1 keeps a provider-agnostic text tool protocol as the common fallback
- the model can emit a single `<tool_call>...</tool_call>` block containing JSON
- the harness executes the request, appends a tool result, and re-prompts
- Anthropic BYOK, OpenAI-compatible BYOK, and Ollama now also expose the same built-in tools through native tool calling
- this keeps Claude adapter and Codex adapter on one common fallback path
- the shared prompt now encodes explicit `Do` / `Don't` rules, verification reminders, and repeated boundary reminders
- `<thinking>...</thinking>` output is stripped before the final answer is surfaced
- verification only counts when it happens after the last workspace mutation
- docs-only edits are exempt from mandatory verification
- if relevant workspace mutations happen without a recorded verification step, the harness annotates the final answer as `Not verified`
- verification policy can now be `off`, `annotate`, or `require`
- `parallel_read` batches multiple safe read-only discovery operations into one turn
- prompt context uses model-aware budget profiles and shrinks conversation recall, memory recall, recent history, handoff detail, and instructions in that order
- verification decisions are centralized in a verifier module so policy stays separate from the main loop
- verifier guidance now considers workspace manifests so project-level commands can be suggested even when file extensions are ambiguous
- weaker local and open-weight model families use a more compact prompt shape with tighter line limits and shorter step guidance

Planned evolution:

- use provider-native tool calling when a backend supports it well
- preserve the text tool-call protocol as the common fallback

Current provider-native state:

- Anthropic exposes native tools
- OpenAI-compatible backends expose native tools
- Ollama exposes native tools
- external CLI adapters currently stay on the text tool path

## Provider strategy

We support three backend families.

### 1. Native BYOK APIs

- Anthropic via `ANTHROPIC_API_KEY`
- OpenAI-compatible providers via `OPENAI_API_KEY` and custom base URLs

The OpenAI-compatible lane should cover provider families such as:

- OpenAI
- OpenRouter
- DeepSeek
- DashScope / Qwen endpoints
- SiliconFlow

This lane is the default production path.

### 2. Local Ollama

- endpoint-based local transport
- no cloud credential required
- useful for private/offline development

### 3. External authenticated adapters

- Claude Code
- Codex CLI

Important boundary:

- We do not scrape or reuse private login state directly.
- We integrate through official CLI binaries, documented env vars, or explicit adapter contracts.

This keeps the design defensible and reduces fragile coupling.

Connection policy:

- the fixed default is `api`
- interactive sessions default to `auto`
- `api` is the stable and testable path
- `auth` is a convenience lane for personal subscribed tools
- `auto` should resolve in this order:
  1. use an explicit BYOK profile or API key when present
  2. fall back to an auth adapter only when the route officially supports it
  3. otherwise surface a clear setup error

Provider policy for v1:

- Claude: `api` and `auth`
- OpenAI/Codex: `api` and `auth`
- Z.AI: `api` first
- MiniMax: `api` first
- Groq / Qwen API / DeepInfra / other OpenAI-compatible routes: `api` first
- Ollama: local API only
- runtime target resolution now follows this policy
- the CLI auto-loads workspace `.env` files so local BYOK setup can stay project-scoped

Saved provider profiles:

- the harness can store reusable BYOK profiles locally
- key-prefix detection may suggest a provider, but must not spray the key across multiple endpoints
- unknown keys should require either a preset or an explicit base URL

## Permission model

Three default modes:

- `read-only`
- `workspace-write`
- `danger-full-access`

Rules:

- file writes outside workspace require escalation
- shell commands are classified conservatively
- destructive commands are denied or escalated

Current approval behavior:

- model-requested tools go through an approval gate
- default approval policy is `prompt`
- every request is classified as `low`, `medium`, `high`, or `critical`
- `prompt` mode auto-approves low-risk tools, prompts for medium/high, and blocks critical
- `auto` mode auto-approves low/medium/high and still blocks critical
- REPL can switch policy at runtime

Context budget behavior:

- recent history, Local-Lite recall, and conversation recall are all trimmed before the final prompt is assembled
- if the total prompt context still exceeds budget, instruction text is truncated last

## Workspace context

Every turn should have access to:

- current working directory
- git branch and dirty status
- project instruction files such as `AGENTS.md` or `CLAUDE.md`
- model/provider selection
- active permission mode
- active approval policy

Context should eventually be layered in this order:

1. runtime state
2. local instruction files
3. recent working history
4. Local-Lite recall
5. relevant conversation recall

Prompt assembly principles:

- repeat the most important rules at the front, middle, and end of the prompt
- prefer explicit `Do` / `Don't` operating rules over vague style guidance
- require verification before completion claims whenever possible
- keep final-answer expectations short and concrete
- keep session boundaries explicit so unrelated context does not leak across tasks
- pull only relevant snippets from prior sessions; do not dump raw transcript history wholesale

## Session model

Store sessions as JSONL.

Each event should be append-only:

- user turn
- assistant turn
- tool request
- tool result
- approval event
- model change
- model handoff
- model probe failure
- usage accounting

This keeps resume and audit straightforward.

Boundary strengthening:

- prompt context includes `session_id`, `session_path`, and an explicit workspace boundary line
- REPL emits a `session_start` event with workspace scope metadata
- model switching stores a structured handoff snapshot in the session event log
- the first turn after a model switch gets a temporary handoff boost in prompt context

## Local-Lite memory

Local-Lite memory is the default persistence layer.

Principles:

- no account required
- stored locally inside the project
- cheap recall using local search and structured summaries
- survives model switching

Planned storage root:

- `.harness/memory/`

Planned memory categories:

- summaries
- decisions
- tasks
- errors
- notes

The transcript is not the memory system.
The transcript is raw history.
Local-Lite memory is the compressed, reusable context layer built from that history.

## Skills

Skills are promptable workflow bundles discovered from common project and user directories.

Initial discovery roots:

- `.harness/skills`
- `.agents/skills`
- `.codex/skills`
- `.claude/skills`
- `~/.harness/skills`
- `~/.agents/skills`
- `~/.codex/skills`
- `~/.claude/skills`

Current implementation:

- `skills list`
- resolve skill metadata from `SKILL.md`
- prefer frontmatter `description:` as the short routing summary when present
- cap discovered skill summaries to about 250 characters
- expose `skill` as a runtime tool action that returns a prompt packet

## MCP

MCP is a required integration surface.

Initial shape:

- discover configured MCP servers from local JSON config
- expose `mcp list`
- expose `mcp_list_tools` and `mcp_call` inside the runtime loop
- keep stdio bridge minimal and test it with a local mock server

Planned local config file:

```json
{
  "servers": [
    {
      "name": "filesystem",
      "transport": "stdio",
      "command": "npx @modelcontextprotocol/server-filesystem .",
      "enabled": true
    }
  ]
}
```

## Crate layout

```text
crates/
  cli/        terminal entrypoint and REPL
  runtime/    shared types, provider registry, permissions, doctor surfaces
```

Planned crates after v1 scaffolding:

```text
crates/
  cli/
  runtime/
  session/
  tools/
  providers/
  adapters/
```

## External adapter design

External adapters should expose a narrow internal interface:

- `name()`
- `is_available()`
- `auth_status()`
- `send_prompt()`
- `stream_prompt()` later

The adapter itself may call:

- `claude ...`
- `codex ...`

But the harness should treat that as a backend transport, not as part of the core runtime.

## Current implementation sequence

1. `harness.toml`
2. JSONL session store
3. permission evaluator
4. `read` / `write` / `edit` / `grep` / `glob` / `exec`
5. skill discovery and prompt packets
6. MCP discovery and stdio bridge
7. Anthropic BYOK provider
8. OpenAI-compatible BYOK provider
9. Ollama provider
10. Claude/Codex CLI adapters
11. one-shot `prompt` command
12. resumable REPL
13. tested provider-agnostic tool loop
14. prompt/auto approval policy
15. context assembly from local instructions and git state
16. structured model-switch handoff and first-turn boost
17. dedicated verifier abstraction
18. env-gated live provider integration tests

## Test stance

Important runtime boundaries should stay covered by unit tests:

- config loading and defaults
- permission allow/deny decisions
- model target parsing and URL joining
- skill resolution precedence
- MCP stdio listing and calling
- agent loop progress, max-step guard, skill tool, and MCP tool path
- verifier policy and post-mutation verification timing

The next testing focus should be:

- saved provider profile flows against live endpoints when env credentials exist
- external adapter smoke coverage for `claude` and `codex`
- model-aware context budget assertions for smaller local models

## Planning doc

Execution detail is tracked in [ROADMAP.md](/Users/paul_k/Documents/p-23/3122/docs/ROADMAP.md).

## Naming

The current command name is `harness`.

This is intentionally plain until product naming is fixed.
