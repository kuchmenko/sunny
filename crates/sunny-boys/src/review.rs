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

    fn build_prompt(content: &str) -> LlmRequest {
        LlmRequest {
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: "You are a senior software engineer. Analyze the provided code or content and deliver your findings directly. Be specific and reference actual code paths. Do not narrate intent or explain your process.".to_string(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: format!("Analyze this content:\n\n{content}"),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            max_tokens: Some(4096),
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

        let provider = self
            .provider
            .as_ref()
            .ok_or_else(|| AgentError::ExecutionFailed {
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "review provider is not configured",
                )),
            })?;

        let mut metadata = HashMap::new();
        let response = tokio::time::timeout(
            crate::timeouts::tool_provider_timeout(),
            provider.chat(Self::build_prompt(trimmed)),
        )
        .await
        .map_err(|_| AgentError::Timeout)?
        .map_err(|err| AgentError::ExecutionFailed {
            source: Box::new(err),
        })?;
        metadata.insert("mode".to_string(), "LLM_ENRICHED".to_string());
        let feedback = response.content.trim().to_string();

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
    use std::time::Duration;

    use sunny_core::agent::{
        Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability,
    };
    use sunny_mind::{
        LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage,
    };

    use super::ReviewAgent;

    struct MockProvider;

    struct HangingProvider;

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
                reasoning_content: None,
            })
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for HangingProvider {
        fn provider_id(&self) -> &str {
            "mock"
        }

        fn model_id(&self) -> &str {
            "mock-hanging-review"
        }

        async fn chat(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            tokio::time::sleep(Duration::from_secs(91)).await;
            Err(LlmError::InvalidResponse {
                message: "provider call should have timed out".to_string(),
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
        let err = agent
            .handle_message(mk_msg("fn main() {}"), &mk_ctx())
            .await
            .expect_err("should fail when provider is not configured");

        match err {
            AgentError::ExecutionFailed { source } => {
                assert_eq!(source.to_string(), "review provider is not configured");
            }
            AgentError::Timeout => panic!("expected execution error, got timeout"),
            AgentError::NotFound { id } => {
                panic!("expected execution error, got not found for id={id}")
            }
        }
    }

    #[tokio::test]
    async fn test_review_agent_without_provider_returns_error() {
        let agent = ReviewAgent::new(None);
        let err = agent
            .handle_message(mk_msg("let x = 1;"), &mk_ctx())
            .await
            .expect_err("missing provider should return an error");

        assert!(matches!(err, AgentError::ExecutionFailed { .. }));
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

    #[tokio::test(start_paused = true)]
    async fn test_review_agent_times_out_provider_call() {
        let agent = ReviewAgent::new(Some(Arc::new(HangingProvider)));
        let err = agent
            .handle_message(mk_msg("fn main() {}"), &mk_ctx())
            .await
            .expect_err("provider call should time out");

        assert!(matches!(err, AgentError::Timeout));
    }
}
