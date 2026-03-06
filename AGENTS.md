# Sunny Agent Engineering Standard

Agent-focused operating standard for the Sunny Rust multi-agent runtime.
This document is written for autonomous coding agents. Optimize for correctness,
traceability, and safe parallel execution.

## 0) Source of Truth and Priority

When rules conflict, use this priority order:
1. Rust compiler and Cargo behavior
2. This file (`AGENTS.md`)
3. In-code conventions already present in the touched module
4. External style preferences

If this file and repository reality diverge, fix the code or fix this file in the same change.

Definitions used in this policy:
- **Public API change**: signature/behavior change in `pub` items across crate boundaries.
- **Architecture change**: dependency-direction change, runtime semantic change, or protocol/message format change.
- **DoD** (Definition of Done): required checks and artifacts for the change class.

## 1) Multi-Agent Workflow (Mandatory)

Every non-trivial task follows this lifecycle:
1. **Discover**: inspect code paths, constraints, and existing patterns before edits.
2. **Plan**: define touched files, invariants, and verification commands.
3. **Execute**: make minimal, local changes; preserve module boundaries.
4. **Verify**: run mandatory quality gates for changed scope.
5. **Record**: update docs/ADRs when architecture or policy changed.

### 1.1 Delegation Contract

When delegating to another agent, include all fields:
- `task`: one atomic objective
- `inputs`: files and assumptions used
- `constraints`: forbidden actions and safety limits
- `expected_output`: concrete artifact format
- `verification`: commands/checks that must pass
- `rollback`: how to undo if verification fails

No delegation without an explicit verification step.

### 1.2 Handoff Rules

Handoff message must contain:
- current state and what is already verified
- exact files changed
- unresolved risks or unknowns
- next executable step (not abstract advice)

### 1.3 Change Classes and System Design Gates

Classify each change before implementation:
- **S** (small): one module/crate, no public API or runtime semantics changes.
- **M** (medium): multi-module or behavioral change across boundaries.
- **L** (large): public API change, dependency change, protocol change, or runtime scheduling/cancellation semantic change.

Required gates by class:
- **S**: unit tests for touched logic + mandatory quality gates.
- **M**: at least one integration test for cross-module behavior.
- **L**: ADR-lite note + integration test + observability field/event update + migration notes.

`sunny-core` public trait changes are always class **L**.

## 2) Rust Guidelines: Official vs Community

### 2.1 Official Rust Guidance (Required)

- Follow Rust API Guidelines for naming, trait ergonomics, and conversions.
- Follow Rust Style Guide (`rustfmt`) for formatting behavior.
- Follow Rust Book error-handling model: expected failures use `Result`, panic only for violated internal invariants.
- Follow Rustonomicon requirements for `unsafe`: every unsafe block requires a `// SAFETY:` proof comment.
- Follow Async Book model: do not block executor threads inside async paths.

Reference set:
- https://rust-lang.github.io/api-guidelines/checklist.html
- https://doc.rust-lang.org/style-guide/
- https://doc.rust-lang.org/book/ch09-00-error-handling.html
- https://doc.rust-lang.org/nomicon/
- https://rust-lang.github.io/async-book/

### 2.2 Community-Stable Guidance (Adopted Here)

- `thiserror` for domain errors; `anyhow` for application boundaries.
- `tracing` for logs and spans; no `println!` in runtime/library code.
- Tokio runtime only.
- `async-trait` allowed for trait-object async ergonomics.

## 3) Architecture and System Design Rules

### 3.1 Boundary Discipline

- `sunny-core` must not depend on CLI concerns.
- Keep crate interfaces narrow; prefer explicit types over ad-hoc maps at boundaries.
- One module, one concern. Split files once a module starts mixing orchestration, transport, and policy.

### 3.2 Decision Records (ADR-lite)

When changing architecture, append a short note to PR description or project doc with:
- **Context**: what constraint/problem existed
- **Decision**: what changed
- **Consequences**: tradeoffs and migration impact

No architecture-altering change ships without recorded rationale.

### 3.3 Failure Domains and Recovery

- Actor/task shutdown must be cancellation-aware (`CancellationToken`).
- External effects (I/O, process, network) must have timeout and error mapping.
- State-changing operations should be idempotent when possible.

## 4) Code Quality Gates (Mandatory)

For touched Rust crates, run in this order:
1. `cargo fmt --all -- --check`
2. `cargo clippy --workspace --all-targets --all-features -- -D warnings`
3. `cargo test --workspace`
4. `cargo check --workspace`

For public API changes, also run:
5. `cargo doc --workspace --no-deps`

If a gate fails, fix root cause or explicitly document pre-existing failure.

Definition of Done (DoD):
- **S**: `fmt + clippy + tests/check` pass for workspace.
- **M**: S requirements + integration test covering changed boundary.
- **L**: M requirements + ADR-lite rationale + `cargo doc --workspace --no-deps`.

## 5) Error Handling Policy

- Every public function in `sunny-core` returns typed `Result<T, E>`.
- No `.unwrap()` in library code (`sunny-core`, `sunny-mind`, `sunny-boys`).
- `.expect("reason")` allowed only with invariant explanation.
- Preserve causal chains (`source`) in error variants.

Example pattern:

```rust
#[derive(thiserror::Error, Debug)]
pub enum AgentError {
    #[error("agent {id} not found")]
    NotFound { id: String },
    #[error("execution timeout")]
    Timeout,
}
```

## 6) Async and Concurrency Policy

- Tokio-only runtime. No async-std, no smol.
- Prefer message passing to shared mutable state.
- In async contexts, use `tokio::sync` primitives, not `std::sync::Mutex`.
- Use `tokio::select!` for cancellation/timeouts/races.
- Use bounded channels unless unbounded is justified in code comments.

## 7) Trait and API Design

- Prefer thin traits (2-5 methods).
- Prefer associated types over wide generic fan-out when practical.
- Public traits require at least one test double (`#[cfg(test)]` stub/mock).
- For shared dynamic dispatch, prefer `Arc<dyn Trait + Send + Sync>`.

## 8) Module Organization

- Internal-only items default to `pub(crate)`.
- Cross-crate API must be intentionally `pub` and documented.
- Unit tests inline or adjacent; integration tests in `tests/`.

Current workspace paths:
- Agent traits: `crates/sunny-core/src/agent/`
- Tool traits: `crates/sunny-core/src/tool/`
- LLM abstractions: `crates/sunny-mind/src/`
- CLI commands: `crates/sunny-cli/src/commands/`

## 9) Testing Strategy

- `#[tokio::test]` for async tests.
- Add integration tests for cross-module orchestration paths.
- Use property tests (`proptest`) for serialization/protocol invariants.
- Test naming: `test_<unit>_<scenario>_<expected>`.

Minimum test expectations per change:
- bug fix: reproduce + prevent regression
- new behavior: happy path + one failure path
- concurrency change: cancellation or timeout behavior

## 10) Observability and Debuggability

- Use structured logs with stable fields (`agent`, `task_id`, `operation`, `duration_ms`).
- Emit error logs at failure boundaries with causal chain.
- Do not log secrets or full sensitive payloads.

## 11) Dependency and Security Hygiene

- No new dependency without `# dep: <justification>` comment in `Cargo.toml`.
- Prefer ecosystem-standard crates over bespoke utility crates.
- Keep dependencies scoped: runtime deps in `[dependencies]`, test-only deps in `[dev-dependencies]`.
- Remove unused dependencies in the same PR when discovered.

## 12) Prohibited Patterns (MUST NOT)

- `unsafe` without `// SAFETY:` explanation.
- `as any`-style type erasure equivalents (`Box<dyn Any>`) for core domain flow.
- Silent error swallowing.
- Panic-driven control flow in library crates.
- `println!` in runtime/library code.
- `std::sync::Mutex` in async-heavy paths.

## 13) Naming Conventions

- Modules: `snake_case`
- Types/traits: `PascalCase`
- Constants: `SCREAMING_SNAKE_CASE`
- Error types: suffix `Error`
- Builders: suffix `Builder`
- Avoid vague standalone type names: `data`, `info`, `manager`, `handler`

Allowed abbreviations: `msg`, `ctx`, `cfg`, `err`, `req`, `res`.

## 14) Commits

- Format: `type(scope): description` (imperative, <=72 chars)
- Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`
- Scopes: `core`, `cli`, `llm`, `agent`, `tool`, `orchestrator`
- No scope only for repo-wide changes

Example: `feat(agent): add capability-based permission system`

### 14.1 Lean Git History (Mandatory)

Keep the commit graph minimal and readable. One commit = one logical change.

**Group related changes into a single commit**:
- A feature and its tests → one commit
- A refactor and its fmt/clippy fixes → one commit
- Multiple small related additions (e.g. two new methods on the same struct) → one commit

**Do not create separate commits for**:
- Fixing a lint warning introduced in the same session
- Adding a test that was obviously required by the change
- Adjusting formatting of code you just wrote

**Each commit must**:
- Pass all quality gates on its own (no broken intermediate states)
- Be understandable from its subject line alone
- Contain only changes relevant to its stated purpose

**Anti-patterns (never do)**:
- `fix: address review comments` (squash into the original commit instead)
- `wip: partial implementation` on any persistent branch
- A chain of `fix fix fix` commits for the same bug
- Separate `test:` commit for tests that belong to a `feat:` or `fix:` commit

## 15) Agent Self-Check Before Finish

Before marking task complete, confirm:
1. Changed code follows existing module pattern.
2. Quality gates ran for touched scope.
3. Errors and cancellation paths were considered.
4. Tests prove behavior, not only compile success.
5. Any architectural change has a recorded rationale.

## 16) Minimal Adoption Order

When introducing this policy into new crates or modules, adopt in this order:
1. Quality gates and DoD mapping.
2. Change-class classification (S/M/L) and design gates.
3. Delegation/handoff contract enforcement.
4. Observability field consistency.
5. Drift-control hardening (warnings budget, flaky test policy).
