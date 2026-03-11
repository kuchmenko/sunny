use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};

use crate::{LlmError, LlmProvider};
use crate::{LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage};

const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434";
const DEFAULT_OLLAMA_MODEL: &str = "qwen3.5";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

#[derive(Debug)]
pub struct OllamaProvider {
    client: reqwest::Client,
    base_url: String,
    model: String,
    timeout: Duration,
}

impl OllamaProvider {
    pub fn from_env() -> Result<Self, LlmError> {
        let base_url = std::env::var("OLLAMA_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_OLLAMA_BASE_URL.to_string())
            .trim_end_matches('/')
            .to_string();
        let model =
            std::env::var("OLLAMA_MODEL").unwrap_or_else(|_| DEFAULT_OLLAMA_MODEL.to_string());
        let timeout_ms = std::env::var("OLLAMA_TIMEOUT_MS")
            .ok()
            .and_then(|raw| raw.parse::<u64>().ok())
            .unwrap_or(DEFAULT_TIMEOUT_MS);

        Ok(Self::new(
            base_url,
            model,
            Duration::from_millis(timeout_ms),
        ))
    }

    pub fn new(base_url: String, model: String, timeout: Duration) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            timeout,
        }
    }

    fn timeout_ms(&self) -> u64 {
        self.timeout.as_millis() as u64
    }
}

#[derive(Debug, Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    format: &'static str,
    options: OllamaOptions,
}

#[derive(Debug, Serialize)]
struct OllamaMessage {
    role: &'static str,
    content: String,
}

#[derive(Debug, Serialize)]
struct OllamaOptions {
    temperature: f32,
}

#[derive(Debug, Deserialize)]
struct OllamaChatResponse {
    message: OllamaResponseMessage,
    #[serde(default)]
    eval_count: u32,
    #[serde(default)]
    prompt_eval_count: u32,
    #[serde(default)]
    done_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaResponseMessage {
    content: String,
}

fn map_message(msg: crate::ChatMessage) -> OllamaMessage {
    let role = match msg.role {
        crate::ChatRole::System => "system",
        crate::ChatRole::User => "user",
        crate::ChatRole::Assistant => "assistant",
        crate::ChatRole::Tool => "tool",
    };
    OllamaMessage {
        role,
        content: msg.content,
    }
}

#[async_trait::async_trait]
impl LlmProvider for OllamaProvider {
    fn provider_id(&self) -> &str {
        "ollama"
    }

    fn model_id(&self) -> &str {
        &self.model
    }

    async fn chat(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let payload = OllamaChatRequest {
            model: self.model.clone(),
            messages: req.messages.into_iter().map(map_message).collect(),
            stream: false,
            format: "json",
            options: OllamaOptions {
                temperature: req.temperature.unwrap_or(0.1),
            },
        };

        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .timeout(self.timeout)
            .json(&payload)
            .send()
            .await
            .map_err(|err| {
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
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(LlmError::RateLimited);
        }
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<failed to read error body>".to_string());
            return Err(LlmError::Transport {
                source: Box::new(std::io::Error::other(format!(
                    "unexpected provider status: {} body: {}",
                    status, body
                ))),
            });
        }

        let body: OllamaChatResponse =
            response
                .json()
                .await
                .map_err(|err| LlmError::InvalidResponse {
                    message: err.to_string(),
                })?;

        Ok(LlmResponse {
            content: body.message.content,
            usage: TokenUsage {
                input_tokens: body.prompt_eval_count,
                output_tokens: body.eval_count,
                total_tokens: body.prompt_eval_count + body.eval_count,
            },
            finish_reason: body.done_reason.unwrap_or_else(|| "stop".to_string()),
            provider_id: ProviderId(self.provider_id().to_string()),
            model_id: ModelId(self.model.clone()),
            tool_calls: None,
            reasoning_content: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use crate::{ChatMessage, ChatRole, LlmProvider, LlmRequest, OllamaProvider};

    fn env_lock() -> &'static Mutex<()> {
        static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn clear_ollama_env() {
        std::env::remove_var("OLLAMA_BASE_URL");
        std::env::remove_var("OLLAMA_MODEL");
        std::env::remove_var("OLLAMA_TIMEOUT_MS");
    }

    fn test_request(temperature: Option<f32>) -> LlmRequest {
        LlmRequest {
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "hello".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            max_tokens: None,
            temperature,
            tools: None,
            tool_choice: None,
        }
    }

    #[test]
    fn test_ollama_provider_from_env_defaults() {
        let _guard = env_lock().lock().expect("env lock");
        clear_ollama_env();

        let provider = OllamaProvider::from_env().expect("provider should load from env");

        assert_eq!(provider.base_url, "http://localhost:11434");
        assert_eq!(provider.model_id(), "qwen3.5");
        assert_eq!(provider.timeout, Duration::from_millis(30_000));

        clear_ollama_env();
    }

    #[test]
    fn test_ollama_provider_id() {
        let provider = OllamaProvider::new(
            "http://localhost:11434".to_string(),
            "qwen3.5".to_string(),
            Duration::from_secs(30),
        );

        assert_eq!(provider.provider_id(), "ollama");
        assert_eq!(provider.model_id(), "qwen3.5");
    }

    #[tokio::test]
    async fn test_ollama_chat_valid_response() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_json(serde_json::json!({
                "model": "qwen3.5",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": false,
                "format": "json",
                "options": {"temperature": 0.2}
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "message": {"role": "assistant", "content": "{\"answer\":\"ok\"}"},
                "eval_count": 12,
                "prompt_eval_count": 8
            })))
            .mount(&server)
            .await;

        let provider =
            OllamaProvider::new(server.uri(), "qwen3.5".to_string(), Duration::from_secs(1));

        let response = provider
            .chat(test_request(Some(0.2)))
            .await
            .expect("chat should succeed");

        assert_eq!(response.content, "{\"answer\":\"ok\"}");
        assert_eq!(response.usage.input_tokens, 8);
        assert_eq!(response.usage.output_tokens, 12);
        assert_eq!(response.usage.total_tokens, 20);
        assert_eq!(response.provider_id.0, "ollama");
    }

    #[tokio::test]
    async fn test_ollama_chat_timeout() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(100)))
            .mount(&server)
            .await;

        let provider = OllamaProvider::new(
            server.uri(),
            "qwen3.5".to_string(),
            Duration::from_millis(50),
        );

        let err = provider
            .chat(test_request(None))
            .await
            .expect_err("request should time out");

        assert!(matches!(err, crate::LlmError::Timeout { timeout_ms: 50 }));
    }

    #[tokio::test]
    async fn test_ollama_chat_connection_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
        let addr = listener.local_addr().expect("local addr");
        drop(listener);

        let provider = OllamaProvider::new(
            format!("http://{}", addr),
            "qwen3.5".to_string(),
            Duration::from_millis(200),
        );

        let err = provider
            .chat(test_request(None))
            .await
            .expect_err("connection should fail");

        assert!(matches!(err, crate::LlmError::Transport { .. }));
    }

    #[tokio::test]
    async fn test_ollama_chat_invalid_json() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(200).set_body_string("{not-json"))
            .mount(&server)
            .await;

        let provider =
            OllamaProvider::new(server.uri(), "qwen3.5".to_string(), Duration::from_secs(1));

        let err = provider
            .chat(test_request(None))
            .await
            .expect_err("bad json should map to invalid response");

        assert!(matches!(err, crate::LlmError::InvalidResponse { .. }));
    }
}
