//! Registry builders for common agent configurations.

use std::sync::Arc;

use sunny_core::agent::{AgentHandle, Capability};
use sunny_core::orchestrator::{AgentRegistry, RegistryError};
use sunny_mind::LlmProvider;
use tokio_util::sync::CancellationToken;

use crate::codebase::CodebaseAgent;
use crate::critique::CritiqueAgent;
use crate::review::ReviewAgent;

/// Builds an [`AgentRegistry`] pre-populated with the agents used by `sunny ask`.
///
/// Registers:
/// - `codebase` agent with `"query"` capability
/// - `review` agent with `"analyze"` capability
/// - `critique` agent with `"action"` capability
pub fn build_ask_registry(
    provider: Option<Arc<dyn LlmProvider>>,
    token: &CancellationToken,
) -> Result<AgentRegistry, RegistryError> {
    let mut registry = AgentRegistry::new();

    let codebase = AgentHandle::spawn(
        Arc::new(CodebaseAgent::new(provider.clone())),
        token.child_token(),
    );
    registry.register(
        "codebase".into(),
        codebase,
        vec![Capability("query".into())],
    )?;

    let review = AgentHandle::spawn(
        Arc::new(ReviewAgent::new(provider.clone())),
        token.child_token(),
    );
    registry.register("review".into(), review, vec![Capability("analyze".into())])?;

    let critique = AgentHandle::spawn(Arc::new(CritiqueAgent::new(provider)), token.child_token());
    registry.register(
        "critique".into(),
        critique,
        vec![Capability("action".into())],
    )?;

    Ok(registry)
}

#[cfg(test)]
mod tests {
    use sunny_core::agent::Capability;
    use tokio_util::sync::CancellationToken;

    use super::build_ask_registry;

    #[tokio::test]
    async fn test_build_ask_registry_registers_three_agents() {
        let token = CancellationToken::new();
        let registry = build_ask_registry(None, &token).expect("should build registry");

        assert!(registry.find("codebase").is_some());
        assert!(registry.find("review").is_some());
        assert!(registry.find("critique").is_some());

        token.cancel();
    }

    #[tokio::test]
    async fn test_build_ask_registry_capabilities() {
        let token = CancellationToken::new();
        let registry = build_ask_registry(None, &token).expect("should build registry");

        let query_agents = registry.find_by_capability(&Capability("query".into()));
        assert!(query_agents.contains(&"codebase"));

        let analyze_agents = registry.find_by_capability(&Capability("analyze".into()));
        assert!(analyze_agents.contains(&"review"));

        let action_agents = registry.find_by_capability(&Capability("action".into()));
        assert!(action_agents.contains(&"critique"));

        token.cancel();
    }
}
