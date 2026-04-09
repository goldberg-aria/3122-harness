# Prompt Runtime Audit

This document checks the current harness against seven prompt-engineering patterns described in the Claude Code analysis summary the user provided.

## Status summary

1. Agent runtime loop: implemented
2. Do/Don't contrast pattern: implemented in the shared loop prompt
3. Verification-before-completion: implemented with heuristics
4. Triple repetition for critical rules: implemented in the shared loop prompt
5. Turn budget and cost awareness: mostly implemented
6. Chain-of-thought stripping: implemented
7. Skill description budget: implemented
8. Context contamination prevention: mostly implemented

## Detailed review

### 1. Agent runtime loop

Status:

- implemented

Evidence:

- `crates/runtime/src/agent.rs`
- `crates/runtime/src/tools.rs`
- `crates/runtime/src/mcp.rs`
- `crates/runtime/src/skills.rs`

Notes:

- the harness already runs a real loop: prompt -> tool intent -> approval -> execution -> re-prompt

### 2. Do/Don't contrast pattern

Status:

- implemented in this pass

What changed:

- the shared loop prompt now includes explicit `Do:` and `Don't:` sections
- the rules use concrete limits such as `1 tool call at a time` and `12 lines by default`

Code:

- `crates/runtime/src/agent.rs`

### 3. Verification-before-completion

Status:

- implemented with heuristics

What is implemented:

- the prompt instructs the model to verify before claiming success
- the prompt requires an explicit `Not verified` statement when verification is not possible
- the harness now auto-annotates the final answer with `Not verified` when relevant workspace mutations were recorded but no valid verification step was recorded
- the harness now supports `off`, `annotate`, and `require` verification policies
- verification only counts when it occurs after the last mutation
- docs-only edits are exempt from mandatory verification

Gap:

- verification detection is still heuristic rather than provider- or build-system-aware
- there is no dedicated verifier abstraction yet

Next step:

- add a dedicated verifier abstraction and provider-aware suggestions

### 4. Triple repetition for critical rules

Status:

- implemented in this pass

What changed:

- critical rules appear at the front of the prompt
- the same rules are repeated in a middle reminder after context injection
- a final reminder is appended at the end of prompt assembly

Code:

- `crates/runtime/src/agent.rs`

### 5. Turn budget and cost awareness

Status:

- mostly implemented

What is implemented:

- `max_steps` exists in the loop
- prompt guidance now biases toward `read/grep/glob` before mutations
- `parallel_read` batches multiple safe read-only exploration operations into one turn

Gap:

- no token-budget-aware context compaction policy beyond basic recall truncation

Next step:

- add a context budget manager

### 6. Chain-of-thought stripping

Status:

- implemented in this pass

What changed:

- `<thinking>...</thinking>` blocks are removed from provider output before the harness stores or returns the final answer

Code:

- `crates/runtime/src/agent.rs`

### 7. Skill description budget

Status:

- implemented

What exists:

- skills are discovered and loaded from `SKILL.md`
- short summaries are capped to about 250 characters
- frontmatter `description:` is preferred when present

Gap:

- full skill contents are injected after explicit invocation

Next step:

- use the summary field for future automatic routing

### 8. Context contamination prevention

Status:

- mostly implemented

What exists:

- session files are isolated
- Local-Lite memory is project-local
- the shared loop prompt explicitly instructs the model to stay within the current workspace context
- prompt context includes `session_id`, `session_path`, and workspace boundary metadata
- conversation recall pulls only matching snippets from older sessions instead of dumping raw transcript history

Gap:

- there is no explicit fork/subtask isolation model yet

Next step:

- keep subtask context isolated when multi-agent or forked task execution is added

## Code changes made in this pass

- strengthened the shared agent loop prompt with explicit operating rules
- added verification language, verification policies, and concise-answer defaults
- added triple reminder placement for key rules
- stripped `<thinking>` blocks from provider final output
- added recent and relevant conversation recall to prompt context
- added short skill summaries and read-only batch exploration
- added tests covering prompt rule presence, thinking-strip behavior, verification policy, and batch exploration

## Follow-up order

1. stronger context budget manager
2. dedicated verifier abstraction
3. stronger session/task isolation metadata for future forked work
