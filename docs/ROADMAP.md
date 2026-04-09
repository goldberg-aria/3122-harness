# Roadmap

## Product direction

This project is not a wrapper around one preferred model.
It is a model-agnostic coding harness.

The harness should:

- wrap any model behind one consistent runtime
- extract the best practical performance from each model
- preserve task continuity even when the model changes
- keep safety, tool use, and session behavior stable across providers

The product advantage is:

- model independence
- durable local memory
- safe tool execution

## Core strategy

Three layers define the product.

### 1. Harness core

The harness owns:

- prompt assembly
- tool orchestration
- permissions
- approvals
- sessions
- skills
- MCP

### 2. Capability adaptation

Each provider has different strengths and weaknesses.
The harness should normalize those differences instead of exposing them to the user.

This layer should eventually handle:

- provider-native tool calling when available
- text tool-call fallback when native tool calling is missing
- model-specific prompt shaping
- streaming differences
- structured output differences

### 3. Local-Lite memory

Memory is a first-class product feature.
It should not require signup.

`Local-Lite` means:

- memory is stored in the project locally
- recall is done with simple local indexing and `rg`-style search
- the user can change models without losing working context

This is the v1 memory spine.

## V1 outcome

V1 is complete when the harness can:

- run coding tasks through multiple providers behind one runtime
- preserve useful working context locally across sessions
- let the user switch models without losing direction
- keep tool execution safe through permissions and approvals
- use skills and MCP as part of the same loop

## Explicit V1 scope

Included:

- terminal REPL and one-shot CLI
- provider-agnostic harness loop
- Anthropic, OpenAI-compatible, Ollama, Claude, and Codex lanes
- skill and MCP bridges
- prompt and auto approval policies
- Local-Lite memory stored under `.harness/`
- session replay and recall

Excluded:

- Nexus account login
- cloud sync
- vector search
- multi-agent orchestration
- polished TUI

## Workstreams

### Workstream 1: Local-Lite memory

Goal:

- create a local memory system that survives across runs and model changes

Deliverables:

- `.harness/memory/` layout
- memory record schema
- save/search/list/prune operations
- `rg`-based recall
- session summary promotion into memory

Suggested files:

- `crates/runtime/src/memory.rs`
- `crates/runtime/src/recall.rs`

Exit criteria:

- memory can be saved without external services
- recall can find relevant past facts
- tests cover save and search paths

### Workstream 2: Session-to-memory pipeline

Goal:

- connect short-term session history to long-term local memory

Deliverables:

- transcript summarization
- decision extraction
- task extraction
- error extraction
- recall injection at session start

Exit criteria:

- new sessions start with useful recall
- repeated work in the same repo benefits from prior sessions

### Workstream 3: Context budget manager

Goal:

- keep prompts coherent while fitting different model limits

Deliverables:

- ordered context layers
- truncation rules
- recall prioritization
- working-set compaction
- repeated prompt rules for verification, tool shape, and workspace boundaries
- relevant conversation recall from prior sessions

Context order:

1. runtime state
2. local instructions
3. recent turn history
4. Local-Lite recall
5. relevant conversation recall

Exit criteria:

- prompts stay stable in shape
- small models do not drown in context noise

### Workstream 4: Model capability normalization

Goal:

- make different models behave consistently behind the harness

Deliverables:

- provider capability table
- model-specific prompt rendering
- native tool-calling integration where supported
- fallback text tool-call path everywhere else
- final-output sanitization for provider-specific reasoning tags
- read-only discovery batching for fewer turns

Exit criteria:

- same task can run through at least three provider families
- harness behavior remains consistent despite provider differences

Current progress:

- native tool calling is wired for Anthropic, OpenAI-compatible providers, and Ollama
- text tool calling remains the common fallback path

### Workstream 5: Model switching

Goal:

- change models without losing task continuity

Deliverables:

- `/model` command
- handoff snapshot
- resume prompt synthesis
- provider switch logging

Exit criteria:

- user can switch models mid-session
- the next model starts with enough context to continue

### Workstream 6: Approval refinement

Goal:

- reduce friction without weakening safety

Deliverables:

- tool risk classification
- per-risk approval defaults
- keep current `prompt` and `auto` modes

Exit criteria:

- low-risk tools do not create unnecessary prompts
- high-risk tools still require strong review

### Workstream 7: Operator commands

Goal:

- expose memory and handoff behavior clearly to the user

Deliverables:

- `/memory`
- `/memory search <query>`
- `/memory save`
- `/resume`
- `/handoff`
- `/why-context`

Exit criteria:

- the user can inspect why the agent remembers something
- memory state is visible and debuggable

### Workstream 8: Verification

Goal:

- keep the harness stable while the runtime becomes more complex

Deliverables:

- unit tests for memory
- unit tests for recall
- tests for model switching
- tests for approval flows
- env-gated adapter integration tests
- a task-aware verification policy that detects unverified completion claims after the last relevant mutation
- tests for read-only batch exploration

Exit criteria:

- core runtime paths are covered by automated tests
- regressions are caught quickly

## Recommended implementation order

1. Local-Lite memory storage
2. session-to-memory pipeline
3. context budget manager
4. `/model` and handoff
5. provider-native tool calling
6. approval refinement
7. operator commands
8. release cleanup

## Release checklist

Before calling v1 done:

- memory survives restarts
- recall improves follow-up sessions
- model switching works
- approval behavior is predictable
- at least three provider families are usable
- README and architecture docs reflect the actual runtime
- tests cover the critical paths

## Product message

Short version:

- a model-agnostic coding harness with persistent local memory

Longer version:

- switch models without losing context
- keep memory local by default
- use one safe runtime for tools, skills, and MCP
