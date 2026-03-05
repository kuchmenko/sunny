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
pub mod provider;
pub mod types;

pub use error::LlmError;
pub use kimi::KimiProvider;
pub use provider::LlmProvider;
pub use types::{
    ChatMessage, ChatRole, LlmRequest, LlmResponse, ModelId, ProviderEconomics, ProviderId,
    ProviderRoutingPolicy, TokenUsage,
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
        };
        assert_eq!(req.messages.len(), 1);
        assert!(req.max_tokens.is_none());
        assert!(req.temperature.is_none());

        let req_with_opts = LlmRequest {
            messages: vec![],
            max_tokens: Some(1024),
            temperature: Some(0.7),
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
        };
        let req_json = serde_json::to_string(&req).expect("serialize LlmRequest");
        let req_de: LlmRequest = serde_json::from_str(&req_json).expect("deserialize LlmRequest");
        assert_eq!(req, req_de);
    }
}
