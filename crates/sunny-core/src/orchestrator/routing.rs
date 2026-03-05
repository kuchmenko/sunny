use crate::agent::AgentHandle;

use super::AgentRegistry;

pub trait RoutingStrategy: Send + Sync {
    fn resolve<'a>(&self, agent_name: &str, registry: &'a AgentRegistry)
        -> Option<&'a AgentHandle>;
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

#[cfg(test)]
mod tests {
    use super::{NameRouting, RoutingStrategy};
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
}
