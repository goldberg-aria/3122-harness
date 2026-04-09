# Project Bootstrap

Use this skill when starting implementation from an approved architecture.

## Goals

- inspect the current workspace layout
- verify provider and adapter configuration
- confirm permission mode before running mutating commands
- create the first vertical slice instead of spreading across the whole codebase

## Checklist

1. Read `docs/ARCHITECTURE.md`.
2. Run `harness doctor`.
3. Run `harness providers`.
4. Confirm whether the task is provider work, tool work, or adapter work.
5. Implement one narrow slice end to end.
