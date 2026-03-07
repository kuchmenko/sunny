use std::sync::Arc;

use sunny_core::agent::{AgentHandle, Capability};
use sunny_core::orchestrator::{AgentRegistry, RegistryError};
use sunny_mind::LlmProvider;
use tokio_util::sync::CancellationToken;

use crate::background::BackgroundTaskManager;
use crate::codebase::CodebaseAgent;
use crate::critique::CritiqueAgent;
use crate::delegate::DelegateAgent;
use crate::explore::ExploreAgent;
use crate::oracle::OracleAgent;
use crate::review::ReviewAgent;

fn register_explore_oracle_agents(
    registry: &mut AgentRegistry,
    provider: Option<Arc<dyn LlmProvider>>,
    token: &CancellationToken,
) -> Result<(), RegistryError> {
    let explore = AgentHandle::spawn(
        Arc::new(ExploreAgent::new(provider.clone())),
        token.child_token(),
    );
    registry.register(
        "explore".into(),
        explore,
        vec![Capability("explore".into())],
    )?;

    let oracle = AgentHandle::spawn(Arc::new(OracleAgent::new(provider)), token.child_token());
    registry.register("oracle".into(), oracle, vec![Capability("advise".into())])?;

    Ok(())
}

fn register_core_agents(
    registry: &mut AgentRegistry,
    provider: Option<Arc<dyn LlmProvider>>,
    token: &CancellationToken,
) -> Result<(), RegistryError> {
    let codebase_token = token.child_token();
    let codebase = AgentHandle::spawn(
        Arc::new(CodebaseAgent::with_cancel(
            provider.clone(),
            codebase_token.clone(),
        )),
        codebase_token,
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

    Ok(())
}

pub fn build_boys_registry(
    provider: Option<Arc<dyn LlmProvider>>,
    token: &CancellationToken,
) -> Result<AgentRegistry, RegistryError> {
    let mut registry = AgentRegistry::new();
    register_core_agents(&mut registry, provider.clone(), token)?;

    register_explore_oracle_agents(&mut registry, provider.clone(), token)?;

    let mut delegate_registry = AgentRegistry::new();
    register_core_agents(&mut delegate_registry, provider.clone(), token)?;
    register_explore_oracle_agents(&mut delegate_registry, provider, token)?;

    let background = Arc::new(BackgroundTaskManager::new(10, token.child_token()));

    let delegate = AgentHandle::spawn(
        Arc::new(DelegateAgent::new(Arc::new(delegate_registry), background)),
        token.child_token(),
    );
    registry.register(
        "delegate".into(),
        delegate,
        vec![Capability("delegate".into())],
    )?;

    Ok(registry)
}

#[cfg(test)]
mod tests {
    use sunny_core::agent::Capability;
    use tokio_util::sync::CancellationToken;

    use super::build_boys_registry;

    #[tokio::test]
    async fn test_build_boys_registry_includes_all_agents() {
        let token = CancellationToken::new();
        let registry = build_boys_registry(None, &token).expect("should build registry");

        assert!(registry.find("codebase").is_some());
        assert!(registry.find("review").is_some());
        assert!(registry.find("critique").is_some());
        assert!(registry.find("explore").is_some());
        assert!(registry.find("oracle").is_some());
        assert!(registry.find("delegate").is_some());

        token.cancel();
    }

    #[tokio::test]
    async fn test_build_boys_registry_capabilities() {
        let token = CancellationToken::new();
        let registry = build_boys_registry(None, &token).expect("should build registry");

        let query_agents = registry.find_by_capability(&Capability("query".into()));
        assert!(query_agents.contains(&"codebase"));

        let analyze_agents = registry.find_by_capability(&Capability("analyze".into()));
        assert!(analyze_agents.contains(&"review"));

        let action_agents = registry.find_by_capability(&Capability("action".into()));
        assert!(action_agents.contains(&"critique"));

        let explore_agents = registry.find_by_capability(&Capability("explore".into()));
        assert!(explore_agents.contains(&"explore"));

        let advise_agents = registry.find_by_capability(&Capability("advise".into()));
        assert!(advise_agents.contains(&"oracle"));

        let delegate_agents = registry.find_by_capability(&Capability("delegate".into()));
        assert!(delegate_agents.contains(&"delegate"));

        token.cancel();
    }
}
