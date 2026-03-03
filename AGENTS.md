# Sunny — Rust Coding Conventions for AI Agents

Conventions for agents working on the Sunny multi-agent orchestration runtime.

## Error Handling

- Use `thiserror` for domain errors. Define typed enums per module.
- Use `anyhow` for application-layer errors (CLI, orchestrator main loop) where context > type.
- Every public function in `sunny-core` returns `Result<T, SomeSpecificError>`.
- NO `.unwrap()` in library code (`sunny-core`, `sunny-llm`). Use `?` operator.
- `.expect("descriptive reason")` allowed ONLY with message explaining invariant.

```rust
#[derive(thiserror::Error, Debug)]
pub enum AgentError {
    #[error("agent {id} not found")]
    NotFound { id: String },
    #[error("execution timeout")]
    Timeout,
}
```

## Async Patterns

- Tokio-only runtime. No async-std, no smol.
- Use `async_trait` crate for trait objects with async methods (until AFIT stabilizes).
- Prefer `tokio::spawn` for fire-and-forget concurrent work.
- Use `tokio::select!` for racing multiple futures.
- ALWAYS use `tokio_util::sync::CancellationToken` for graceful shutdown — pass it through the call stack.
- Avoid `std::sync::Mutex` in async code — use `tokio::sync::Mutex` or prefer message passing.

## Module Organization

- One module = one concern. `sunny-core/src/agent/mod.rs` exports only the public trait.
- Internal types: `pub(crate)`. Cross-crate types: `pub`. Never leak internals as `pub`.
- Test files: in-module via `#[cfg(test)]` mod tests block OR adjacent `agent_test.rs`.
- Module paths for this project:
  - Agent traits: `crates/sunny-core/src/agent/`
  - Tool traits: `crates/sunny-core/src/tool/`
  - LLM abstractions: `crates/sunny-llm/src/`
  - CLI commands: `crates/sunny-cli/src/commands/`

## Trait Design

- Prefer thin traits: 2-5 methods maximum per trait.
- Use associated types over generics where possible to avoid monomorphization explosion.
- Every public trait MUST have a mock/stub implementation in `#[cfg(test)]` for unit testing.
- Trait objects: prefer `Arc<dyn Trait>` over `Box<dyn Trait>` for shared ownership.

## Testing

- `#[tokio::test]` for ALL async tests. No `block_on` in tests.
- Use `test-log` crate for tracing output in test runs.
- Property-based testing with `proptest` for: serialization round-trips, message encoding/decoding.
- Integration tests in `tests/` directory at crate root, unit tests inline.
- Test naming: `test_<unit>_<scenario>_<expected>`.

## Naming Conventions

- Modules: `snake_case`. Types/Traits: `PascalCase`. Constants: `SCREAMING_SNAKE_CASE`.
- Allowed abbreviations: `msg`, `ctx`, `cfg`, `err`, `req`, `res`. All others: spell out.
- Error types: suffix with `Error` (e.g., `AgentError`, `RegistryError`).
- Builder types: suffix with `Builder` (e.g., `OrchestratorBuilder`).
- Avoid: `data`, `info`, `manager`, `handler` as standalone type names — too generic.

## Prohibited Patterns (MUST NOT)

- MUST NOT use `unsafe` blocks without `// SAFETY:` comment and code review.
- MUST NOT call `.clone()` without `// clone: <reason>` comment.
- MUST NOT use `Box<dyn Any>` — prefer concrete enums or typed trait objects.
- MUST NOT use `println!` — use `tracing::info!`, `tracing::debug!`, `tracing::error!`.
- MUST NOT panic in library crates (`sunny-core`, `sunny-llm`) — return `Result`.
- MUST NOT add new dependencies without a `// dep: <justification>` comment in Cargo.toml.
- MUST NOT use `std::sync::Mutex` in async contexts — use `tokio::sync::Mutex`.

## Dependencies (Approved Stack)

- Runtime: `tokio` (full features), `tokio-util`
- Traits: `async-trait`
- Serialization: `serde`, `serde_json`, `toml`
- Error handling: `thiserror`, `anyhow`
- CLI: `clap` (derive feature)
- Logging: `tracing`, `tracing-subscriber`
- Testing: `tokio-test`, `test-log`, `proptest`

## Commit Convention

- Format: `type(scope): description` (imperative, ≤72 chars)
- Types: `feat`, `fix`, `refactor`, `docs`, `test`, `chore`
- Scopes: `core`, `cli`, `llm`, `agent`, `tool`, `orchestrator`
- Example: `feat(agent): add capability-based permission system`
- NO scope: only for repo-wide changes
