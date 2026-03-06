use crate::agent::{AgentHandle, Capability};
use crate::orchestrator::events::EVENT_ROUTE_RESOLVED;
use crate::orchestrator::OrchestratorError;
use tracing::info;

use super::AgentRegistry;

/// Name-based routing strategy: resolves agents by exact name match.
///
/// This trait is frozen per architectural contract and must not be modified.
pub trait RoutingStrategy: Send + Sync {
    fn resolve<'a>(&self, agent_name: &str, registry: &'a AgentRegistry)
        -> Option<&'a AgentHandle>;
}

/// Capability-first routing strategy: resolves agents by capability match.
///
/// This trait enables intent-driven orchestration by routing based on agent capabilities
/// rather than explicit names. It coexists with `RoutingStrategy` to preserve backward
/// compatibility while enabling new capability-based routing patterns.
pub trait IntentRouter: Send + Sync {
    /// Route a capability request to an appropriate agent.
    ///
    /// # Arguments
    /// * `capability` - The capability being requested
    /// * `registry` - The agent registry to search
    ///
    /// # Returns
    /// A reference to the selected agent handle, or an error if no suitable agent exists.
    fn route<'a>(
        &self,
        capability: &Capability,
        registry: &'a AgentRegistry,
    ) -> Result<&'a AgentHandle, OrchestratorError>;
}

/// Policy for breaking ties when multiple agents have the same capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TieBreakPolicy {
    /// Sort candidates lexicographically by agent name and select the first.
    #[default]
    Lexicographic,
}

pub struct NameRouting;

impl RoutingStrategy for NameRouting {
    fn resolve<'a>(
        &self,
        agent_name: &str,
        registry: &'a AgentRegistry,
    ) -> Option<&'a AgentHandle> {
        registry.find(agent_name)
    }
}

/// Capability-based agent router with deterministic tie-breaking.
///
/// Routes requests to agents by matching capabilities from the registry.
/// When multiple agents share a capability, candidates are **sorted
/// lexicographically by name** before the tie-break policy is applied.
/// Sorting is required because `AgentRegistry::find_by_capability` iterates
/// over a `HashMap` whose order is non-deterministic.
pub struct CapabilityRouter {
    pub tie_break: TieBreakPolicy,
}

impl CapabilityRouter {
    /// Creates a new router with the given tie-break policy.
    pub fn new(tie_break: TieBreakPolicy) -> Self {
        Self { tie_break }
    }
}

impl IntentRouter for CapabilityRouter {
    fn route<'a>(
        &self,
        capability: &Capability,
        registry: &'a AgentRegistry,
    ) -> Result<&'a AgentHandle, OrchestratorError> {
        let mut candidates = registry.find_by_capability(capability);

        if candidates.is_empty() {
            return Err(OrchestratorError::AgentNotFound {
                name: format!("capability:{}", capability.0),
            });
        }

        // CRITICAL: sort before tie-break — HashMap iteration is non-deterministic.
        candidates.sort();

        let selected = match self.tie_break {
            TieBreakPolicy::Lexicographic => candidates[0],
        };

        info!(
            name: EVENT_ROUTE_RESOLVED,
            capability = %capability.0,
            selected_agent = selected,
            candidate_count = candidates.len(),
            tie_break = ?self.tie_break,
            reason = "capability_match",
        );

        registry
            .find(selected)
            .ok_or_else(|| OrchestratorError::AgentNotFound {
                name: selected.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{CapabilityRouter, IntentRouter, NameRouting, RoutingStrategy, TieBreakPolicy};
    use crate::orchestrator::AgentRegistry;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    use crate::agent::{AgentHandle, Capability, EchoAgent};

    #[tokio::test]
    async fn test_name_routing_resolves_registered_agent() {
        let cancellation_token = CancellationToken::new();
        let agent_handle =
            AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());

        let mut registry = AgentRegistry::new();
        let register_result = registry.register(
            "echo".to_string(),
            agent_handle,
            vec![Capability("echo".to_string())],
        );
        assert!(register_result.is_ok());

        let routing = NameRouting;
        let resolved = routing.resolve("echo", &registry);

        assert_eq!(resolved.map(AgentHandle::name), Some("echo"));

        cancellation_token.cancel();
    }

    #[tokio::test]
    async fn test_name_routing_returns_none_for_unknown() {
        let cancellation_token = CancellationToken::new();
        let mut registry = AgentRegistry::new();

        let agent_handle =
            AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());
        let register_result = registry.register(
            "echo".to_string(),
            agent_handle,
            vec![Capability("echo".to_string())],
        );
        assert!(register_result.is_ok());

        let routing = NameRouting;

        assert!(routing.resolve("unknown", &registry).is_none());

        cancellation_token.cancel();
    }

    #[test]
    fn test_intent_router_trait_is_object_safe() {
        fn _check_object_safe(_router: Box<dyn IntentRouter>) {}
    }

    #[test]
    fn test_tie_break_policy_default_is_lexicographic() {
        let policy = TieBreakPolicy::default();
        assert_eq!(policy, TieBreakPolicy::Lexicographic);
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_capability_router_lexicographic_tiebreak_selects_first_sorted() {
        let cancellation_token = CancellationToken::new();
        let mut registry = AgentRegistry::new();
        let cap = Capability("analyze".to_string());

        for name in ["charlie", "alpha", "bravo"] {
            let handle = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());
            registry
                .register(name.to_string(), handle, vec![cap.clone()])
                .expect("register");
        }

        let router = CapabilityRouter::new(TieBreakPolicy::Lexicographic);
        let result = router.route(&cap, &registry);

        assert!(result.is_ok());
        // All handles are EchoAgent so name() == "echo"; verify selection via tracing event
        assert!(logs_contain("selected_agent=\"alpha\""));

        cancellation_token.cancel();
    }

    #[tokio::test]
    async fn test_capability_router_no_match_returns_agent_not_found() {
        let cancellation_token = CancellationToken::new();
        let mut registry = AgentRegistry::new();

        let handle = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());
        registry
            .register(
                "agent1".to_string(),
                handle,
                vec![Capability("echo".to_string())],
            )
            .expect("register");

        let router = CapabilityRouter::new(TieBreakPolicy::Lexicographic);
        let unknown_cap = Capability("unknown".to_string());
        let result = router.route(&unknown_cap, &registry);

        match result {
            Err(crate::orchestrator::OrchestratorError::AgentNotFound { name }) => {
                assert!(
                    name.contains("unknown"),
                    "error should reference the capability"
                );
            }
            Ok(_) => panic!("expected AgentNotFound error"),
            Err(other) => panic!("expected AgentNotFound, got: {:?}", other),
        }

        cancellation_token.cancel();
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_capability_router_single_agent_returns_that_agent() {
        let cancellation_token = CancellationToken::new();
        let mut registry = AgentRegistry::new();
        let cap = Capability("process".to_string());

        let handle = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());
        registry
            .register("only_agent".to_string(), handle, vec![cap.clone()])
            .expect("register");

        let router = CapabilityRouter::new(TieBreakPolicy::Lexicographic);
        let result = router.route(&cap, &registry);

        assert!(result.is_ok());
        assert!(logs_contain("selected_agent=\"only_agent\""));
        assert!(logs_contain("candidate_count=1"));

        cancellation_token.cancel();
    }

    #[tracing_test::traced_test]
    #[tokio::test]
    async fn test_capability_router_emits_tracing_event() {
        let cancellation_token = CancellationToken::new();
        let mut registry = AgentRegistry::new();
        let cap = Capability("analyze".to_string());

        let handle = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());
        registry
            .register("analyzer".to_string(), handle, vec![cap.clone()])
            .expect("register");

        let router = CapabilityRouter::new(TieBreakPolicy::Lexicographic);
        let _result = router.route(&cap, &registry);

        assert!(logs_contain("selected_agent=\"analyzer\""));
        assert!(logs_contain("candidate_count=1"));
        assert!(logs_contain("reason=\"capability_match\""));

        cancellation_token.cancel();
    }
}
