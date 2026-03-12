use std::sync::Arc;

use sunny_core::orchestrator::{
    CapabilityRegistry, IntakeAdvisor, IntakeAdvisorError, RawIntakeAdvice, WorkspaceContext,
};
use sunny_mind::{ChatMessage, ChatRole, LlmError, LlmProvider, LlmRequest};

#[allow(dead_code)]
pub const INTAKE_SYSTEM_PROMPT: &str = r#"You are Sunny intake routing advisor.
Return only valid JSON with this exact shape:
{
  "suggested_capability": "query|analyze|action|explore|advise|null",
  "complexity_hint": "low|medium|high|null",
  "context_tags": ["tag1", "tag2"],
  "reasoning": "short reason"
}

Rules:
- Select exactly one capability from: query, analyze, action, explore, advise.
- Use workspace_context to bias decisions toward local context gathering when the user asks broad,
  repository-scoped questions.
- Reserve advise for explicit strategy/validation needs; do not use it as default for ambiguous
  repository exploration requests.
- Use null when capability or complexity is unknown.
- Keep context_tags short, lower_snake_case preferred.
- Respond with JSON only. No markdown, no prose.
"#;

#[allow(dead_code)]
pub struct LlmIntakeAdvisor {
    provider: Arc<dyn LlmProvider>,
}

impl LlmIntakeAdvisor {
    #[allow(dead_code)]
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        Self { provider }
    }
}

#[allow(dead_code)]
#[derive(serde::Deserialize)]
struct RawIntakeAdvicePayload {
    suggested_capability: Option<String>,
    complexity_hint: Option<String>,
    #[serde(default)]
    context_tags: Vec<String>,
    reasoning: Option<String>,
}

#[async_trait::async_trait]
impl IntakeAdvisor for LlmIntakeAdvisor {
    async fn advise(
        &self,
        user_input: &str,
        workspace_context: &WorkspaceContext,
    ) -> Result<RawIntakeAdvice, IntakeAdvisorError> {
        let workspace_summary = workspace_context.summarize();
        let req = LlmRequest {
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: INTAKE_SYSTEM_PROMPT.to_string(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: format!(
                        "user_input:\n{}\n\nworkspace_context:\n{}",
                        user_input, workspace_summary
                    ),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            max_tokens: Some(256),
            temperature: Some(0.1),
            tools: None,
            tool_choice: None,
        };

        let response = self.provider.chat(req).await.map_err(map_llm_error)?;
        let parsed: RawIntakeAdvicePayload = serde_json::from_str(&response.content)
            .map_err(|err| IntakeAdvisorError::ParseFailed(err.to_string()))?;

        let suggested_capability = match parsed.suggested_capability {
            Some(capability) => {
                let normalized = capability.trim().to_ascii_lowercase();
                if normalized.is_empty() {
                    None
                } else if CapabilityRegistry::default().is_allowed(&normalized) {
                    Some(normalized)
                } else {
                    return Err(IntakeAdvisorError::InvalidCapability(capability));
                }
            }
            None => None,
        };

        Ok(RawIntakeAdvice {
            suggested_capability,
            complexity_hint: parsed.complexity_hint,
            context_tags: parsed.context_tags,
            reasoning: parsed.reasoning,
        })
    }
}

#[allow(dead_code)]
fn map_llm_error(err: LlmError) -> IntakeAdvisorError {
    match err {
        LlmError::Timeout { .. } => IntakeAdvisorError::Timeout,
        LlmError::Transport { source } => IntakeAdvisorError::ConnectionFailed(source.to_string()),
        LlmError::InvalidResponse { message } => IntakeAdvisorError::ParseFailed(message),
        other => IntakeAdvisorError::ConnectionFailed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use sunny_mind::{LlmResponse, ModelId, ProviderId, TokenUsage};

    struct MockProvider {
        response: Result<LlmResponse, LlmError>,
        last_request: Mutex<Option<LlmRequest>>,
    }

    fn clone_llm_error(err: &LlmError) -> LlmError {
        match err {
            LlmError::AuthFailed { message } => LlmError::AuthFailed {
                message: message.clone(),
            },
            LlmError::Timeout { timeout_ms } => LlmError::Timeout {
                timeout_ms: *timeout_ms,
            },
            LlmError::RateLimited => LlmError::RateLimited,
            LlmError::InvalidResponse { message } => LlmError::InvalidResponse {
                message: message.clone(),
            },
            LlmError::Transport { source } => LlmError::Transport {
                source: Box::new(std::io::Error::other(source.to_string())),
            },
            LlmError::NotConfigured { message } => LlmError::NotConfigured {
                message: message.clone(),
            },
            LlmError::UnsupportedAuthMode { mode } => {
                LlmError::UnsupportedAuthMode { mode: mode.clone() }
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        fn provider_id(&self) -> &str {
            "mock"
        }

        fn model_id(&self) -> &str {
            "mock-model"
        }

        async fn chat(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            self.last_request
                .lock()
                .expect("mock provider request lock poisoned")
                .replace(req);

            match &self.response {
                Ok(response) => Ok(response.clone()),
                Err(err) => Err(clone_llm_error(err)),
            }
        }
    }

    fn make_llm_response(content: &str) -> LlmResponse {
        LlmResponse {
            content: content.to_string(),
            usage: TokenUsage {
                input_tokens: 11,
                output_tokens: 9,
                total_tokens: 20,
            },
            finish_reason: "stop".to_string(),
            provider_id: ProviderId("mock".to_string()),
            model_id: ModelId("mock-model".to_string()),
            tool_calls: None,
            reasoning_content: None,
        }
    }

    #[tokio::test]
    async fn test_llm_intake_advisor_returns_advice_on_valid_response() {
        let provider = Arc::new(MockProvider {
            response: Ok(make_llm_response(
                r#"{"suggested_capability":"Query","complexity_hint":"medium","context_tags":["repo","routing"],"reasoning":"user asks for lookup"}"#,
            )),
            last_request: Mutex::new(None),
        });

        let advisor = LlmIntakeAdvisor::new(provider.clone() as Arc<dyn LlmProvider>);
        let advice = advisor
            .advise("where is intake trait", &WorkspaceContext::default())
            .await
            .expect("expected valid advice");

        assert_eq!(advice.suggested_capability.as_deref(), Some("query"));
        assert_eq!(advice.complexity_hint.as_deref(), Some("medium"));
        assert_eq!(
            advice.context_tags,
            vec!["repo".to_string(), "routing".to_string()]
        );

        let request = provider
            .last_request
            .lock()
            .expect("mock provider request lock poisoned")
            .clone()
            .expect("expected request to be captured");
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, ChatRole::System);
        assert_eq!(request.messages[1].role, ChatRole::User);
        assert!(request.messages[1]
            .content
            .contains("where is intake trait"));
        assert!(request.messages[1].content.contains("workspace_context"));
    }

    #[tokio::test]
    async fn test_llm_intake_advisor_returns_error_on_invalid_json() {
        let provider = Arc::new(MockProvider {
            response: Ok(make_llm_response("not json")),
            last_request: Mutex::new(None),
        });

        let advisor = LlmIntakeAdvisor::new(provider as Arc<dyn LlmProvider>);
        let error = advisor
            .advise("classify this", &WorkspaceContext::default())
            .await
            .expect_err("expected parse error");

        assert!(
            matches!(error, IntakeAdvisorError::ParseFailed(_)),
            "expected parse error, got: {error:?}"
        );
    }

    #[tokio::test]
    async fn test_llm_intake_advisor_returns_error_on_invalid_capability() {
        let provider = Arc::new(MockProvider {
            response: Ok(make_llm_response(
                r#"{"suggested_capability":"delegate","complexity_hint":"low","context_tags":[],"reasoning":null}"#,
            )),
            last_request: Mutex::new(None),
        });

        let advisor = LlmIntakeAdvisor::new(provider as Arc<dyn LlmProvider>);
        let error = advisor
            .advise("delegate work", &WorkspaceContext::default())
            .await
            .expect_err("expected invalid capability error");

        assert!(
            matches!(&error, IntakeAdvisorError::InvalidCapability(cap) if cap == "delegate"),
            "expected invalid capability error, got: {error:?}"
        );
    }

    #[tokio::test]
    async fn test_llm_intake_advisor_maps_timeout_error() {
        let provider = Arc::new(MockProvider {
            response: Err(LlmError::Timeout { timeout_ms: 30_000 }),
            last_request: Mutex::new(None),
        });

        let advisor = LlmIntakeAdvisor::new(provider as Arc<dyn LlmProvider>);
        let error = advisor
            .advise("what should run", &WorkspaceContext::default())
            .await
            .expect_err("expected timeout mapping");

        assert!(
            matches!(error, IntakeAdvisorError::Timeout),
            "expected timeout error, got: {error:?}"
        );
    }
}
