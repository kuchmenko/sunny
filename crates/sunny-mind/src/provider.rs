use crate::error::LlmError;
use crate::stream::{StreamEvent, StreamResult};
use crate::tokenizer::{CharHeuristicCounter, TokenCounter};
use crate::types::{LlmRequest, LlmResponse, Provider};

// ADR: chat_stream() addition (class L — public API change)
//
// Context: The REPL command (sunny chat) needs streaming responses to display text
// character-by-character as tokens arrive. The existing chat() method waits for the
// full response before returning, which prevents live streaming output.
//
// Decision: Added chat_stream() to LlmProvider with a default impl that calls chat()
// and wraps the result into a single-item stream. This preserves backward compatibility:
// existing providers work unchanged until they override
// chat_stream() with native streaming.
//
// Consequences: Breaking change — implementors of LlmProvider must recompile.
// No behavior change for providers that don't override chat_stream().
// AnthropicProvider will override chat_stream() with real SSE streaming.

/// LLM provider contract. Implementations must be `Send + Sync` for `Arc<dyn LlmProvider>`
/// and should support tool definitions/tool call responses when available.
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    fn provider(&self) -> Provider;
    fn model_id(&self) -> &str;
    async fn chat(&self, req: LlmRequest) -> Result<LlmResponse, LlmError>;

    /// Stream a chat response as a sequence of `StreamEvent`s.
    ///
    /// Default implementation calls `chat()` and wraps the response in a single-item stream
    /// emitting `ContentDelta` + `Usage` + `Done`. Providers with native streaming should
    /// override this method.
    async fn chat_stream(&self, req: LlmRequest) -> Result<StreamResult, LlmError> {
        let response = self.chat(req).await?;

        let content = response.content;
        let usage = response.usage;

        // Build a stream that emits: ContentDelta, Usage, Done
        let events: Vec<Result<StreamEvent, LlmError>> = vec![
            Ok(StreamEvent::ContentDelta { text: content }),
            Ok(StreamEvent::Usage { usage }),
            Ok(StreamEvent::Done),
        ];

        let stream = tokio_stream::iter(events);
        Ok(Box::pin(stream))
    }

    /// Get a token counter for this provider.
    ///
    /// Default implementation returns a character-based heuristic counter.
    /// Providers with exact tokenizers (e.g., OpenAI models) should override this method.
    fn token_counter(&self) -> std::sync::Arc<dyn TokenCounter> {
        std::sync::Arc::new(CharHeuristicCounter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_provider::MockToolCallProvider;
    use crate::types::{ChatMessage, ChatRole, ModelId, Provider, TokenUsage};

    #[tokio::test]
    async fn test_default_chat_stream_wraps_chat() {
        use tokio_stream::StreamExt;

        let response = LlmResponse {
            content: "hello world".to_string(),
            usage: TokenUsage {
                input_tokens: 5,
                output_tokens: 2,
                total_tokens: 7,
            },
            finish_reason: "stop".to_string(),
            provider: Provider::Anthropic,
            model_id: ModelId("mock-tool-call".to_string()),
            tool_calls: None,
            reasoning_content: None,
        };
        let provider = MockToolCallProvider::new(vec![response]);
        let req = LlmRequest {
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "hello".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
            thinking_budget: None,
        };

        let mut stream = provider
            .chat_stream(req)
            .await
            .expect("chat_stream should succeed");
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event.expect("stream item should be Ok"));
        }

        // Default impl should emit: ContentDelta, Usage, Done
        assert_eq!(events.len(), 3);
        assert!(matches!(&events[0], StreamEvent::ContentDelta { .. }));
        assert!(matches!(&events[1], StreamEvent::Usage { .. }));
        assert!(matches!(&events[2], StreamEvent::Done));
    }
}
