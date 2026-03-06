use std::collections::HashMap;
use std::sync::Arc;

use sunny_core::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability};
use sunny_mind::{ChatMessage, ChatRole, LlmProvider, LlmRequest};

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

    fn build_prompt(content: &str) -> LlmRequest {
        LlmRequest {
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: "Produce concise review feedback with correctness, style, and concrete suggestions. Do not invent facts beyond the provided input.".to_string(),
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: format!("Review this content and provide structured feedback:\n\n{content}"),
                },
            ],
            max_tokens: Some(600),
            temperature: Some(0.7),
            tools: None,
            tool_choice: None,
        }
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

        let mut metadata = HashMap::new();
        let feedback = if let Some(provider) = &self.provider {
            let response = provider
                .chat(Self::build_prompt(trimmed))
                .await
                .map_err(|err| AgentError::ExecutionFailed {
                    source: Box::new(err),
                })?;
            metadata.insert("mode".to_string(), "LLM_ENRICHED".to_string());
            response.content.trim().to_string()
        } else {
            metadata.insert("mode".to_string(), "TEMPLATE".to_string());
            Self::build_feedback_template(trimmed)
        };

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
    use std::sync::Arc;

    use sunny_core::agent::{Agent, AgentContext, AgentMessage, AgentResponse, Capability};
    use sunny_mind::{
        LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage,
    };

    use super::ReviewAgent;

    struct MockProvider;

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        fn provider_id(&self) -> &str {
            "mock"
        }

        fn model_id(&self) -> &str {
            "mock-review"
        }

        async fn chat(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                content: "LLM review feedback".to_string(),
                usage: TokenUsage {
                    input_tokens: 5,
                    output_tokens: 7,
                    total_tokens: 12,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-review".to_string()),
                tool_calls: None,
            })
        }
    }

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

    #[tokio::test]
    async fn test_review_agent_uses_provider_when_available() {
        let agent = ReviewAgent::new(Some(Arc::new(MockProvider)));
        let response = agent
            .handle_message(mk_msg("fn main() {}"), &mk_ctx())
            .await
            .expect("should succeed");

        match response {
            AgentResponse::Success { content, metadata } => {
                assert_eq!(content, "LLM review feedback");
                assert_eq!(
                    metadata.get("mode").map(String::as_str),
                    Some("LLM_ENRICHED")
                );
            }
            AgentResponse::Error { code, message } => {
                panic!("expected success, got error code={code}, message={message}");
            }
        }
    }
}
