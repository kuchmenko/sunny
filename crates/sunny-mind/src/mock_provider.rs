use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::error::LlmError;
use crate::provider::LlmProvider;
use crate::types::{LlmRequest, LlmResponse};

/// Mock LLM provider for testing tool_call loops.
///
/// Returns pre-configured responses in FIFO order. Thread-safe via `tokio::sync::Mutex`.
pub struct MockToolCallProvider {
    responses: Arc<Mutex<VecDeque<LlmResponse>>>,
}

impl MockToolCallProvider {
    pub fn new(responses: Vec<LlmResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into())),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for MockToolCallProvider {
    fn provider_id(&self) -> &str {
        "mock"
    }

    fn model_id(&self) -> &str {
        "mock-tool-call"
    }

    async fn chat(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let mut queue = self.responses.lock().await;
        queue.pop_front().ok_or_else(|| LlmError::InvalidResponse {
            message: "mock provider: no more responses".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ModelId, ProviderId, TokenUsage, ToolCall};

    fn make_response(content: &str, finish_reason: &str) -> LlmResponse {
        LlmResponse {
            content: content.to_string(),
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            finish_reason: finish_reason.to_string(),
            provider_id: ProviderId("mock".to_string()),
            model_id: ModelId("mock-tool-call".to_string()),
            tool_calls: None,
            reasoning_content: None,
        }
    }

    fn make_request() -> LlmRequest {
        LlmRequest {
            messages: vec![],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
            thinking_budget: None,
        }
    }

    #[tokio::test]
    async fn test_mock_returns_responses_in_order() {
        let responses = vec![
            make_response("first", "stop"),
            make_response("second", "stop"),
            make_response("third", "stop"),
        ];
        let provider = MockToolCallProvider::new(responses);

        let r1 = provider.chat(make_request()).await.expect("first response");
        assert_eq!(r1.content, "first");

        let r2 = provider
            .chat(make_request())
            .await
            .expect("second response");
        assert_eq!(r2.content, "second");

        let r3 = provider.chat(make_request()).await.expect("third response");
        assert_eq!(r3.content, "third");
    }

    #[tokio::test]
    async fn test_mock_returns_tool_calls() {
        let tool_calls = vec![ToolCall {
            id: "call_1".to_string(),
            name: "search_web".to_string(),
            arguments: r#"{"query":"test"}"#.to_string(),
            execution_depth: 0,
        }];
        let mut response = make_response("", "tool_calls");
        response.tool_calls = Some(tool_calls.clone());

        let provider = MockToolCallProvider::new(vec![response]);
        let res = provider
            .chat(make_request())
            .await
            .expect("tool_call response");

        assert_eq!(res.finish_reason, "tool_calls");
        assert!(res.content.is_empty());
        let calls = res.tool_calls.expect("should have tool_calls");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search_web");
    }

    #[tokio::test]
    async fn test_mock_exhausted_returns_error() {
        let provider = MockToolCallProvider::new(vec![]);
        let err = provider.chat(make_request()).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("no more responses"),
            "expected 'no more responses' in error, got: {msg}"
        );
    }
}
