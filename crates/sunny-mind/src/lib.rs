//! sunny-mind: LLM Provider abstraction for Sunny runtime
//!
//! # ADR: sunny-mind crate for LLM abstraction
//!
//! **Context**: LLM provider integration needed isolation from runtime
//! (sunny-core) and CLI concerns. Provider-agnostic trait enables
//! swapping providers without touching agent or CLI code.
//!
//! **Decision**: Created sunny-mind with LlmProvider trait + typed errors.
//! Kimi is first implementation. No dependency on sunny-core.
//!
//! **Consequences**: Adding new providers (Anthropic, OpenAI) only requires
//! new struct implementing LlmProvider. No changes to agents or CLI.

pub mod error;
pub mod kimi;
pub mod mock_provider;
pub mod provider;
pub mod types;

pub use error::LlmError;
pub use kimi::KimiProvider;
pub use mock_provider::MockToolCallProvider;
pub use provider::LlmProvider;
pub use types::{
    ChatMessage, ChatRole, LlmRequest, LlmResponse, ModelId, ProviderEconomics, ProviderId,
    ProviderRoutingPolicy, TokenUsage, ToolCall, ToolCallResult, ToolChoice, ToolDefinition,
};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn test_chat_message_construction() {
        let msg = ChatMessage {
            role: ChatRole::User,
            content: "Hello, world!".to_string(),
        };
        assert_eq!(msg.role, ChatRole::User);
        assert_eq!(msg.content, "Hello, world!");

        let system_msg = ChatMessage {
            role: ChatRole::System,
            content: "You are helpful.".to_string(),
        };
        assert_eq!(system_msg.role, ChatRole::System);

        let assistant_msg = ChatMessage {
            role: ChatRole::Assistant,
            content: "Sure!".to_string(),
        };
        assert_eq!(assistant_msg.role, ChatRole::Assistant);
    }

    #[test]
    fn test_llm_request_defaults() {
        let req = LlmRequest {
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "test".to_string(),
            }],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };
        assert_eq!(req.messages.len(), 1);
        assert!(req.max_tokens.is_none());
        assert!(req.temperature.is_none());

        let req_with_opts = LlmRequest {
            messages: vec![],
            max_tokens: Some(1024),
            temperature: Some(0.7),
            tools: None,
            tool_choice: None,
        };
        assert_eq!(req_with_opts.max_tokens, Some(1024));
        assert_eq!(req_with_opts.temperature, Some(0.7));
    }

    #[test]
    fn test_llm_response_fields() {
        let res = LlmResponse {
            content: "Generated text".to_string(),
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30,
            },
            finish_reason: "stop".to_string(),
            provider_id: ProviderId("test-provider".to_string()),
            model_id: ModelId("test-model".to_string()),
            tool_calls: None,
        };
        assert_eq!(res.content, "Generated text");
        assert_eq!(res.finish_reason, "stop");
        assert_eq!(res.provider_id, ProviderId("test-provider".to_string()));
        assert_eq!(res.model_id, ModelId("test-model".to_string()));
        assert_eq!(res.usage.input_tokens, 10);
        assert_eq!(res.usage.output_tokens, 20);
    }

    #[test]
    fn test_token_usage_total() {
        let usage = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
        };
        assert_eq!(usage.total(), 150);
        assert_eq!(usage.total(), usage.input_tokens + usage.output_tokens);

        let zero_usage = TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        };
        assert_eq!(zero_usage.total(), 0);
    }

    #[test]
    fn test_llm_error_display() {
        let auth_err = LlmError::AuthFailed {
            message: "invalid key".to_string(),
        };
        assert_eq!(auth_err.to_string(), "authentication failed: invalid key");

        let timeout_err = LlmError::Timeout { timeout_ms: 5000 };
        assert_eq!(timeout_err.to_string(), "request timed out after 5000ms");

        let rate_err = LlmError::RateLimited;
        assert_eq!(rate_err.to_string(), "rate limited by provider");

        let invalid_err = LlmError::InvalidResponse {
            message: "malformed JSON".to_string(),
        };
        assert_eq!(
            invalid_err.to_string(),
            "invalid response from provider: malformed JSON"
        );

        let transport_err = LlmError::Transport {
            source: Box::new(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "refused",
            )),
        };
        let rendered = transport_err.to_string();
        assert!(
            rendered.starts_with("transport error:"),
            "transport error should include prefix, got: {rendered}"
        );
        assert!(
            rendered.contains("refused"),
            "transport error should include source details, got: {rendered}"
        );

        let not_configured_err = LlmError::NotConfigured {
            message: "missing API key".to_string(),
        };
        assert_eq!(
            not_configured_err.to_string(),
            "provider not configured: missing API key"
        );
    }

    #[test]
    fn test_provider_object_safety() {
        // Verifies LlmProvider is object-safe by compiling Arc<dyn LlmProvider>.
        // If this compiles, the trait is object-safe.
        fn _check_object_safe(_provider: Arc<dyn LlmProvider>) {}
    }

    #[test]
    fn test_serde_roundtrip_chat_message() {
        let msg = ChatMessage {
            role: ChatRole::User,
            content: "Hello!".to_string(),
        };
        let json = serde_json::to_string(&msg).expect("serialize ChatMessage");
        let deserialized: ChatMessage =
            serde_json::from_str(&json).expect("deserialize ChatMessage");
        assert_eq!(msg, deserialized);

        for role in [ChatRole::System, ChatRole::User, ChatRole::Assistant] {
            let m = ChatMessage {
                role: role.clone(),
                content: "test".to_string(),
            };
            let j = serde_json::to_string(&m).expect("serialize");
            let d: ChatMessage = serde_json::from_str(&j).expect("deserialize");
            assert_eq!(m, d);
        }

        let req = LlmRequest {
            messages: vec![msg.clone()],
            max_tokens: Some(512),
            temperature: Some(0.5),
            tools: None,
            tool_choice: None,
        };
        let req_json = serde_json::to_string(&req).expect("serialize LlmRequest");
        let req_de: LlmRequest = serde_json::from_str(&req_json).expect("deserialize LlmRequest");
        assert_eq!(req, req_de);
    }

    #[test]
    fn test_llm_request_with_tools() {
        let request = LlmRequest {
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "Use a tool".to_string(),
            }],
            max_tokens: Some(128),
            temperature: Some(0.2),
            tools: Some(vec![ToolDefinition {
                name: "search_web".to_string(),
                description: "Searches the web".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" }
                    },
                    "required": ["query"]
                }),
            }]),
            tool_choice: Some(ToolChoice::Required),
        };

        let actual = serde_json::to_value(&request).expect("serialize request with tools");

        // Compare temperature separately to avoid f32→f64 precision mismatch
        let temp_val = actual.get("temperature").expect("temperature present");
        let temp_f = temp_val.as_f64().expect("temperature is number");
        assert!((temp_f - 0.2).abs() < 1e-6, "temperature should be ~0.2");

        let expected_rest = serde_json::json!({
            "messages": [{"role": "user", "content": "Use a tool"}],
            "max_tokens": 128,
            "tools": [{
                "name": "search_web",
                "description": "Searches the web",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string"}
                    },
                    "required": ["query"]
                }
            }],
            "tool_choice": "required"
        });
        assert_eq!(actual.get("messages"), expected_rest.get("messages"));
        assert_eq!(actual.get("max_tokens"), expected_rest.get("max_tokens"));
        assert_eq!(actual.get("tools"), expected_rest.get("tools"));
        assert_eq!(actual.get("tool_choice"), expected_rest.get("tool_choice"));

        let backward_compatible_request = LlmRequest {
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "test".to_string(),
            }],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };

        let backward_actual =
            serde_json::to_value(&backward_compatible_request).expect("serialize default request");
        // With skip_serializing_if, None fields are omitted for compactness
        let backward_expected = serde_json::json!({
            "messages": [{"role": "user", "content": "test"}]
        });
        assert_eq!(backward_actual, backward_expected);
    }

    #[test]
    fn test_llm_response_with_tool_calls() {
        let response = LlmResponse {
            content: "".to_string(),
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 8,
                total_tokens: 18,
            },
            finish_reason: "tool_calls".to_string(),
            provider_id: ProviderId("test-provider".to_string()),
            model_id: ModelId("test-model".to_string()),
            tool_calls: Some(vec![ToolCall {
                id: "call_123".to_string(),
                name: "search_web".to_string(),
                arguments: "{\"query\":\"sunny\"}".to_string(),
                execution_depth: 0,
            }]),
        };

        let actual = serde_json::to_value(&response).expect("serialize response with tool calls");
        let expected = serde_json::json!({
            "content": "",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 8,
                "total_tokens": 18
            },
            "finish_reason": "tool_calls",
            "provider_id": "test-provider",
            "model_id": "test-model",
            "tool_calls": [{
                "id": "call_123",
                "name": "search_web",
                "arguments": "{\"query\":\"sunny\"}",
                "execution_depth": 0
            }]
        });
        assert_eq!(actual, expected);
    }

    #[test]
    fn test_tool_definition_serde() {
        let definition = ToolDefinition {
            name: "search_web".to_string(),
            description: "Searches the web".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                },
                "required": ["query"]
            }),
        };

        let json = serde_json::to_string(&definition).expect("serialize ToolDefinition");
        let decoded: ToolDefinition =
            serde_json::from_str(&json).expect("deserialize ToolDefinition");
        assert_eq!(definition, decoded);
    }

    #[test]
    fn test_tool_call_serde() {
        let call = ToolCall {
            id: "call_123".to_string(),
            name: "search_web".to_string(),
            arguments: "{\"query\":\"sunny rust\"}".to_string(),
            execution_depth: 0,
        };

        let json = serde_json::to_string(&call).expect("serialize ToolCall");
        let decoded: ToolCall = serde_json::from_str(&json).expect("deserialize ToolCall");
        assert_eq!(call, decoded);
    }

    #[test]
    fn test_tool_choice_variants() {
        assert_eq!(
            serde_json::to_string(&ToolChoice::Auto).expect("serialize auto"),
            "\"auto\""
        );
        assert_eq!(
            serde_json::to_string(&ToolChoice::None).expect("serialize none"),
            "\"none\""
        );
        assert_eq!(
            serde_json::to_string(&ToolChoice::Required).expect("serialize required"),
            "\"required\""
        );
        assert_eq!(
            serde_json::to_string(&ToolChoice::Specific("tool_name".to_string()))
                .expect("serialize specific"),
            "{\"specific\":\"tool_name\"}"
        );
    }
}
