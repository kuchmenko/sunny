pub mod echo;
pub mod error;
pub mod handle;

pub use echo::EchoAgent;
pub use error::AgentError;
pub use handle::AgentHandle;

use std::collections::HashMap;

/// Capability represents a single capability an agent can perform.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Capability(pub String);

/// AgentContext provides minimal context for agent execution.
#[derive(Debug, Clone)]
pub struct AgentContext {
    pub agent_name: String,
}

/// AgentMessage represents a message sent to an agent.
#[derive(Debug, Clone)]
pub enum AgentMessage {
    Task {
        id: String,
        content: String,
        metadata: HashMap<String, String>,
    },
}

/// AgentResponse represents a response from an agent.
#[derive(Debug, Clone)]
pub enum AgentResponse {
    Success {
        content: String,
        metadata: HashMap<String, String>,
    },
    Error {
        code: String,
        message: String,
    },
}

/// Agent trait defines the interface for agents in the Sunny runtime.
#[async_trait::async_trait]
pub trait Agent: Send + Sync {
    /// Returns the name of this agent.
    fn name(&self) -> &str;

    /// Returns the list of capabilities this agent provides.
    fn capabilities(&self) -> Vec<Capability>;

    /// Handles an incoming message and returns a response.
    async fn handle_message(
        &self,
        msg: AgentMessage,
        ctx: &AgentContext,
    ) -> Result<AgentResponse, AgentError>;

    /// Called when the agent starts. Default implementation returns Ok(()).
    async fn on_start(&self, _ctx: &AgentContext) -> Result<(), AgentError> {
        Ok(())
    }

    /// Called when the agent stops. Default implementation returns Ok(()).
    async fn on_stop(&self) -> Result<(), AgentError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_trait_is_object_safe() {
        // This test verifies that Agent trait is object-safe by creating a trait object.
        // If this compiles, the trait is object-safe.
        fn _check_object_safe(_agent: Box<dyn Agent>) {}
    }
}
