use super::{Agent, AgentContext, AgentMessage, AgentResponse, Capability};
use crate::agent::AgentError;

/// EchoAgent is the simplest Agent implementation for testing.
/// It echoes back the input content exactly as-is.
#[derive(Debug, Clone)]
pub struct EchoAgent;

#[async_trait::async_trait]
impl Agent for EchoAgent {
    fn name(&self) -> &str {
        "echo"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability("echo".to_string())]
    }

    async fn handle_message(
        &self,
        msg: AgentMessage,
        _ctx: &AgentContext,
    ) -> Result<AgentResponse, AgentError> {
        match msg {
            AgentMessage::Task {
                id: _,
                content,
                metadata,
            } => Ok(AgentResponse::Success { content, metadata }),
        }
    }

    async fn on_start(&self, ctx: &AgentContext) -> Result<(), AgentError> {
        tracing::info!(agent = %ctx.agent_name, "EchoAgent starting");
        Ok(())
    }

    async fn on_stop(&self) -> Result<(), AgentError> {
        tracing::info!("EchoAgent stopping");
        Ok(())
    }
}

impl Default for EchoAgent {
    fn default() -> Self {
        EchoAgent
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[tokio::test]
    async fn test_echo_agent_returns_input_as_output() {
        let agent = EchoAgent;
        let ctx = AgentContext {
            agent_name: "test_echo".to_string(),
        };

        let mut metadata = HashMap::new();
        metadata.insert("key".to_string(), "value".to_string());

        let msg = AgentMessage::Task {
            id: "task_1".to_string(),
            content: "hello world".to_string(),
            metadata: metadata.clone(),
        };

        let response = agent.handle_message(msg, &ctx).await.unwrap();

        match response {
            AgentResponse::Success {
                content,
                metadata: resp_metadata,
            } => {
                assert_eq!(content, "hello world");
                assert_eq!(resp_metadata, metadata);
            }
            _ => panic!("Expected Success response"),
        }
    }
}
