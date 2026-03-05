use std::collections::HashMap;

use crate::agent::{AgentHandle, Capability};

use super::{RegistryError, RestartPolicy};

/// AgentEntry stores an agent handle, its capabilities, and restart policy.
pub(crate) struct AgentEntry {
    pub(crate) handle: AgentHandle,
    pub(crate) capabilities: Vec<Capability>,
    #[allow(dead_code)]
    pub(crate) restart_policy: RestartPolicy,
}

/// AgentRegistry stores registered agents and provides lookup by name.
pub struct AgentRegistry {
    agents: HashMap<String, AgentEntry>,
}

impl AgentRegistry {
    /// Creates a new empty registry.
    pub fn new() -> Self {
        Self {
            agents: HashMap::new(),
        }
    }

    /// Registers an agent with the given name and capabilities.
    /// Uses RestartPolicy::Never by default.
    /// Returns an error if an agent with the same name already exists.
    pub fn register(
        &mut self,
        name: String,
        handle: AgentHandle,
        capabilities: Vec<Capability>,
    ) -> Result<(), RegistryError> {
        self.register_with_policy(name, handle, capabilities, RestartPolicy::Never)
    }

    /// Registers an agent with the given name, capabilities, and restart policy.
    /// Returns an error if an agent with the same name already exists.
    pub fn register_with_policy(
        &mut self,
        name: String,
        handle: AgentHandle,
        capabilities: Vec<Capability>,
        restart_policy: RestartPolicy,
    ) -> Result<(), RegistryError> {
        if self.agents.contains_key(&name) {
            return Err(RegistryError::DuplicateName { name });
        }

        self.agents.insert(
            name,
            AgentEntry {
                handle,
                capabilities,
                restart_policy,
            },
        );

        Ok(())
    }

    /// Finds an agent by name and returns a reference to its handle.
    pub fn find(&self, name: &str) -> Option<&AgentHandle> {
        self.agents.get(name).map(|entry| &entry.handle)
    }

    /// Lists all registered agents with their names and capabilities.
    pub fn list(&self) -> Vec<(&str, &[Capability])> {
        self.agents
            .iter()
            .map(|(name, entry)| (name.as_str(), entry.capabilities.as_slice()))
            .collect()
    }

    /// Finds all agents that have the given capability.
    /// Returns agent names (not handles — handles have lifetime issues with iteration).
    pub fn find_by_capability(&self, capability: &Capability) -> Vec<&str> {
        self.agents
            .iter()
            .filter(|(_, entry)| entry.capabilities.contains(capability))
            .map(|(name, _)| name.as_str())
            .collect()
    }

    /// Returns all unique capabilities across all registered agents.
    pub fn all_capabilities(&self) -> Vec<&Capability> {
        use std::collections::HashSet;
        let mut seen: HashSet<&Capability> = HashSet::new();
        self.agents
            .values()
            .flat_map(|entry| entry.capabilities.iter())
            .filter(|cap| seen.insert(cap))
            .collect()
    }
}

impl Default for AgentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio_util::sync::CancellationToken;

    use crate::agent::EchoAgent;

    use super::*;

    #[tokio::test]
    async fn test_registry_register_and_find() {
        let mut registry = AgentRegistry::new();
        let cancellation_token = CancellationToken::new();
        let handle = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());

        // Register an agent
        let result = registry.register("echo".into(), handle, vec![Capability("echo".into())]);
        assert!(result.is_ok());

        // Find the agent
        let found = registry.find("echo");
        assert_eq!(found.map(|entry| entry.name()), Some("echo"));

        // Find non-existent agent
        let not_found = registry.find("nonexistent");
        assert!(not_found.is_none());

        cancellation_token.cancel();
    }

    #[tokio::test]
    async fn test_register_with_policy_stores_policy() {
        let mut registry = AgentRegistry::new();
        let cancellation_token = CancellationToken::new();
        let handle = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());

        let policy = RestartPolicy::OnFailure { max_retries: 3 };
        let result = registry.register_with_policy(
            "echo".into(),
            handle,
            vec![Capability("echo".into())],
            policy,
        );
        assert!(result.is_ok());

        // Verify the policy was stored
        let entry = registry.agents.get("echo").expect("agent should exist");
        assert_eq!(entry.restart_policy, policy);

        cancellation_token.cancel();
    }

    #[tokio::test]
    async fn test_register_default_policy_is_never() {
        let mut registry = AgentRegistry::new();
        let cancellation_token = CancellationToken::new();
        let handle = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());

        let result = registry.register("echo".into(), handle, vec![Capability("echo".into())]);
        assert!(result.is_ok());

        // Verify default policy is Never
        let entry = registry.agents.get("echo").expect("agent should exist");
        assert_eq!(entry.restart_policy, RestartPolicy::Never);

        cancellation_token.cancel();
    }

    #[tokio::test]
    async fn test_registry_duplicate_name_returns_error() {
        let mut registry = AgentRegistry::new();
        let cancellation_token = CancellationToken::new();
        let handle_1 = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());
        let handle_2 = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());

        // Register first agent
        let result1 = registry.register("echo".into(), handle_1, vec![Capability("echo".into())]);
        assert!(result1.is_ok());

        // Try to register agent with same name
        let result2 = registry.register("echo".into(), handle_2, vec![Capability("echo".into())]);
        assert!(result2.is_err());

        // Verify error is DuplicateName
        match result2 {
            Err(RegistryError::DuplicateName { name }) => {
                assert_eq!(name, "echo");
            }
            _ => panic!("Expected DuplicateName error"),
        }

        cancellation_token.cancel();
    }

    #[tokio::test]
    async fn test_capability_index_finds_agents_with_capability() {
        let mut registry = AgentRegistry::new();
        let cancellation_token = CancellationToken::new();

        let handle_1 = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());
        let handle_2 = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());
        let handle_3 = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());

        let echo_cap = Capability("echo".into());
        let process_cap = Capability("process".into());

        registry
            .register(
                "agent1".into(),
                handle_1,
                vec![echo_cap.clone(), process_cap.clone()],
            )
            .expect("register agent1");

        registry
            .register("agent2".into(), handle_2, vec![echo_cap.clone()])
            .expect("register agent2");

        registry
            .register("agent3".into(), handle_3, vec![process_cap.clone()])
            .expect("register agent3");

        let echo_agents = registry.find_by_capability(&echo_cap);
        assert_eq!(echo_agents.len(), 2);
        assert!(echo_agents.contains(&"agent1"));
        assert!(echo_agents.contains(&"agent2"));

        let process_agents = registry.find_by_capability(&process_cap);
        assert_eq!(process_agents.len(), 2);
        assert!(process_agents.contains(&"agent1"));
        assert!(process_agents.contains(&"agent3"));

        cancellation_token.cancel();
    }

    #[tokio::test]
    async fn test_capability_index_returns_empty_for_unknown() {
        let mut registry = AgentRegistry::new();
        let cancellation_token = CancellationToken::new();

        let handle = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());

        registry
            .register("agent1".into(), handle, vec![Capability("echo".into())])
            .expect("register agent1");

        let unknown_cap = Capability("unknown".into());
        let result = registry.find_by_capability(&unknown_cap);
        assert!(result.is_empty());

        cancellation_token.cancel();
    }

    #[tokio::test]
    async fn test_all_capabilities_lists_unique_caps() {
        let mut registry = AgentRegistry::new();
        let cancellation_token = CancellationToken::new();

        let handle_1 = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());
        let handle_2 = AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());

        let echo_cap = Capability("echo".into());
        let process_cap = Capability("process".into());

        registry
            .register(
                "agent1".into(),
                handle_1,
                vec![echo_cap.clone(), process_cap.clone()],
            )
            .expect("register agent1");

        registry
            .register(
                "agent2".into(),
                handle_2,
                vec![echo_cap.clone(), Capability("analyze".into())],
            )
            .expect("register agent2");

        let all_caps = registry.all_capabilities();
        assert_eq!(all_caps.len(), 3);
        assert!(all_caps.contains(&&echo_cap));
        assert!(all_caps.contains(&&process_cap));
        assert!(all_caps.contains(&&Capability("analyze".into())));

        cancellation_token.cancel();
    }
}
