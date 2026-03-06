use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sunny_core::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability};
use sunny_mind::{ChatMessage, ChatRole, LlmProvider, LlmRequest};

pub struct CritiqueAgent {
    provider: Option<Arc<dyn LlmProvider>>,
}

const PROVIDER_TIMEOUT: Duration = Duration::from_secs(30);

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

    fn build_prompt(content: &str) -> LlmRequest {
        LlmRequest {
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: "Produce a concise critique covering feasibility, risks, gaps, and improvements. Stay grounded in the provided input.".to_string(),
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: format!("Critique this proposal and call out the main risks and gaps:\n\n{content}"),
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

        let mut metadata = HashMap::new();
        let feedback = if let Some(provider) = &self.provider {
            let response =
                tokio::time::timeout(PROVIDER_TIMEOUT, provider.chat(Self::build_prompt(trimmed)))
                    .await
                    .map_err(|_| AgentError::Timeout)?
                    .map_err(|err| AgentError::ExecutionFailed {
                        source: Box::new(err),
                    })?;
            metadata.insert("mode".to_string(), "LLM_ENRICHED".to_string());
            response.content.trim().to_string()
        } else {
            metadata.insert("mode".to_string(), "TEMPLATE".to_string());
            Self::build_critique_template(trimmed)
        };

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
            tokio::time::sleep(Duration::from_secs(31)).await;
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
