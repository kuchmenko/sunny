use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::{LlmError, LlmProvider};
use crate::{LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage, ToolChoice};

const DEFAULT_KIMI_API_BASE_URL: &str = "https://api.moonshot.ai/v1";
const DEFAULT_KIMI_API_MODEL: &str = "kimi-k2.5";
const DEFAULT_KIMI_CODING_BASE_URL: &str = "https://api.kimi.com/coding/v1";
const DEFAULT_KIMI_CODING_MODEL: &str = "kimi-for-coding";
const DEFAULT_KIMI_CODING_USER_AGENT: &str = "kimi-cli/1.0";
const DEFAULT_TIMEOUT_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KimiAuthMode {
    Api,
    CodingPlan,
}

impl KimiAuthMode {
    fn from_env_or_key_prefix(api_key: &str) -> Result<Self, LlmError> {
        match std::env::var("KIMI_AUTH_MODE") {
            Ok(raw) => match raw.as_str() {
                "api" => Ok(Self::Api),
                "coding_plan" => Ok(Self::CodingPlan),
                other => Err(LlmError::UnsupportedAuthMode {
                    mode: other.to_string(),
                }),
            },
            Err(_) => {
                if api_key.starts_with("sk-kimi-") {
                    Ok(Self::CodingPlan)
                } else {
                    Ok(Self::Api)
                }
            }
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Api => "api",
            Self::CodingPlan => "coding_plan",
        }
    }
}

#[derive(Debug)]
pub struct KimiProvider {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    model: String,
    timeout: Duration,
    auth_mode: KimiAuthMode,
    user_agent: Option<String>,
}

impl KimiProvider {
    pub fn from_env() -> Result<Self, LlmError> {
        let api_key = std::env::var("KIMI_API_KEY").map_err(|_| LlmError::NotConfigured {
            message: "KIMI_API_KEY is required".to_string(),
        })?;

        let auth_mode = KimiAuthMode::from_env_or_key_prefix(&api_key)?;

        let default_base_url = match auth_mode {
            KimiAuthMode::Api => DEFAULT_KIMI_API_BASE_URL,
            KimiAuthMode::CodingPlan => DEFAULT_KIMI_CODING_BASE_URL,
        };
        let default_model = match auth_mode {
            KimiAuthMode::Api => DEFAULT_KIMI_API_MODEL,
            KimiAuthMode::CodingPlan => DEFAULT_KIMI_CODING_MODEL,
        };

        let base_url = std::env::var("KIMI_BASE_URL")
            .unwrap_or_else(|_| default_base_url.to_string())
            .trim_end_matches('/')
            .to_string();
        let model = std::env::var("KIMI_MODEL").unwrap_or_else(|_| default_model.to_string());

        let user_agent = match auth_mode {
            KimiAuthMode::Api => None,
            KimiAuthMode::CodingPlan => Some(
                std::env::var("KIMI_USER_AGENT")
                    .unwrap_or_else(|_| DEFAULT_KIMI_CODING_USER_AGENT.to_string()),
            ),
        };

        let timeout_ms = match std::env::var("LLM_TIMEOUT_MS") {
            Ok(raw) => raw.parse::<u64>().map_err(|_| LlmError::NotConfigured {
                message: "LLM_TIMEOUT_MS must be a positive integer".to_string(),
            })?,
            Err(_) => DEFAULT_TIMEOUT_MS,
        };

        Ok(Self::new_with_mode(
            api_key,
            base_url,
            model,
            Duration::from_millis(timeout_ms),
            auth_mode,
            user_agent,
        ))
    }

    pub fn new(api_key: String, base_url: String, model: String, timeout: Duration) -> Self {
        Self::new_with_mode(api_key, base_url, model, timeout, KimiAuthMode::Api, None)
    }

    fn new_with_mode(
        api_key: String,
        base_url: String,
        model: String,
        timeout: Duration,
        auth_mode: KimiAuthMode,
        user_agent: Option<String>,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            api_key,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            timeout,
            auth_mode,
            user_agent,
        }
    }

    fn timeout_ms(&self) -> u64 {
        self.timeout.as_millis() as u64
    }

    pub fn auth_mode(&self) -> &'static str {
        self.auth_mode.as_str()
    }

    /// Parse the HTTP response into LlmResponse
    async fn parse_response(&self, response: reqwest::Response) -> Result<LlmResponse, LlmError> {
        let body: KimiChatResponse =
            response
                .json()
                .await
                .map_err(|err| LlmError::InvalidResponse {
                    message: err.to_string(),
                })?;

        let first_choice = body
            .choices
            .first()
            .ok_or_else(|| LlmError::InvalidResponse {
                message: "missing choices[0] in provider response".to_string(),
            })?;

        let content = if !first_choice.message.content.trim().is_empty() {
            first_choice.message.content.clone()
        } else {
            first_choice
                .message
                .reasoning_content
                .clone()
                .unwrap_or_default()
        };

        let tool_calls = first_choice
            .message
            .tool_calls
            .clone()
            .map(|calls| {
                calls
                    .into_iter()
                    .map(map_kimi_tool_call)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        Ok(LlmResponse {
            content,
            usage: TokenUsage {
                input_tokens: body.usage.prompt_tokens,
                output_tokens: body.usage.completion_tokens,
                total_tokens: body.usage.total_tokens,
            },
            finish_reason: first_choice.finish_reason.clone(),
            provider_id: ProviderId(self.provider_id().to_string()),
            model_id: ModelId(self.model.clone()),
            tool_calls,
            reasoning_content: first_choice.message.reasoning_content.clone(),
        })
    }
}

/// Wire format for a single message in an outbound Kimi chat request.
///
/// Maps our domain [`crate::ChatMessage`] to the OpenAI-compatible message schema.
/// Kimi uses the same wire format as OpenAI; other providers will implement their own mapping.
#[derive(Debug, Serialize, Clone)]
#[serde(untagged)]
enum KimiOutboundMessage {
    /// user / system / plain assistant (no tool calls) messages.
    Standard { role: &'static str, content: String },
    /// Assistant message that invoked one or more tools.
    AssistantWithTools {
        role: &'static str,
        content: String,
        tool_calls: Vec<KimiRequestToolCall>,
        /// Kimi thinking models require reasoning_content to be echoed verbatim on follow-up turns.
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
    },
    /// Tool result correlated with a prior [`AssistantWithTools`] message.
    ToolResult {
        role: &'static str,
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Serialize, Clone)]
struct KimiRequestToolCall {
    id: String,
    r#type: &'static str,
    function: KimiRequestToolCallFunction,
}

#[derive(Debug, Serialize, Clone)]
struct KimiRequestToolCallFunction {
    name: String,
    arguments: String,
}

fn map_to_kimi_message(msg: crate::ChatMessage) -> Result<KimiOutboundMessage, LlmError> {
    match msg.role {
        crate::ChatRole::Tool => {
            let tool_call_id = msg.tool_call_id.ok_or_else(|| LlmError::InvalidResponse {
                message: "tool result message is missing tool_call_id".to_string(),
            })?;
            Ok(KimiOutboundMessage::ToolResult {
                role: "tool",
                tool_call_id,
                content: msg.content,
            })
        }
        crate::ChatRole::Assistant if msg.tool_calls.is_some() => {
            let tool_calls = msg
                .tool_calls
                .expect("checked above via is_some guard")
                .into_iter()
                .map(|tc| KimiRequestToolCall {
                    id: tc.id,
                    r#type: "function",
                    function: KimiRequestToolCallFunction {
                        name: tc.name,
                        arguments: tc.arguments,
                    },
                })
                .collect();
            Ok(KimiOutboundMessage::AssistantWithTools {
                role: "assistant",
                content: msg.content,
                tool_calls,
                reasoning_content: msg.reasoning_content,
            })
        }
        role => {
            let role_str = match role {
                crate::ChatRole::System => "system",
                crate::ChatRole::User => "user",
                crate::ChatRole::Assistant => "assistant",
                crate::ChatRole::Tool => unreachable!("Tool role is handled in the first arm"),
            };
            Ok(KimiOutboundMessage::Standard {
                role: role_str,
                content: msg.content,
            })
        }
    }
}

#[derive(Serialize, Clone)]
struct KimiChatRequest {
    model: String,
    messages: Vec<KimiOutboundMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<KimiToolDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<KimiToolChoice>,
}

#[derive(Serialize, Clone)]
struct KimiToolDefinition {
    r#type: &'static str,
    function: KimiToolFunction,
}

#[derive(Serialize, Clone)]
struct KimiToolFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

#[derive(Serialize, Clone)]
#[serde(untagged)]
enum KimiToolChoice {
    Mode(&'static str),
    Specific {
        r#type: &'static str,
        function: KimiToolChoiceFunction,
    },
}

#[derive(Serialize, Clone)]
struct KimiToolChoiceFunction {
    name: String,
}

fn map_tool_choice(tool_choice: Option<ToolChoice>) -> Result<Option<KimiToolChoice>, LlmError> {
    match tool_choice {
        None => Ok(None),
        Some(ToolChoice::Auto) => Ok(Some(KimiToolChoice::Mode("auto"))),
        Some(ToolChoice::None) => Ok(Some(KimiToolChoice::Mode("none"))),
        Some(ToolChoice::Specific(name)) => Ok(Some(KimiToolChoice::Specific {
            r#type: "function",
            function: KimiToolChoiceFunction { name },
        })),
        Some(ToolChoice::Required) => Err(LlmError::InvalidResponse {
            message: "Kimi does not support tool_choice=required".to_string(),
        }),
    }
}

#[derive(Deserialize)]
struct KimiChatResponse {
    choices: Vec<KimiChoice>,
    usage: KimiUsage,
}

#[derive(Deserialize)]
struct KimiChoice {
    message: KimiMessage,
    finish_reason: String,
}

#[derive(Deserialize)]
struct KimiMessage {
    content: String,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<KimiToolCall>>,
}

#[derive(Debug, Clone, Deserialize)]
struct KimiToolCall {
    id: String,
    #[serde(default)]
    function: Option<KimiToolCallFunction>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct KimiToolCallFunction {
    name: String,
    arguments: String,
}

fn map_kimi_tool_call(call: KimiToolCall) -> Result<crate::ToolCall, LlmError> {
    let KimiToolCall {
        id,
        function,
        name,
        arguments,
    } = call;

    let (name, arguments) = match function {
        Some(function) => (function.name, function.arguments),
        None => match (name, arguments) {
            (Some(name), Some(arguments)) => (name, arguments),
            (maybe_name, maybe_arguments) => {
                let detail = match (maybe_name.is_some(), maybe_arguments.is_some()) {
                    (false, false) => "missing tool call function, name, and arguments",
                    (false, true) => "missing tool call function and name",
                    (true, false) => "missing tool call function and arguments",
                    (true, true) => "missing tool call function",
                };

                return Err(LlmError::InvalidResponse {
                    message: format!("malformed Kimi tool call '{id}': {detail}"),
                });
            }
        },
    };

    Ok(crate::ToolCall {
        id,
        name,
        arguments,
        execution_depth: 0,
    })
}

#[derive(Deserialize)]
struct KimiUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
    total_tokens: u32,
}

#[async_trait::async_trait]
impl LlmProvider for KimiProvider {
    fn provider_id(&self) -> &str {
        "kimi"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    async fn chat(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let tool_choice = map_tool_choice(req.tool_choice)?;
        let tools = req.tools.map(|defs| {
            defs.into_iter()
                .map(|def| KimiToolDefinition {
                    r#type: "function",
                    function: KimiToolFunction {
                        name: def.name,
                        description: def.description,
                        parameters: def.parameters,
                    },
                })
                .collect::<Vec<_>>()
        });

        let payload = KimiChatRequest {
            model: self.model.clone(),
            messages: req
                .messages
                .into_iter()
                .map(map_to_kimi_message)
                .collect::<Result<Vec<_>, _>>()?,
            max_tokens: req.max_tokens,
            temperature: req.temperature,
            tools,
            tool_choice,
        };

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url));

        let mut request = response
            .bearer_auth(&self.api_key)
            .timeout(self.timeout)
            .json(&payload);

        if self.auth_mode == KimiAuthMode::CodingPlan {
            if let Some(ua) = &self.user_agent {
                request = request.header(reqwest::header::USER_AGENT, ua);
            }
        }

        let response = request.send().await.map_err(|err| {
            if err.is_timeout() {
                LlmError::Timeout {
                    timeout_ms: self.timeout_ms(),
                }
            } else {
                LlmError::Transport {
                    source: Box::new(err),
                }
            }
        })?;

        let status = response.status();
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "unauthorized".to_string());
            return Err(LlmError::AuthFailed { message });
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(LlmError::RateLimited);
        }
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read error body>".to_string());

            // Check if this is a temperature error for kimi-k2.5
            if body.contains("invalid temperature") && body.contains("only 1 is allowed") {
                eprintln!(
                    "WARN: Kimi model {} requires temperature=1 (requested: {}), retrying with override",
                    self.model,
                    payload.temperature.unwrap_or(1.0)
                );

                // Retry with temperature=1
                let mut retry_payload = payload.clone();
                retry_payload.temperature = Some(1.0);

                let retry_response = self
                    .client
                    .post(format!("{}/chat/completions", self.base_url))
                    .bearer_auth(&self.api_key)
                    .timeout(self.timeout)
                    .json(&retry_payload)
                    .send()
                    .await
                    .map_err(|err| LlmError::Transport {
                        source: Box::new(err),
                    })?;

                if retry_response.status().is_success() {
                    return self.parse_response(retry_response).await;
                }
            }

            return Err(LlmError::Transport {
                source: Box::new(std::io::Error::other(format!(
                    "unexpected provider status: {} body: {}",
                    status, body
                ))),
            });
        }

        let body: KimiChatResponse =
            response
                .json()
                .await
                .map_err(|err| LlmError::InvalidResponse {
                    message: err.to_string(),
                })?;

        let first_choice = body
            .choices
            .first()
            .ok_or_else(|| LlmError::InvalidResponse {
                message: "missing choices[0] in provider response".to_string(),
            })?;

        let content = if !first_choice.message.content.trim().is_empty() {
            first_choice.message.content.clone()
        } else {
            first_choice
                .message
                .reasoning_content
                .clone()
                .unwrap_or_default()
        };

        let tool_calls = first_choice
            .message
            .tool_calls
            .clone()
            .map(|calls| {
                calls
                    .into_iter()
                    .map(map_kimi_tool_call)
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;

        Ok(LlmResponse {
            content,
            usage: TokenUsage {
                input_tokens: body.usage.prompt_tokens,
                output_tokens: body.usage.completion_tokens,
                total_tokens: body.usage.total_tokens,
            },
            finish_reason: first_choice.finish_reason.clone(),
            provider_id: ProviderId(self.provider_id().to_string()),
            model_id: ModelId(self.model.clone()),
            tool_calls,
            reasoning_content: first_choice.message.reasoning_content.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::KimiAuthMode;

    use crate::{
        ChatMessage, ChatRole, KimiProvider, LlmError, LlmProvider, LlmRequest, ModelId,
        ProviderId, TokenUsage, ToolChoice,
    };

    fn env_lock() -> &'static Mutex<()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn clear_kimi_env() {
        std::env::remove_var("KIMI_API_KEY");
        std::env::remove_var("KIMI_BASE_URL");
        std::env::remove_var("KIMI_MODEL");
        std::env::remove_var("KIMI_AUTH_MODE");
        std::env::remove_var("KIMI_USER_AGENT");
        std::env::remove_var("LLM_TIMEOUT_MS");
    }

    fn test_request() -> LlmRequest {
        LlmRequest {
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "hello".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            max_tokens: Some(128),
            temperature: Some(0.2),
            tools: None,
            tool_choice: None,
        }
    }

    #[test]
    fn test_kimi_provider_id() {
        let provider = KimiProvider::new(
            "test-key".to_string(),
            "https://api.moonshot.ai/v1".to_string(),
            "kimi-k2.5".to_string(),
            Duration::from_secs(30),
        );

        assert_eq!(provider.provider_id(), "kimi");
        assert_eq!(provider.auth_mode(), "api");
    }

    #[test]
    fn test_kimi_from_env_auto_detects_coding_plan_from_key() {
        let _guard = env_lock().lock().expect("env lock");
        clear_kimi_env();
        std::env::set_var("KIMI_API_KEY", "sk-kimi-test-key");

        let provider = KimiProvider::from_env().expect("provider should load from env");

        assert_eq!(provider.auth_mode(), "coding_plan");
        assert_eq!(provider.model_id(), "kimi-for-coding");
        clear_kimi_env();
    }

    #[test]
    fn test_kimi_from_env_respects_explicit_auth_mode_api() {
        let _guard = env_lock().lock().expect("env lock");
        clear_kimi_env();
        std::env::set_var("KIMI_API_KEY", "sk-kimi-test-key");
        std::env::set_var("KIMI_AUTH_MODE", "api");

        let provider = KimiProvider::from_env().expect("provider should load from env");

        assert_eq!(provider.auth_mode(), "api");
        assert_eq!(provider.model_id(), "kimi-k2.5");
        clear_kimi_env();
    }

    #[test]
    fn test_kimi_from_env_invalid_auth_mode_errors() {
        let _guard = env_lock().lock().expect("env lock");
        clear_kimi_env();
        std::env::set_var("KIMI_API_KEY", "test-key");
        std::env::set_var("KIMI_AUTH_MODE", "invalid");

        let err = KimiProvider::from_env().expect_err("invalid auth mode should error");

        assert!(matches!(err, LlmError::UnsupportedAuthMode { .. }));
        clear_kimi_env();
    }

    #[test]
    fn test_kimi_model_id_default() {
        let _guard = env_lock().lock().expect("env lock");
        clear_kimi_env();
        std::env::set_var("KIMI_API_KEY", "test-key");

        let provider = KimiProvider::from_env().expect("provider should load from env");

        assert_eq!(provider.model_id(), "kimi-k2.5");
        clear_kimi_env();
    }

    #[test]
    fn test_kimi_model_id_from_env() {
        let _guard = env_lock().lock().expect("env lock");
        clear_kimi_env();
        std::env::set_var("KIMI_API_KEY", "test-key");
        std::env::set_var("KIMI_MODEL", "kimi-k2.5");

        let provider = KimiProvider::from_env().expect("provider should load from env");

        assert_eq!(provider.model_id(), "kimi-k2.5");
        clear_kimi_env();
    }

    #[test]
    fn test_kimi_from_env_missing_key() {
        let _guard = env_lock().lock().expect("env lock");
        clear_kimi_env();

        let err = KimiProvider::from_env().expect_err("missing key should error");
        assert!(matches!(err, LlmError::NotConfigured { .. }));
    }

    #[test]
    fn test_kimi_from_env_with_key() {
        let _guard = env_lock().lock().expect("env lock");
        clear_kimi_env();
        std::env::set_var("KIMI_API_KEY", "test-key");

        let provider = KimiProvider::from_env();

        assert!(provider.is_ok());
        clear_kimi_env();
    }

    #[tokio::test]
    async fn test_kimi_chat_timeout() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(100)))
            .mount(&server)
            .await;

        let provider = KimiProvider::new(
            "test-key".to_string(),
            format!("{}/v1", server.uri()),
            "kimi-k2.5".to_string(),
            Duration::from_millis(50),
        );

        let err = provider
            .chat(test_request())
            .await
            .expect_err("request should time out");

        assert!(matches!(err, LlmError::Timeout { timeout_ms: 50 }));
    }

    #[tokio::test]
    async fn test_kimi_chat_success() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .and(body_json(serde_json::json!({
                "model": "kimi-k2.5",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 128,
                "temperature": 0.2
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [
                    {
                        "message": { "content": "Hi there!" },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 20,
                    "total_tokens": 30
                }
            })))
            .mount(&server)
            .await;

        let provider = KimiProvider::new(
            "test-key".to_string(),
            format!("{}/v1", server.uri()),
            "kimi-k2.5".to_string(),
            Duration::from_secs(1),
        );

        let res = provider
            .chat(test_request())
            .await
            .expect("chat should succeed");

        assert_eq!(res.content, "Hi there!");
        assert_eq!(
            res.usage,
            TokenUsage {
                input_tokens: 10,
                output_tokens: 20,
                total_tokens: 30
            }
        );
        assert_eq!(res.finish_reason, "stop");
        assert_eq!(res.provider_id, ProviderId("kimi".to_string()));
        assert_eq!(res.model_id, ModelId("kimi-k2.5".to_string()));
    }

    #[tokio::test]
    async fn test_kimi_chat_auth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("unauthorized"))
            .mount(&server)
            .await;

        let provider = KimiProvider::new(
            "test-key".to_string(),
            format!("{}/v1", server.uri()),
            "kimi-k2.5".to_string(),
            Duration::from_secs(1),
        );

        let err = provider
            .chat(test_request())
            .await
            .expect_err("401 should map to auth error");

        assert!(matches!(err, LlmError::AuthFailed { .. }));
    }

    #[tokio::test]
    async fn test_kimi_chat_invalid_json() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string("{not-json"),
            )
            .mount(&server)
            .await;

        let provider = KimiProvider::new(
            "test-key".to_string(),
            format!("{}/v1", server.uri()),
            "kimi-k2.5".to_string(),
            Duration::from_secs(1),
        );

        let err = provider
            .chat(test_request())
            .await
            .expect_err("bad json should map to invalid response");

        assert!(matches!(err, LlmError::InvalidResponse { .. }));
    }

    #[tokio::test]
    async fn test_kimi_chat_rate_limited() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;

        let provider = KimiProvider::new(
            "test-key".to_string(),
            format!("{}/v1", server.uri()),
            "kimi-k2.5".to_string(),
            Duration::from_secs(1),
        );

        let err = provider
            .chat(test_request())
            .await
            .expect_err("429 should map to rate limit");

        assert!(matches!(err, LlmError::RateLimited));
    }

    #[tokio::test]
    async fn test_kimi_chat_coding_plan_sets_user_agent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer test-key"))
            .and(header("user-agent", "kimi-cli/1.0"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [
                    {
                        "message": { "content": "Hi there!" },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 20,
                    "total_tokens": 30
                }
            })))
            .mount(&server)
            .await;

        let provider = KimiProvider::new_with_mode(
            "test-key".to_string(),
            format!("{}/v1", server.uri()),
            "kimi-for-coding".to_string(),
            Duration::from_secs(1),
            KimiAuthMode::CodingPlan,
            Some("kimi-cli/1.0".to_string()),
        );

        let res = provider
            .chat(test_request())
            .await
            .expect("chat should succeed");
        assert_eq!(res.provider_id, ProviderId("kimi".to_string()));
    }

    #[tokio::test]
    async fn test_kimi_chat_with_tool_call_response() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [
                    {
                        "message": {
                            "content": "",
                            "tool_calls": [
                                {
                                    "id": "call_abc123",
                                    "type": "function",
                                    "function": {
                                        "name": "get_weather",
                                        "arguments": "{\"location\":\"NYC\"}"
                                    }
                                }
                            ]
                        },
                        "finish_reason": "tool_calls"
                    }
                ],
                "usage": {
                    "prompt_tokens": 15,
                    "completion_tokens": 25,
                    "total_tokens": 40
                }
            })))
            .mount(&server)
            .await;

        let provider = KimiProvider::new(
            "test-key".to_string(),
            format!("{}/v1", server.uri()),
            "kimi-k2.5".to_string(),
            Duration::from_secs(1),
        );

        let res = provider
            .chat(test_request())
            .await
            .expect("chat should succeed");

        assert_eq!(res.finish_reason, "tool_calls");
        let tool_calls = res.tool_calls.expect("tool_calls should be present");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_abc123");
        assert_eq!(tool_calls[0].name, "get_weather");
        assert_eq!(tool_calls[0].arguments, "{\"location\":\"NYC\"}");
    }

    #[tokio::test]
    async fn test_kimi_chat_rejects_malformed_tool_call_without_arguments() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [
                    {
                        "message": {
                            "content": "",
                            "tool_calls": [
                                {
                                    "id": "call_bad",
                                    "name": "get_weather"
                                }
                            ]
                        },
                        "finish_reason": "tool_calls"
                    }
                ],
                "usage": {
                    "prompt_tokens": 15,
                    "completion_tokens": 25,
                    "total_tokens": 40
                }
            })))
            .mount(&server)
            .await;

        let provider = KimiProvider::new(
            "test-key".to_string(),
            format!("{}/v1", server.uri()),
            "kimi-k2.5".to_string(),
            Duration::from_secs(1),
        );

        let err = provider
            .chat(test_request())
            .await
            .expect_err("malformed tool call should fail");

        assert!(matches!(err, LlmError::InvalidResponse { .. }));
        assert!(err
            .to_string()
            .contains("malformed Kimi tool call 'call_bad'"));
    }

    #[tokio::test]
    async fn test_kimi_chat_request_with_tools() {
        use crate::{ToolChoice, ToolDefinition};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_json(serde_json::json!({
                "model": "kimi-k2.5",
                "messages": [{"role": "user", "content": "hello"}],
                "max_tokens": 128,
                "temperature": 0.2,
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "description": "Get weather for a location",
                            "parameters": {"type": "object", "properties": {"location": {"type": "string"}}}
                        }
                    }
                ],
                "tool_choice": "auto"
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [
                    {
                        "message": { "content": "I'll check the weather." },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {
                    "prompt_tokens": 20,
                    "completion_tokens": 10,
                    "total_tokens": 30
                }
            })))
            .mount(&server)
            .await;

        let provider = KimiProvider::new(
            "test-key".to_string(),
            format!("{}/v1", server.uri()),
            "kimi-k2.5".to_string(),
            Duration::from_secs(1),
        );

        let req = LlmRequest {
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "hello".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            max_tokens: Some(128),
            temperature: Some(0.2),
            tools: Some(vec![ToolDefinition {
                name: "get_weather".to_string(),
                description: "Get weather for a location".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"location": {"type": "string"}}
                }),
            }]),
            tool_choice: Some(ToolChoice::Auto),
        };

        let res = provider.chat(req).await.expect("chat should succeed");

        assert_eq!(res.content, "I'll check the weather.");
        assert!(res.tool_calls.is_none());
    }

    #[tokio::test]
    async fn test_kimi_chat_request_with_specific_tool_choice() {
        use crate::{ToolChoice, ToolDefinition};

        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_json(serde_json::json!({
                "model": "kimi-k2.5",
                "messages": [{"role": "user", "content": "hello"}],
                "tools": [
                    {
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "description": "Get weather for a location",
                            "parameters": {"type": "object", "properties": {"location": {"type": "string"}}}
                        }
                    }
                ],
                "tool_choice": {
                    "type": "function",
                    "function": {"name": "get_weather"}
                }
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [
                    {
                        "message": { "content": "tool forced" },
                        "finish_reason": "stop"
                    }
                ],
                "usage": {
                    "prompt_tokens": 20,
                    "completion_tokens": 10,
                    "total_tokens": 30
                }
            })))
            .mount(&server)
            .await;

        let provider = KimiProvider::new(
            "test-key".to_string(),
            format!("{}/v1", server.uri()),
            "kimi-k2.5".to_string(),
            Duration::from_secs(1),
        );

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
            tools: Some(vec![ToolDefinition {
                name: "get_weather".to_string(),
                description: "Get weather for a location".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"location": {"type": "string"}}
                }),
            }]),
            tool_choice: Some(ToolChoice::Specific("get_weather".to_string())),
        };

        let res = provider.chat(req).await.expect("chat should succeed");
        assert_eq!(res.content, "tool forced");
    }

    #[tokio::test]
    async fn test_kimi_chat_rejects_required_tool_choice() {
        let provider = KimiProvider::new(
            "test-key".to_string(),
            "https://api.moonshot.ai/v1".to_string(),
            "kimi-k2.5".to_string(),
            Duration::from_secs(1),
        );

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
            tool_choice: Some(ToolChoice::Required),
        };

        let err = provider
            .chat(req)
            .await
            .expect_err("required tool_choice should fail");
        assert!(err
            .to_string()
            .contains("does not support tool_choice=required"));
    }
}

#[cfg(test)]
mod map_tests {
    use crate::{ChatMessage, ChatRole, ToolCall};

    use super::map_to_kimi_message;

    fn mk_msg(role: ChatRole, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    #[test]
    fn test_map_to_kimi_message_standard_user() {
        let msg = mk_msg(ChatRole::User, "hello");
        let wire = map_to_kimi_message(msg).expect("should map user message");
        let json = serde_json::to_value(&wire).expect("should serialize");
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "hello");
        assert!(
            json.get("tool_calls").is_none(),
            "no tool_calls field on standard message"
        );
        assert!(
            json.get("tool_call_id").is_none(),
            "no tool_call_id field on standard message"
        );
    }

    #[test]
    fn test_map_to_kimi_message_assistant_with_tool_calls() {
        let msg = ChatMessage {
            role: ChatRole::Assistant,
            content: "".to_string(),
            tool_calls: Some(vec![ToolCall {
                id: "call-1".to_string(),
                name: "fs_read".to_string(),
                arguments: "{\"path\":\"a.rs\"}".to_string(),
                execution_depth: 0,
            }]),
            tool_call_id: None,
            reasoning_content: None,
        };
        let wire = map_to_kimi_message(msg).expect("should map assistant-with-tools");
        let json = serde_json::to_value(&wire).expect("should serialize");
        assert_eq!(json["role"], "assistant");
        assert!(
            json.get("tool_calls").is_some(),
            "must carry tool_calls array"
        );
        let tc = &json["tool_calls"][0];
        assert_eq!(tc["id"], "call-1");
        assert_eq!(tc["type"], "function");
        assert_eq!(tc["function"]["name"], "fs_read");
        assert_eq!(tc["function"]["arguments"], "{\"path\":\"a.rs\"}");
    }

    #[test]
    fn test_map_to_kimi_message_tool_result() {
        let msg = ChatMessage {
            role: ChatRole::Tool,
            content: "file content".to_string(),
            tool_calls: None,
            tool_call_id: Some("call-1".to_string()),
            reasoning_content: None,
        };
        let wire = map_to_kimi_message(msg).expect("should map tool result");
        let json = serde_json::to_value(&wire).expect("should serialize");
        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "call-1");
        assert_eq!(json["content"], "file content");
        assert!(
            json.get("tool_calls").is_none(),
            "no tool_calls field on tool result"
        );
    }

    #[test]
    fn test_map_to_kimi_message_tool_result_missing_id_errors() {
        let msg = ChatMessage {
            role: ChatRole::Tool,
            content: "result".to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let err = map_to_kimi_message(msg).expect_err("missing tool_call_id must error");
        assert!(
            err.to_string().contains("tool_call_id"),
            "error message should mention tool_call_id, got: {err}"
        );
    }
}
