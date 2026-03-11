use std::sync::Arc;
use std::time::Duration;

use sunny_core::agent::{
    Agent, AgentContext, AgentCost, AgentError, AgentMessage, AgentMetadata, AgentMode,
    AgentResponse, Capability,
};
use sunny_mind::{ChatMessage, ChatRole, LlmProvider, LlmRequest};

const PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);

pub struct OracleAgent {
    provider: Option<Arc<dyn LlmProvider>>,
    metadata: AgentMetadata,
}

impl OracleAgent {
    pub fn new(provider: Option<Arc<dyn LlmProvider>>) -> Self {
        Self {
            provider,
            metadata: AgentMetadata {
                mode: AgentMode::Subagent,
                category: "advisor",
                cost: AgentCost::Expensive,
            },
        }
    }

    fn build_system_prompt() -> &'static str {
        "You are Oracle, a strategic advisor for high-stakes decisions.\
         Reason step-by-step internally, then provide a concise recommendation.\
         Explicitly cover trade-offs, key risks, and alternatives.\
         Prioritize actionable recommendations with clear rationale and assumptions."
    }

    fn build_prompt(content: &str) -> LlmRequest {
        LlmRequest {
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: Self::build_system_prompt().to_string(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: content.to_string(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            max_tokens: Some(32000),
            temperature: Some(0.7),
            tools: None,
            tool_choice: None,
        }
    }
}

#[async_trait::async_trait]
impl Agent for OracleAgent {
    fn name(&self) -> &str {
        "oracle"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability("advise".into())]
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
                    "oracle query cannot be empty",
                )),
            });
        }

        let provider = self
            .provider
            .as_ref()
            .ok_or_else(|| AgentError::ExecutionFailed {
                source: Box::new(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "oracle provider is not configured",
                )),
            })?;

        tracing::info!(agent = %ctx.agent_name, query_len = trimmed.len(), "OracleAgent started");

        let response =
            tokio::time::timeout(PROVIDER_TIMEOUT, provider.chat(Self::build_prompt(trimmed)))
                .await
                .map_err(|_| AgentError::Timeout)?
                .map_err(|err| AgentError::ExecutionFailed {
                    source: Box::new(err),
                })?;

        let mut metadata = std::collections::HashMap::new();
        metadata.insert("mode".to_string(), "SINGLE_SHOT".to_string());
        metadata.insert("agent_mode".to_string(), self.metadata.mode.to_string());
        metadata.insert("agent_cost".to_string(), self.metadata.cost.to_string());
        metadata.insert(
            "agent_category".to_string(),
            self.metadata.category.to_string(),
        );
        metadata.insert("provider_id".to_string(), response.provider_id.0.clone());
        metadata.insert("model_id".to_string(), response.model_id.0.clone());
        metadata.insert("finish_reason".to_string(), response.finish_reason.clone());
        metadata.insert(
            "total_tokens".to_string(),
            response.usage.total_tokens.to_string(),
        );
        if let Some(reasoning_content) = response.reasoning_content.clone() {
            metadata.insert("reasoning_content".to_string(), reasoning_content);
        }

        tracing::info!(agent = %ctx.agent_name, "OracleAgent completed");

        Ok(AgentResponse::Success {
            content: response.content.trim().to_string(),
            metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use sunny_core::agent::{
        Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability,
    };
    use sunny_mind::{
        LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage,
    };
    use tokio::sync::Mutex;

    use super::OracleAgent;

    struct MockProvider {
        response_content: &'static str,
        reasoning_content: Option<&'static str>,
        seen_request: Arc<Mutex<Option<LlmRequest>>>,
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        fn provider_id(&self) -> &str {
            "mock"
        }

        fn model_id(&self) -> &str {
            "mock-oracle"
        }

        async fn chat(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            *self.seen_request.lock().await = Some(req);
            Ok(LlmResponse {
                content: self.response_content.to_string(),
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 20,
                    total_tokens: 30,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-oracle".to_string()),
                tool_calls: None,
                reasoning_content: self.reasoning_content.map(str::to_string),
            })
        }
    }

    fn mk_ctx() -> AgentContext {
        AgentContext {
            agent_name: "test-oracle".to_string(),
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
    fn test_oracle_agent_name_and_capabilities() {
        let agent = OracleAgent::new(None);
        assert_eq!(agent.name(), "oracle");
        assert_eq!(agent.capabilities(), vec![Capability("advise".to_string())]);
    }

    #[tokio::test]
    async fn test_oracle_agent_single_shot_with_mock_provider() {
        let seen_request = Arc::new(Mutex::new(None));
        let provider = MockProvider {
            response_content: "Strategic recommendation",
            reasoning_content: None,
            seen_request: Arc::clone(&seen_request),
        };
        let agent = OracleAgent::new(Some(Arc::new(provider)));

        let response = agent
            .handle_message(
                mk_msg("How should we sequence roadmap initiatives?"),
                &mk_ctx(),
            )
            .await
            .expect("oracle should return success with provider");

        let request = seen_request.lock().await.clone();
        let request = request.expect("provider should receive exactly one request");
        assert!(request.tools.is_none());
        assert!(request.tool_choice.is_none());
        assert_eq!(request.temperature, Some(0.7));
        assert_eq!(request.max_tokens, Some(32000));

        match response {
            AgentResponse::Success { content, .. } => {
                assert_eq!(content, "Strategic recommendation");
            }
            AgentResponse::Error { code, message } => {
                panic!("expected success, got error code={code}, message={message}");
            }
        }
    }

    #[tokio::test]
    async fn test_oracle_agent_without_provider_returns_error() {
        let agent = OracleAgent::new(None);
        let err = agent
            .handle_message(mk_msg("Advise on pricing strategy"), &mk_ctx())
            .await
            .expect_err("missing provider should return AgentError");

        assert!(matches!(err, AgentError::ExecutionFailed { .. }));
    }

    #[tokio::test]
    async fn test_oracle_agent_preserves_reasoning_content() {
        let seen_request = Arc::new(Mutex::new(None));
        let provider = MockProvider {
            response_content: "Recommendation with rationale",
            reasoning_content: Some("private reasoning trace"),
            seen_request,
        };
        let agent = OracleAgent::new(Some(Arc::new(provider)));

        let response = agent
            .handle_message(mk_msg("Advise on market entry"), &mk_ctx())
            .await
            .expect("oracle should return success with provider");

        match response {
            AgentResponse::Success { metadata, .. } => {
                assert_eq!(
                    metadata.get("reasoning_content").map(String::as_str),
                    Some("private reasoning trace")
                );
            }
            AgentResponse::Error { code, message } => {
                panic!("expected success, got error code={code}, message={message}");
            }
        }
    }

    #[test]
    fn test_oracle_agent_system_prompt_structure() {
        let prompt = OracleAgent::build_system_prompt();
        assert!(prompt.contains("strategic advisor"));
        assert!(prompt.contains("trade-offs"));
        assert!(prompt.contains("recommendations"));
    }
}
