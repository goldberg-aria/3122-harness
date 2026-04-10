# 3122 AMCP-Native Memory Directive 2026-04-10

## Copy-Paste Agent Prompt

```text
3122를 AMCP-native coding harness로 전환한다.

목표:
- 현재 terminal-first runtime, transcript logging, trajectory continuity, handoff 흐름은 유지한다.
- portable memory layer만 AMCP core shape로 재구성한다.
- Local-Lite 기본값은 유지한다.

반드시 지킬 것:
- `.harness/sessions/` JSONL transcript는 유지
- `trajectories`, `trajectory_steps`, `file_memory`, `skill_candidates`는 continuity runtime state로 유지
- portable memory는 별도 AMCP backend layer로 분리
- 로컬 backend와 Nexus cloud backend가 같은 외부 record shape를 써야 함
- 병렬의 새로운 public memory schema를 만들지 말 것

필수 구현:
1. memory backend abstraction 추가
2. LocalAmcpBackend 추가
3. NexusCloudBackend 추가
4. 현재 session promotion을 AMCP item 저장으로 전환
5. local export/import/migrate 명령 추가
6. prompt recall이 selected backend를 읽도록 수정

AMCP core 필드:
- id
- content
- type
- scope
- origin
- visibility
- retention
- tags
- metadata
- source_refs
- energy
- created_at
- updated_at

semantic profile은 우선 metadata에 유지:
- confidence
- inference_basis
- evidence_span
- decay_rate
- domain
- valence
- time_scope
- relations

주의:
- trajectories는 operational continuity용이지 portable atom schema가 아님
- external adapter lane이나 auth adapter는 이번 작업에서 건드리지 말 것
- full rewrite 금지
```

## Goal

Turn `3122` into an AMCP-native coding harness without breaking its current terminal-first runtime.

The harness keeps ownership of:

- conversation loop
- approval model
- tool execution
- session transcripts
- trajectory continuity
- model handoff

But the portable memory layer must stop using a harness-only schema.

## Product Position

`3122` is not "just another coding tool".

It is:

> An AMCP-native, model-neutral coding harness.

That means:

- local memory must be portable
- cloud memory must be swappable
- local and remote backends must share one external atom shape

## Hard Constraints

Do not break:

- `.harness/sessions/` JSONL transcript logging
- current `/resume`, `/handoff`, `/why-context`
- current trajectory continuity behavior
- local-first default
- no-signup local mode

Do not do a full rewrite.
Insert an AMCP-native memory layer under the existing runtime.
Do not change the external adapter lane in this pass.

## Architectural Decision

Split memory into two layers:

### Layer 1. Continuity Runtime State

Keep the current harness-native state for operational flow:

- `trajectories`
- `trajectory_steps`
- `file_memory`
- `skill_candidates`
- handoff snapshots

This is not the portable atom schema.

### Layer 2. AMCP Memory Backend

Add a real AMCP-native memory backend interface.

Required operations:

- `remember`
- `recall`
- `sessions`
- `session`
- `export`
- `import`
- `delete`

Initial backend implementations:

1. `LocalAmcpBackend`
2. `NexusCloudBackend`
3. `ThirdPartyAmcpBackend` stub with config shape only

## Canonical Record Rule

Every portable memory record in 3122 must serialize to the AMCP core shape:

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

Do not invent a parallel public schema.

## Semantic Profile Rule

The AMCP core stays thin.

Richer semantics should live in the semantic profile carried through `metadata` until the SDK exposes them first-class:

- `confidence`
- `inference_basis`
- `evidence_span`
- `decay_rate`
- `domain`
- `valence`
- `time_scope`
- `relations`

## Current Gap

Current 3122 local memory records are:

- `ts_ms`
- `kind`
- `title`
- `body`
- `tags`
- `session_path`

This is not AMCP-compatible.

Current trajectories are also continuity-focused, not portable atom records.

## Required Implementation Phases

### Phase 1. Introduce backend abstraction

Create a dedicated memory backend interface in runtime code.

Requirements:

- one trait or equivalent abstraction for AMCP operations
- one local implementation
- one Nexus cloud implementation
- configuration-based backend resolution

Do not wire cloud mode by replacing the harness runtime.
Wire it by swapping the memory backend.

### Phase 2. Add AMCP-native local storage

Local mode must store AMCP-native records.

Minimum local storage requirements:

- stable `id`
- `content`
- `type`
- serialized `scope`
- serialized `origin`
- `visibility`
- serialized `retention`
- `tags`
- `metadata`
- `source_refs`
- `energy`
- `created_at`
- `updated_at`

Recommended approach:

- keep the existing SQLite database
- add AMCP tables rather than replacing `trajectories`
- use FTS for local recall

Do not delete the existing trajectory tables in this phase.

### Phase 3. Re-map current save pipeline

Current session promotion should write AMCP items instead of `MemoryRecord` JSONL as the primary portable store.

Mapping baseline:

- `Summary` -> `context` or `artifact`, choose one and standardize
- `Decision` -> `decision`
- `Task` -> `task`
- `Error` -> `error`
- `Note` -> `context`

Use one explicit mapping table in code.
Do not scatter this logic.

### Phase 4. Keep trajectories as continuity metadata

Trajectories should continue to exist, but they should not be treated as the primary portable memory format.

Use trajectories for:

- active task continuity
- handoff snapshots
- file-centric recall boost
- skill candidate mining

Use AMCP items for:

- durable portable memory
- backend migration
- export/import
- cloud switchover

### Phase 5. Add export/import and migration commands

Required user-facing flows:

- export local portable memory
- import portable memory
- migrate local -> Nexus cloud
- optional dual-write mode during migration

Target commands:

- `memory export --format amcp-jsonl`
- `memory import --format amcp-jsonl`
- `memory migrate --from local --to nexus-cloud`

Names can vary, but the flow must exist.

### Phase 6. Add backend-aware prompt context

Prompt context should keep using:

- active trajectory
- relevant file memory
- recent working history

But portable memory recall must come from the selected AMCP backend.

That means:

- local mode recalls from `LocalAmcpBackend`
- hosted mode recalls from `NexusCloudBackend`
- future third-party backends can plug in without prompt rewrites

## Data Model Decisions

### Scope

Default local scope:

```json
{ "kind": "user", "id": "local-workspace" }
```

You may refine the `id`, but it must be stable and explicit.

### Origin

Local origin should include:

- `agent_id`
- `app_id`
- `session_id`
- `timestamp`

Recommended values:

- `agent_id`: current harness agent identifier
- `app_id`: `3122`

### Visibility

Default local visibility:

- `private`

### Retention

Default local retention:

- `{ "mode": "persistent" }`

### Source refs

If the memory came from file work, include source refs instead of dropping them.

Recommended shape:

```json
[
  { "kind": "file", "uri": "file:///absolute/path/to/file" }
]
```

## Explicit Non-Goals For This Pass

- full semantic graph implementation
- seven relation types inside 3122 local storage on day one
- replacing trajectories with atoms
- cloud signup UX
- rewriting the main REPL loop

## Acceptance Criteria

The work is done when all of the following are true:

1. Local 3122 memory records are stored and exported as AMCP-native items
2. Existing trajectory continuity features still work
3. The memory backend can be swapped without changing prompt-building logic
4. Local export produces portable JSON or JSONL in AMCP shape
5. A future Nexus cloud backend can be added without changing the record schema

## Implementation Order

1. Introduce backend abstraction
2. Add AMCP-native local tables and serializers
3. Re-map session promotion into AMCP items
4. Route recall through the backend layer
5. Add export/import/migrate commands
6. Add hosted Nexus backend

## Final Rule

Do not design a second memory protocol inside 3122.

`3122` should own the harness runtime.
`AMCP` should own the portable memory contract.
