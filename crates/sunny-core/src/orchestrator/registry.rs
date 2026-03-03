use std::collections::HashMap;

use crate::agent::{AgentHandle, Capability};

use super::RegistryError;

/// AgentEntry stores an agent handle and its capabilities.
pub(crate) struct AgentEntry {
    pub(crate) handle: AgentHandle,
    pub(crate) capabilities: Vec<Capability>,
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
    /// Returns an error if an agent with the same name already exists.
    pub fn register(
        &mut self,
        name: String,
        handle: AgentHandle,
        capabilities: Vec<Capability>,
    ) -> Result<(), RegistryError> {
        if self.agents.contains_key(&name) {
            return Err(RegistryError::DuplicateName { name });
        }

        self.agents.insert(
            name,
            AgentEntry {
                handle,
                capabilities,
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
        let handle = AgentHandle::spawn(
            Arc::new(EchoAgent),
            cancellation_token.child_token(),
        );

        // Register an agent
        let result = registry.register(
            "echo".into(),
            handle,
            vec![Capability("echo".into())],
        );
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
    async fn test_registry_duplicate_name_returns_error() {
        let mut registry = AgentRegistry::new();
        let cancellation_token = CancellationToken::new();
        let handle_1 = AgentHandle::spawn(
            Arc::new(EchoAgent),
            cancellation_token.child_token(),
        );
        let handle_2 = AgentHandle::spawn(
            Arc::new(EchoAgent),
            cancellation_token.child_token(),
        );

        // Register first agent
        let result1 = registry.register(
            "echo".into(),
            handle_1,
            vec![Capability("echo".into())],
        );
        assert!(result1.is_ok());

        // Try to register agent with same name
        let result2 = registry.register(
            "echo".into(),
            handle_2,
            vec![Capability("echo".into())],
        );
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
}
