use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::error::LlmError;
use crate::provider::LlmProvider;
use crate::stream::StreamResult;
use crate::types::{ChatRole, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage, ToolCall};

use super::credentials::{
    load_credentials, refresh_oauth_token, AnthropicCredentials, CredentialSource,
};
use super::stream_parser::StreamParser;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_BETA_HEADER: &str =
    "oauth-2025-04-20,claude-code-20250219,interleaved-thinking-2025-05-14,fine-grained-tool-streaming-2025-05-14";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const CLAUDE_CODE_SYSTEM_MESSAGE: &str =
    "You are Claude Code, Anthropic's official CLI for Claude.";

pub struct AnthropicProvider {
    credentials: Arc<RwLock<AnthropicCredentials>>,
    client: reqwest::Client,
    model: String,
}

impl AnthropicProvider {
    pub fn new(model: &str) -> Result<Self, LlmError> {
        let credentials = load_credentials()?;
        Ok(Self {
            credentials: Arc::new(RwLock::new(credentials)),
            client: reqwest::Client::new(),
            model: model.to_string(),
        })
    }

    async fn get_access_token(&self) -> Result<(String, CredentialSource), LlmError> {
        let creds = self.credentials.read().await;
        if !creds.is_expired() {
            return Ok((creds.access_token.clone(), creds.source.clone()));
        }
        drop(creds);

        let mut creds = self.credentials.write().await;
        if !creds.is_expired() {
            return Ok((creds.access_token.clone(), creds.source.clone()));
        }

        if let Some(refresh_token) = creds.refresh_token.clone() {
            warn!(provider = "anthropic", "OAuth token expired, refreshing");
            let refreshed = refresh_oauth_token(&self.client, &refresh_token).await?;
            *creds = refreshed;
            Ok((creds.access_token.clone(), creds.source.clone()))
        } else {
            Err(LlmError::AuthFailed {
                message: "OAuth token expired and no refresh token available".to_string(),
            })
        }
    }

    fn build_headers(
        &self,
        token: &str,
        source: &CredentialSource,
    ) -> Result<reqwest::header::HeaderMap, LlmError> {
        use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            "anthropic-version",
            HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        headers.insert(
            "anthropic-beta",
            HeaderValue::from_static(ANTHROPIC_BETA_HEADER),
        );

        match source {
            CredentialSource::OAuthFile => {
                let bearer = format!("Bearer {token}");
                headers.insert(
                    "authorization",
                    HeaderValue::from_str(&bearer).map_err(|e| LlmError::AuthFailed {
                        message: format!("invalid OAuth token for authorization header: {e}"),
                    })?,
                );
            }
            CredentialSource::ApiKey => {
                headers.insert(
                    "x-api-key",
                    HeaderValue::from_str(token).map_err(|e| LlmError::AuthFailed {
                        message: format!("invalid API key for x-api-key header: {e}"),
                    })?,
                );
            }
        }

        Ok(headers)
    }

    fn prepare_messages(&self, req: &LlmRequest) -> Result<Vec<serde_json::Value>, LlmError> {
        let mut messages_json = Vec::new();
        let msgs = &req.messages;
        let mut i = 0;

        while i < msgs.len() {
            let msg = &msgs[i];
            match msg.role {
                ChatRole::System => {
                    i += 1;
                }
                ChatRole::User => {
                    messages_json.push(serde_json::json!({
                        "role": "user",
                        "content": msg.content,
                    }));
                    i += 1;
                }
                ChatRole::Tool => {
                    // Collect ALL consecutive Tool messages into a single user message.
                    // Anthropic requires every tool_use block be answered in one user
                    // message that contains all corresponding tool_result blocks.
                    let mut tool_results = Vec::new();
                    while i < msgs.len() && msgs[i].role == ChatRole::Tool {
                        let m = &msgs[i];
                        let tool_use_id =
                            m.tool_call_id
                                .clone()
                                .ok_or_else(|| LlmError::InvalidResponse {
                                    message: "tool result message missing tool_call_id".to_string(),
                                })?;
                        tool_results.push(serde_json::json!({
                            "type": "tool_result",
                            "tool_use_id": tool_use_id,
                            "content": m.content,
                        }));
                        i += 1;
                    }
                    messages_json.push(serde_json::json!({
                        "role": "user",
                        "content": tool_results,
                    }));
                }
                ChatRole::Assistant => {
                    if let Some(tool_calls) = &msg.tool_calls {
                        let mut content_blocks = Vec::new();

                        if !msg.content.is_empty() {
                            content_blocks.push(serde_json::json!({
                                "type": "text",
                                "text": msg.content,
                            }));
                        }

                        for tool_call in tool_calls {
                            let arguments: serde_json::Value =
                                serde_json::from_str(&tool_call.arguments).map_err(|e| {
                                    LlmError::InvalidResponse {
                                        message: format!(
                                            "invalid tool call arguments JSON for '{}': {e}",
                                            tool_call.id
                                        ),
                                    }
                                })?;
                            content_blocks.push(serde_json::json!({
                                "type": "tool_use",
                                "id": tool_call.id,
                                "name": tool_call.name,
                                "input": arguments,
                            }));
                        }

                        messages_json.push(serde_json::json!({
                            "role": "assistant",
                            "content": content_blocks,
                        }));
                    } else {
                        messages_json.push(serde_json::json!({
                            "role": "assistant",
                            "content": msg.content,
                        }));
                    }
                    i += 1;
                }
            }
        }

        Ok(messages_json)
    }

    fn extract_system(&self, req: &LlmRequest, source: &CredentialSource) -> Option<String> {
        let explicit_system = req
            .messages
            .iter()
            .find(|m| m.role == ChatRole::System)
            .map(|m| m.content.clone());

        match source {
            CredentialSource::OAuthFile => Some(match explicit_system {
                Some(message) => format!("{CLAUDE_CODE_SYSTEM_MESSAGE}\n\n{message}"),
                None => CLAUDE_CODE_SYSTEM_MESSAGE.to_string(),
            }),
            CredentialSource::ApiKey => explicit_system,
        }
    }

    fn build_request_body(
        &self,
        req: &LlmRequest,
        source: &CredentialSource,
        stream: bool,
    ) -> Result<serde_json::Value, LlmError> {
        let mut body = serde_json::json!({
            "model": self.model,
            "max_tokens": req.max_tokens.unwrap_or(8192),
            "messages": self.prepare_messages(req)?,
            "stream": stream,
        });

        if let Some(system) = self.extract_system(req, source) {
            body["system"] = serde_json::Value::String(system);
        }

        if let Some(temperature) = req.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        if let Some(tools) = &req.tools {
            let tools_json = tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters,
                    })
                })
                .collect::<Vec<_>>();
            body["tools"] = serde_json::Value::Array(tools_json);
        }

        Ok(body)
    }

    fn parse_response(&self, json: serde_json::Value) -> LlmResponse {
        let model = json
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.model)
            .to_string();
        let finish_reason = json
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("stop")
            .to_string();

        let usage = match json.get("usage") {
            Some(usage_json) => {
                let input = usage_json
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let output = usage_json
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                TokenUsage {
                    input_tokens: input,
                    output_tokens: output,
                    total_tokens: input + output,
                }
            }
            None => TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
        };

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        if let Some(content_blocks) = json.get("content").and_then(|v| v.as_array()) {
            for block in content_blocks {
                let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                match block_type {
                    "text" => {
                        if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                            text_parts.push(text.to_string());
                        }
                    }
                    "tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let input = block
                            .get("input")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({}));
                        let arguments =
                            serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                        tool_calls.push(ToolCall {
                            id,
                            name,
                            arguments,
                            execution_depth: 0,
                        });
                    }
                    _ => {}
                }
            }
        }

        LlmResponse {
            content: text_parts.join(""),
            usage,
            finish_reason,
            provider_id: ProviderId(self.provider_id().to_string()),
            model_id: ModelId(model),
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            reasoning_content: None,
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for AnthropicProvider {
    fn provider_id(&self) -> &str {
        "anthropic"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    async fn chat(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let (token, source) = self.get_access_token().await?;
        let headers = self.build_headers(&token, &source)?;
        let body = self.build_request_body(&req, &source, false)?;

        debug!(provider = "anthropic", model = %self.model, "sending chat request");

        let response = self
            .client
            .post(ANTHROPIC_API_URL)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transport {
                source: Box::new(e),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::InvalidResponse {
                message: format!("Anthropic API error {status}: {body}"),
            });
        }

        let json: serde_json::Value =
            response
                .json()
                .await
                .map_err(|e| LlmError::InvalidResponse {
                    message: format!("failed to parse Anthropic response JSON: {e}"),
                })?;

        Ok(self.parse_response(json))
    }

    async fn chat_stream(&self, req: LlmRequest) -> Result<StreamResult, LlmError> {
        use eventsource_stream::Eventsource;
        use tokio_stream::StreamExt;

        let (token, source) = self.get_access_token().await?;
        let headers = self.build_headers(&token, &source)?;
        let body = self.build_request_body(&req, &source, true)?;

        debug!(provider = "anthropic", model = %self.model, "sending streaming chat request");

        let response = self
            .client
            .post(ANTHROPIC_API_URL)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| LlmError::Transport {
                source: Box::new(e),
            })?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(LlmError::InvalidResponse {
                message: format!("Anthropic streaming API error {status}: {body}"),
            });
        }

        let sse_stream = response.bytes_stream().eventsource();

        let output_stream = async_stream::stream! {
            let mut parser = StreamParser::new();
            tokio::pin!(sse_stream);

            while let Some(item) = sse_stream.next().await {
                match item {
                    Ok(event) => {
                        let events = parser.parse_event(&event.event, &event.data);
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
mod tests {
    use super::*;

    use crate::provider::LlmProvider;

    use super::super::credentials::make_test_credentials;

    #[test]
    fn test_parse_response_text_only() {
        let provider = AnthropicProvider {
            credentials: Arc::new(RwLock::new(make_test_credentials("test-token"))),
            client: reqwest::Client::new(),
            model: "claude-sonnet-4-6".to_string(),
        };

        let json = serde_json::json!({
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-6",
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 10, "output_tokens": 20},
            "content": [{"type": "text", "text": "Hello, world!"}]
        });

        let response = provider.parse_response(json);
        assert_eq!(response.content, "Hello, world!");
        assert_eq!(response.finish_reason, "end_turn");
        assert_eq!(response.usage.input_tokens, 10);
        assert_eq!(response.usage.output_tokens, 20);
        assert!(response.tool_calls.is_none());
    }

    #[test]
    fn test_parse_response_with_tool_calls() {
        let provider = AnthropicProvider {
            credentials: Arc::new(RwLock::new(make_test_credentials("test-token"))),
            client: reqwest::Client::new(),
            model: "claude-sonnet-4-6".to_string(),
        };

        let json = serde_json::json!({
            "id": "msg_456",
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-4-6",
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 5, "output_tokens": 10},
            "content": [
                {"type": "text", "text": "Let me read that file."},
                {"type": "tool_use", "id": "tc_1", "name": "fs_read", "input": {"path": "src/main.rs"}}
            ]
        });

        let response = provider.parse_response(json);
        assert_eq!(response.content, "Let me read that file.");
        assert_eq!(response.finish_reason, "tool_use");

        let tool_calls = response
            .tool_calls
            .expect("response should include tool call");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "tc_1");
        assert_eq!(tool_calls[0].name, "fs_read");
        assert!(tool_calls[0].arguments.contains("main.rs"));
    }

    #[test]
    fn test_prepare_messages_maps_tool_result_to_user_tool_result_block() {
        let provider = AnthropicProvider {
            credentials: Arc::new(RwLock::new(make_test_credentials("test-token"))),
            client: reqwest::Client::new(),
            model: "claude-sonnet-4-6".to_string(),
        };

        let req = LlmRequest {
            messages: vec![
                crate::types::ChatMessage {
                    role: ChatRole::System,
                    content: "system".to_string(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                crate::types::ChatMessage {
                    role: ChatRole::Tool,
                    content: "file-content".to_string(),
                    tool_calls: None,
                    tool_call_id: Some("tc_1".to_string()),
                    reasoning_content: None,
                },
            ],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };

        let mapped = provider
            .prepare_messages(&req)
            .expect("prepare_messages should map tool result");

        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0]["role"], "user");
        assert_eq!(mapped[0]["content"][0]["type"], "tool_result");
        assert_eq!(mapped[0]["content"][0]["tool_use_id"], "tc_1");
    }

    #[test]
    fn test_prepare_messages_merges_multiple_tool_results_into_one_user_message() {
        let provider = AnthropicProvider {
            credentials: Arc::new(RwLock::new(make_test_credentials("test-token"))),
            client: reqwest::Client::new(),
            model: "claude-sonnet-4-6".to_string(),
        };

        let req = LlmRequest {
            messages: vec![
                crate::types::ChatMessage {
                    role: ChatRole::User,
                    content: "call two tools".to_string(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                crate::types::ChatMessage {
                    role: ChatRole::Assistant,
                    content: String::new(),
                    tool_calls: Some(vec![
                        crate::types::ToolCall {
                            id: "tc_1".to_string(),
                            name: "fs_read".to_string(),
                            arguments: "{}".to_string(),
                            execution_depth: 0,
                        },
                        crate::types::ToolCall {
                            id: "tc_2".to_string(),
                            name: "git_status".to_string(),
                            arguments: "{}".to_string(),
                            execution_depth: 0,
                        },
                    ]),
                    tool_call_id: None,
                    reasoning_content: None,
                },
                crate::types::ChatMessage {
                    role: ChatRole::Tool,
                    content: "file-content".to_string(),
                    tool_calls: None,
                    tool_call_id: Some("tc_1".to_string()),
                    reasoning_content: None,
                },
                crate::types::ChatMessage {
                    role: ChatRole::Tool,
                    content: "git status output".to_string(),
                    tool_calls: None,
                    tool_call_id: Some("tc_2".to_string()),
                    reasoning_content: None,
                },
            ],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };

        let mapped = provider
            .prepare_messages(&req)
            .expect("prepare_messages should merge tool results");

        // user + assistant + ONE merged tool_result user message = 3
        assert_eq!(
            mapped.len(),
            3,
            "multiple tool results must be one user message"
        );
        let tool_msg = &mapped[2];
        assert_eq!(tool_msg["role"], "user");
        let content = tool_msg["content"]
            .as_array()
            .expect("content must be array");
        assert_eq!(
            content.len(),
            2,
            "both tool_result blocks must be in one message"
        );
        assert_eq!(content[0]["type"], "tool_result");
        assert_eq!(content[0]["tool_use_id"], "tc_1");
        assert_eq!(content[1]["type"], "tool_result");
        assert_eq!(content[1]["tool_use_id"], "tc_2");
    }

    #[test]
    fn test_extract_system_injects_claude_code_for_oauth() {
        let provider = AnthropicProvider {
            credentials: Arc::new(RwLock::new(make_test_credentials("test-token"))),
            client: reqwest::Client::new(),
            model: "claude-sonnet-4-6".to_string(),
        };

        let req = LlmRequest {
            messages: vec![crate::types::ChatMessage {
                role: ChatRole::System,
                content: "existing system".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            max_tokens: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };

        let system = provider
            .extract_system(&req, &CredentialSource::OAuthFile)
            .expect("oauth system must be set");
        assert!(system.starts_with(CLAUDE_CODE_SYSTEM_MESSAGE));
        assert!(system.contains("existing system"));
    }

    #[test]
    fn test_provider_trait_methods() {
        let provider = AnthropicProvider {
            credentials: Arc::new(RwLock::new(make_test_credentials("test-token"))),
            client: reqwest::Client::new(),
            model: "claude-sonnet-4-6".to_string(),
        };
        assert_eq!(provider.provider_id(), "anthropic");
        assert_eq!(provider.model_id(), "claude-sonnet-4-6");
    }
}
