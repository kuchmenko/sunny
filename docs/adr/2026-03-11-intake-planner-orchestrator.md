## Context

`ask` requests were planned as a single execution step with capability chosen mostly from
raw text classification or intake LLM suggestion. Intake advisory did not receive workspace
signals, and `advise` capability could become the direct terminal route for ambiguous queries.

## Decision

- Added workspace-aware intake context (`WorkspaceContext`) and passed it to the intake advisor
  prompt.
- Refactored planner to emit staged plans: `context_gather` -> `evidence_check` -> `plan_finalize`
  with optional `oracle_validation`.
- Added planner stop/continue controls via bounded planning iterations and complexity-aware stage
  selection.
- Added explicit step dependencies (`depends_on`) and executor dependency readiness handling.
- Rebalanced `advise` usage so ambiguous query flows default to a primary non-Oracle step, with
  Oracle used as advisory validation when warranted.

## Consequences

- Planning becomes multi-step and dependency-aware, improving context collection before final
  execution.
- Oracle is no longer the default terminal path for broad, ambiguous advisory-style prompts.
- Output metadata now reflects multi-step execution counts, and plan validation now enforces
  dependency integrity.
