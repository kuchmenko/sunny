use std::collections::HashMap;
use std::sync::Arc;

use sunny_core::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability};
use sunny_mind::LlmProvider;

pub struct CritiqueAgent {
    provider: Option<Arc<dyn LlmProvider>>,
}

impl CritiqueAgent {
    pub fn new(provider: Option<Arc<dyn LlmProvider>>) -> Self {
        Self { provider }
    }

    fn build_critique_template(content: &str) -> String {
        format!(
            "CRITIQUE REPORT\n\
             ===============\n\
             Status: PENDING_CRITIQUE\n\
             Input length: {} chars\n\n\
             Sections:\n\
             - Feasibility: [not yet analyzed]\n\
             - Risks: [not yet analyzed]\n\
             - Gaps: [not yet analyzed]\n\
             - Improvements: [not yet analyzed]\n\n\
             Raw proposal:\n\
             {content}",
            content.len()
        )
    }
}

#[async_trait::async_trait]
impl Agent for CritiqueAgent {
    fn name(&self) -> &str {
        "critique"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability("action".into())]
    }

    async fn handle_message(
        &self,
        msg: AgentMessage,
        ctx: &AgentContext,
    ) -> Result<AgentResponse, AgentError> {
        let content = match msg {
            AgentMessage::Task { content, .. } => content,
        };

        let trimmed = content.trim();
        if trimmed.is_empty() {
            return Err(AgentError::ExecutionFailed {
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "critique content cannot be empty",
                )),
            });
        }

        tracing::info!(agent = %ctx.agent_name, content_len = trimmed.len(), "CritiqueAgent started");

        // LLM integration placeholder: when provider is available, send critique prompt
        let _has_llm = self.provider.is_some();

        let feedback = Self::build_critique_template(trimmed);

        let mut metadata = HashMap::new();
        metadata.insert("mode".to_string(), "TEMPLATE".to_string());

        tracing::info!(agent = %ctx.agent_name, "CritiqueAgent completed");
        Ok(AgentResponse::Success {
            content: feedback,
            metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use sunny_core::agent::{Agent, AgentContext, AgentMessage, AgentResponse, Capability};

    use super::CritiqueAgent;

    fn mk_ctx() -> AgentContext {
        AgentContext {
            agent_name: "test-critique".to_string(),
        }
    }

    fn mk_msg(content: &str) -> AgentMessage {
        AgentMessage::Task {
            id: "task-1".to_string(),
            content: content.to_string(),
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn test_critique_agent_name_and_capabilities() {
        let agent = CritiqueAgent::new(None);
        assert_eq!(agent.name(), "critique");
        assert_eq!(agent.capabilities(), vec![Capability("action".to_string())]);
    }

    #[tokio::test]
    async fn test_critique_agent_handles_task() {
        let agent = CritiqueAgent::new(None);
        let response = agent
            .handle_message(mk_msg("Deploy microservices to prod"), &mk_ctx())
            .await
            .expect("should succeed");

        match response {
            AgentResponse::Success { content, metadata } => {
                assert!(content.contains("CRITIQUE REPORT"));
                assert!(content.contains("Deploy microservices to prod"));
                assert_eq!(metadata.get("mode").map(String::as_str), Some("TEMPLATE"));
            }
            AgentResponse::Error { code, message } => {
                panic!("expected success, got error code={code}, message={message}");
            }
        }
    }

    #[tokio::test]
    async fn test_critique_agent_empty_content_error() {
        let agent = CritiqueAgent::new(None);
        let result = agent.handle_message(mk_msg("   "), &mk_ctx()).await;
        assert!(result.is_err(), "empty content should return error");
    }
}
