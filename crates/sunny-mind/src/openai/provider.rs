use std::sync::Arc;
use std::{future::Future, pin::Pin};

use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::error::LlmError;
use crate::provider::LlmProvider;
use crate::stream::StreamResult;
use crate::types::{ChatRole, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage};

use super::credentials::{
    load_credentials, refresh_oauth_token, save_credentials, OpenAiCredentials,
};
use super::stream_parser::StreamParser;

const OPENAI_API_URL: &str = "https://api.openai.com/v1/chat/completions";

/// OpenAI chat completions provider.
///
/// Supports API key authentication (via `OPENAI_API_KEY` or `~/.sunny/openai_credentials.json`)
/// and OAuth token authentication (via `sunny login --openai`).
pub struct OpenAiProvider {
    credentials: Arc<RwLock<OpenAiCredentials>>,
    client: reqwest::Client,
    model: String,
}

impl OpenAiProvider {
    /// Create a new `OpenAiProvider` for the given model.
    ///
    /// Loads credentials from `OPENAI_API_KEY` environment variable or
    /// `~/.sunny/openai_credentials.json`.
    ///
    /// # Errors
    /// Returns [`LlmError::NotConfigured`] if no credentials are found.
    pub fn new(model: &str) -> Result<Self, LlmError> {
        let credentials = load_credentials()?;
        Ok(Self {
            credentials: Arc::new(RwLock::new(credentials)),
            client: reqwest::Client::new(),
            model: model.to_string(),
        })
    }

    async fn get_bearer_token(&self) -> Result<String, LlmError> {
        let creds = self.credentials.read().await;
        if !creds.is_expired() {
            return Ok(creds.bearer_token().to_string());
        }
        drop(creds);

        let mut creds = self.credentials.write().await;
        if !creds.is_expired() {
            return Ok(creds.bearer_token().to_string());
        }

        if let Some(refresh_token) = creds.refresh_token.clone() {
            warn!(provider = "openai", "OAuth token expired, refreshing");
            Self::refresh_with_recovery(
                &self.client,
                &mut creds,
                refresh_token,
                |client, token| Box::pin(refresh_oauth_token(client, token)),
                save_credentials,
            )
            .await?;
            Ok(creds.bearer_token().to_string())
        } else {
            Err(LlmError::AuthFailed {
                message: "OpenAI OAuth token expired and no refresh token available".to_string(),
            })
        }
    }

    async fn refresh_with_recovery<RF, SF>(
        client: &reqwest::Client,
        creds: &mut OpenAiCredentials,
        refresh_token: String,
        mut refresh_fn: RF,
        save_fn: SF,
    ) -> Result<(), LlmError>
    where
        RF: for<'a> FnMut(
            &'a reqwest::Client,
            &'a str,
        ) -> Pin<
            Box<dyn Future<Output = Result<OpenAiCredentials, LlmError>> + Send + 'a>,
        >,
        SF: Fn(&OpenAiCredentials) -> Result<(), LlmError>,
    {
        match refresh_fn(client, &refresh_token).await {
            Ok(refreshed) => {
                *creds = refreshed;
                if let Err(e) = save_fn(creds) {
                    warn!(
                        provider = "openai",
                        "Failed to persist refreshed credentials: {e}"
                    );
                }
                Ok(())
            }
            Err(e) => {
                if e.to_string().contains("invalid_grant") {
                    Err(LlmError::AuthFailed {
                        message: "OpenAI OAuth token refresh failed (invalid_grant). Run `sunny login --openai` to re-authenticate.".to_string(),
                    })
                } else {
                    Err(e)
                }
            }
        }
    }

    fn build_request_body(&self, req: &LlmRequest, stream: bool) -> serde_json::Value {
        let mut messages_json: Vec<serde_json::Value> = Vec::new();

        for msg in &req.messages {
            match msg.role {
                ChatRole::System => {
                    messages_json.push(serde_json::json!({
                        "role": "system",
                        "content": msg.content,
                    }));
                }
                ChatRole::User => {
                    messages_json.push(serde_json::json!({
                        "role": "user",
                        "content": msg.content,
                    }));
                }
                ChatRole::Assistant => {
                    if let Some(tool_calls) = &msg.tool_calls {
                        let tc_json: Vec<serde_json::Value> = tool_calls
                            .iter()
                            .map(|tc| {
                                serde_json::json!({
                                    "id": tc.id,
                                    "type": "function",
                                    "function": {
                                        "name": tc.name,
                                        "arguments": tc.arguments,
                                    }
                                })
                            })
                            .collect();
                        messages_json.push(serde_json::json!({
                            "role": "assistant",
                            "content": if msg.content.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(msg.content.clone()) },
                            "tool_calls": tc_json,
                        }));
                    } else {
                        messages_json.push(serde_json::json!({
                            "role": "assistant",
                            "content": msg.content,
                        }));
                    }
                }
                ChatRole::Tool => {
                    let tool_call_id = msg
                        .tool_call_id
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());
                    messages_json.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": msg.content,
                    }));
                }
            }
        }

        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": req.max_tokens.unwrap_or(8192),
            "messages": messages_json,
            "stream": stream,
        });

        if stream {
            body["stream_options"] = serde_json::json!({"include_usage": true});
        }

        if let Some(temperature) = req.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        if let Some(tools) = &req.tools {
            // OpenAI tool format: {type: "function", function: {name, description, parameters}}
            let tools_json: Vec<serde_json::Value> = tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tools_json);
            body["tool_choice"] = serde_json::json!("auto");
        }

        body
    }

    fn parse_response(&self, json: serde_json::Value) -> LlmResponse {
        let model = json
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.model)
            .to_string();

        let choice = json
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first());

        let finish_reason = choice
            .and_then(|c| c.get("finish_reason"))
            .and_then(|v| v.as_str())
            .unwrap_or("stop")
            .to_string();

        let content = choice
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let tool_calls = choice
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("tool_calls"))
            .map(StreamParser::parse_tool_calls_from_response)
            .filter(|calls| !calls.is_empty());

        let usage = json
            .get("usage")
            .map(|u| {
                let input = u.get("prompt_tokens").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                let output = u
                    .get("completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                TokenUsage {
                    input_tokens: input,
                    output_tokens: output,
                    total_tokens: input + output,
                }
            })
            .unwrap_or(TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            });

        LlmResponse {
            content,
            usage,
            finish_reason,
            provider_id: ProviderId(self.provider_id().to_string()),
            model_id: ModelId(model),
            tool_calls,
            reasoning_content: None,
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for OpenAiProvider {
    fn provider_id(&self) -> &str {
        "openai"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    async fn chat(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let token = self.get_bearer_token().await?;
        let body = self.build_request_body(&req, false);

        debug!(provider = "openai", model = %self.model, "sending chat request");

        let response = self
            .client
            .post(OPENAI_API_URL)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transport {
                source: Box::new(e),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            // Handle 401 as auth failure for better diagnostics.
            if status.as_u16() == 401 {
                return Err(LlmError::AuthFailed {
                    message: "OpenAI API returned 401 Unauthorized — check your API key or run `sunny login --openai`".to_string(),
                });
            }
            let body_text = response.text().await.unwrap_or_default();
            return Err(LlmError::InvalidResponse {
                message: format!("OpenAI API error {status}: {body_text}"),
            });
        }

        let json: serde_json::Value =
            response
                .json()
                .await
                .map_err(|e| LlmError::InvalidResponse {
                    message: format!("failed to parse OpenAI response JSON: {e}"),
                })?;

        Ok(self.parse_response(json))
    }

    async fn chat_stream(&self, req: LlmRequest) -> Result<StreamResult, LlmError> {
        let token = self.get_bearer_token().await?;
        let body = self.build_request_body(&req, true);

        debug!(provider = "openai", model = %self.model, "sending streaming chat request");

        let response = self
            .client
            .post(OPENAI_API_URL)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transport {
                source: Box::new(e),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            if status.as_u16() == 401 {
                return Err(LlmError::AuthFailed {
                    message: "OpenAI API returned 401 Unauthorized — check your API key or run `sunny login --openai`".to_string(),
                });
            }
            let body_text = response.text().await.unwrap_or_default();
            return Err(LlmError::InvalidResponse {
                message: format!("OpenAI streaming API error {status}: {body_text}"),
            });
        }

        // OpenAI streaming uses `data: {json}\n\n` lines (standard SSE).
        use eventsource_stream::Eventsource;
        use tokio_stream::StreamExt;

        let sse_stream = response.bytes_stream().eventsource();

        let output_stream = async_stream::stream! {
            let mut parser = StreamParser::new();
            tokio::pin!(sse_stream);

            while let Some(item) = sse_stream.next().await {
                match item {
                    Ok(event) => {
                        let events = parser.parse_chunk(&event.data);
                        for parsed in events {
                            yield parsed;
                        }
                    }
                    Err(e) => {
                        yield Err(LlmError::Transport {
                            source: Box::new(std::io::Error::other(e.to_string())),
                        });
                        return;
                    }
                }
            }
        };

        Ok(Box::pin(output_stream))
    }
}

#[cfg(test)]
pub(crate) fn build_test_provider(token: &str, model: &str) -> OpenAiProvider {
    use super::credentials::make_test_credentials;
    OpenAiProvider {
        credentials: Arc::new(RwLock::new(make_test_credentials(token))),
        client: reqwest::Client::new(),
        model: model.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ChatMessage;

    #[test]
    fn test_openai_provider_request_format() {
        let provider = build_test_provider("sk-test", "gpt-5.4");

        let req = LlmRequest {
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: "Be helpful.".to_string(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: "Hello".to_string(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            max_tokens: Some(100),
            temperature: None,
            tools: None,
            tool_choice: None,
            thinking_budget: None,
        };

        let body = provider.build_request_body(&req, false);

        // System message must appear as role "system".
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "Be helpful.");
        assert_eq!(messages[1]["role"], "user");
        assert_eq!(messages[1]["content"], "Hello");
        assert_eq!(body["model"], "gpt-5.4");
        assert_eq!(body["max_tokens"], 100);
        assert_eq!(body["stream"], false);
    }

    #[test]
    fn test_openai_provider_tool_format() {
        use crate::types::ToolDefinition;
        let provider = build_test_provider("sk-test", "gpt-5.4");

        let req = LlmRequest {
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "do something".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            max_tokens: None,
            temperature: None,
            tools: Some(vec![ToolDefinition {
                name: "fs_read".to_string(),
                description: "Read a file".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    },
                    "required": ["path"]
                }),
            }]),
            tool_choice: None,
            thinking_budget: None,
        };

        let body = provider.build_request_body(&req, false);

        // OpenAI tools must be wrapped in {type: "function", function: {...}}
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "fs_read");
        assert_eq!(tools[0]["function"]["description"], "Read a file");
    }

    #[test]
    fn test_openai_provider_tool_result_format() {
        let provider = build_test_provider("sk-test", "gpt-5.4");

        let req = LlmRequest {
            messages: vec![ChatMessage {
                role: ChatRole::Tool,
                content: "file content here".to_string(),
                tool_calls: None,
                tool_call_id: Some("call_abc".to_string()),
                reasoning_content: None,
            }],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
            thinking_budget: None,
        };

        let body = provider.build_request_body(&req, false);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "call_abc");
        assert_eq!(messages[0]["content"], "file content here");
    }

    #[test]
    fn test_parse_response_text_only() {
        let provider = build_test_provider("sk-test", "gpt-5.4");

        let json = serde_json::json!({
            "id": "chatcmpl-xxx",
            "model": "gpt-5.4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello, world!"
                },
                "finish_reason": "stop"
            }],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 5,
                "total_tokens": 15
            }
        });

        let response = provider.parse_response(json);
        assert_eq!(response.content, "Hello, world!");
        assert_eq!(response.finish_reason, "stop");
        assert_eq!(response.usage.input_tokens, 10);
        assert_eq!(response.usage.output_tokens, 5);
        assert!(response.tool_calls.is_none());
    }

    #[test]
    fn test_parse_response_with_tool_calls() {
        let provider = build_test_provider("sk-test", "gpt-5.4");

        let json = serde_json::json!({
            "id": "chatcmpl-yyy",
            "model": "gpt-5.4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "fs_read",
                            "arguments": "{\"path\":\"src/main.rs\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 15,
                "total_tokens": 35
            }
        });

        let response = provider.parse_response(json);
        assert_eq!(response.finish_reason, "tool_calls");
        let calls = response.tool_calls.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_abc");
        assert_eq!(calls[0].name, "fs_read");
        assert!(calls[0].arguments.contains("main.rs"));
    }

    #[test]
    fn test_provider_trait_methods() {
        let provider = build_test_provider("sk-test", "gpt-5.4");
        assert_eq!(provider.provider_id(), "openai");
        assert_eq!(provider.model_id(), "gpt-5.4");
    }
}
