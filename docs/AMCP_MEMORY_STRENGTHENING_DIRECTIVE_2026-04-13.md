# 3122 AMCP Memory Strengthening Directive 2026-04-13

This document is the source of truth for the post-AMCP-native strengthening pass.

It preserves the existing split:

- continuity runtime state stays harness-owned and local
- portable memory stays AMCP-owned and backend-swappable

This pass adds five concrete reinforcements:

1. Trajectory-driven auto-promotion with `[memory].auto_promote_policy = off | suggest | auto`
2. Budget-aware portable recall using `RecallRequest` plus `context_budget`
3. Prompt compaction checkpoints emitted as `compaction-checkpoint` AMCP items
4. Provenance stored in `metadata.provenance`, never as a new top-level public field
5. Capability advertisement through backend `capabilities()` and hosted `GET /v1/amcp/capabilities`

Implementation rules:

- `suggest` stores pending promotion candidates in local continuity storage
- `auto` writes directly to the selected AMCP backend
- all trajectory-derived or compaction-derived items use `retention.mode = "session-derived"`
- all harness-generated AMCP items carry `metadata.provenance`
- local and hosted backends continue to share one AMCP item shape
- `.harness/sessions/` JSONL transcripts and trajectory continuity remain intact

Current trigger set:

- verification passed after workspace mutation with final assistant result present
- repeated normalized failure across at least 3 distinct sessions
- skill candidate promoted into a real slash command
- model handoff followed by first assistant result, consuming the boost

Current CLI/REPL surface added by this pass:

- `memory candidates`
- `memory promote <index>`
- `memory dismiss <index>`

This directive extends the AMCP-native memory work without replacing the original directive in
`docs/AMCP_NATIVE_MEMORY_DIRECTIVE_2026-04-10.md`.
