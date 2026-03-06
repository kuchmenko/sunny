use std::collections::HashMap;
use std::sync::Arc;

use sunny_core::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability};
use sunny_mind::LlmProvider;

pub struct ReviewAgent {
    provider: Option<Arc<dyn LlmProvider>>,
}

impl ReviewAgent {
    pub fn new(provider: Option<Arc<dyn LlmProvider>>) -> Self {
        Self { provider }
    }

    fn build_feedback_template(content: &str) -> String {
        format!(
            "REVIEW FEEDBACK\n\
             ===============\n\
             Status: PENDING_REVIEW\n\
             Input length: {} chars\n\n\
             Sections:\n\
             - Correctness: [not yet analyzed]\n\
             - Style: [not yet analyzed]\n\
             - Suggestions: [not yet analyzed]\n\n\
             Raw input:\n\
             {content}",
            content.len()
        )
    }
}

#[async_trait::async_trait]
impl Agent for ReviewAgent {
    fn name(&self) -> &str {
        "review"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability("analyze".into())]
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
                    "review content cannot be empty",
                )),
            });
        }

        tracing::info!(agent = %ctx.agent_name, content_len = trimmed.len(), "ReviewAgent started");

        // LLM integration placeholder: when provider is available, send review prompt
        let _has_llm = self.provider.is_some();

        let feedback = Self::build_feedback_template(trimmed);

        let mut metadata = HashMap::new();
        metadata.insert("mode".to_string(), "TEMPLATE".to_string());

        tracing::info!(agent = %ctx.agent_name, "ReviewAgent completed");
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

    use super::ReviewAgent;

    fn mk_ctx() -> AgentContext {
        AgentContext {
            agent_name: "test-review".to_string(),
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
    fn test_review_agent_name_and_capabilities() {
        let agent = ReviewAgent::new(None);
        assert_eq!(agent.name(), "review");
        assert_eq!(
            agent.capabilities(),
            vec![Capability("analyze".to_string())]
        );
    }

    #[tokio::test]
    async fn test_review_agent_handles_task() {
        let agent = ReviewAgent::new(None);
        let response = agent
            .handle_message(mk_msg("fn main() {}"), &mk_ctx())
            .await
            .expect("should succeed");

        match response {
            AgentResponse::Success { content, metadata } => {
                assert!(content.contains("REVIEW FEEDBACK"));
                assert!(content.contains("fn main() {}"));
                assert_eq!(metadata.get("mode").map(String::as_str), Some("TEMPLATE"));
            }
            AgentResponse::Error { code, message } => {
                panic!("expected success, got error code={code}, message={message}");
            }
        }
    }

    #[tokio::test]
    async fn test_review_agent_empty_content_error() {
        let agent = ReviewAgent::new(None);
        let result = agent.handle_message(mk_msg("   "), &mk_ctx()).await;
        assert!(result.is_err(), "empty content should return error");
    }
}
