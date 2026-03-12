use std::collections::HashMap;
use std::sync::Arc;

use sunny_core::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability};
use sunny_mind::{ChatMessage, ChatRole, LlmProvider, LlmRequest};

pub struct CritiqueAgent {
    provider: Option<Arc<dyn LlmProvider>>,
}

impl CritiqueAgent {
    pub fn new(provider: Option<Arc<dyn LlmProvider>>) -> Self {
        Self { provider }
    }

    fn build_prompt(content: &str) -> LlmRequest {
        LlmRequest {
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: "You are a senior technical reviewer. Analyze the provided content and deliver your critique directly. Be specific about risks, gaps, and improvements. Reference concrete details from the input.".to_string(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: format!("Critique this proposal and call out the main risks and gaps:\n\n{content}"),
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

        let provider = self
            .provider
            .as_ref()
            .ok_or_else(|| AgentError::ExecutionFailed {
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "critique provider is not configured",
                )),
            })?;

        tracing::info!(agent = %ctx.agent_name, content_len = trimmed.len(), "CritiqueAgent started");

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
    use std::sync::Arc;
    use std::time::Duration;

    use sunny_core::agent::{
        Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability,
    };
    use sunny_mind::{
        LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage,
    };

    use super::CritiqueAgent;

    struct MockProvider;

    struct HangingProvider;

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        fn provider_id(&self) -> &str {
            "mock"
        }

        fn model_id(&self) -> &str {
            "mock-critique"
        }

        async fn chat(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                content: "LLM critique feedback".to_string(),
                usage: TokenUsage {
                    input_tokens: 6,
                    output_tokens: 8,
                    total_tokens: 14,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-critique".to_string()),
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
            "mock-hanging-critique"
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
        let err = agent
            .handle_message(mk_msg("Deploy microservices to prod"), &mk_ctx())
            .await
            .expect_err("missing provider should return error");

        match err {
            AgentError::ExecutionFailed { source } => {
                assert_eq!(source.to_string(), "critique provider is not configured");
            }
            other => {
                panic!("expected execution failed error, got {other:?}");
            }
        }
    }

    #[tokio::test]
    async fn test_critique_agent_without_provider_returns_error() {
        let agent = CritiqueAgent::new(None);
        let err = agent
            .handle_message(mk_msg("Deploy microservices to prod"), &mk_ctx())
            .await
            .expect_err("missing provider should return error");

        assert!(matches!(err, AgentError::ExecutionFailed { .. }));
    }

    #[tokio::test]
    async fn test_critique_agent_empty_content_error() {
        let agent = CritiqueAgent::new(None);
        let result = agent.handle_message(mk_msg("   "), &mk_ctx()).await;
        assert!(result.is_err(), "empty content should return error");
    }

    #[tokio::test]
    async fn test_critique_agent_uses_provider_when_available() {
        let agent = CritiqueAgent::new(Some(Arc::new(MockProvider)));
        let response = agent
            .handle_message(mk_msg("Deploy microservices to prod"), &mk_ctx())
            .await
            .expect("should succeed");

        match response {
            AgentResponse::Success { content, metadata } => {
                assert_eq!(content, "LLM critique feedback");
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
    async fn test_critique_agent_times_out_provider_call() {
        let agent = CritiqueAgent::new(Some(Arc::new(HangingProvider)));
        let err = agent
            .handle_message(mk_msg("Deploy microservices to prod"), &mk_ctx())
            .await
            .expect_err("provider call should time out");

        assert!(matches!(err, AgentError::Timeout));
    }
}
