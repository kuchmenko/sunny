use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{json, Value};
use tkach::{
    Content, LlmProvider, ProviderError, ProviderEventStream, Request, Response, Role, StopReason,
    StreamEvent, Usage,
};

use super::OAuthCredentials;

const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";

pub struct OpenAICodex {
    credentials: OAuthCredentials,
    client: reqwest::Client,
}

impl OpenAICodex {
    pub fn new(credentials: OAuthCredentials) -> Self {
        Self {
            credentials,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAICodex {
    async fn complete(&self, request: Request) -> Result<Response, ProviderError> {
        let mut stream = self.stream(request).await?;
        let mut text = String::new();
        let mut usage = Usage::default();
        let mut stop_reason = StopReason::EndTurn;

        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::ContentDelta(delta) => text.push_str(&delta),
                StreamEvent::Usage(u) => usage.merge_max(&u),
                StreamEvent::MessageDelta { stop_reason: sr } => stop_reason = sr,
                _ => {}
            }
        }

        Ok(Response {
            content: vec![Content::text(text)],
            stop_reason,
            usage,
        })
    }

    async fn stream(&self, request: Request) -> Result<ProviderEventStream, ProviderError> {
        let body = build_codex_body(&request);
        let response = self
            .client
            .post(format!("{CODEX_BASE_URL}/codex/responses"))
            .bearer_auth(&self.credentials.access)
            .header("chatgpt-account-id", &self.credentials.account_id)
            .header("originator", "sunny")
            .header("OpenAI-Beta", "responses=experimental")
            .header("accept", "text/event-stream")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = response.status().as_u16();
        if status >= 400 {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status,
                message: text,
                retryable: status == 429 || status >= 500,
            });
        }

        let mut byte_stream = response.bytes_stream();
        let mut raw = String::new();
        while let Some(chunk) = byte_stream.next().await {
            let bytes = chunk.map_err(ProviderError::Http)?;
            raw.push_str(&String::from_utf8_lossy(&bytes));
        }

        let events = parse_sse_events(&raw);
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

fn build_codex_body(request: &Request) -> Value {
    let instructions = request
        .system
        .as_ref()
        .map(|blocks| {
            blocks
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default();

    let input: Vec<Value> = request
        .messages
        .iter()
        .map(|message| {
            let role = match message.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            };
            let content = message
                .content
                .iter()
                .filter_map(|content| match content {
                    Content::Text { text, .. } => Some(text.as_str()),
                    Content::ToolResult { content, .. } => Some(content.as_str()),
                    Content::ToolUse { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            json!({ "role": role, "content": content })
        })
        .collect();

    json!({
        "model": request.model,
        "store": false,
        "stream": true,
        "instructions": instructions,
        "input": input,
        "text": { "verbosity": "low" },
        "include": ["reasoning.encrypted_content"],
    })
}

fn parse_sse_events(raw: &str) -> Vec<StreamEvent> {
    let mut out = Vec::new();
    let mut usage = Usage::default();

    for chunk in raw.split("\n\n") {
        let data = chunk
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim)
            .collect::<Vec<_>>()
            .join("\n");

        if data.is_empty() || data == "[DONE]" {
            continue;
        }

        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            continue;
        };

        match value.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    out.push(StreamEvent::ContentDelta(delta.to_owned()));
                }
            }
            Some("response.completed") | Some("response.done") | Some("response.incomplete") => {
                if let Some(u) = value.get("response").and_then(|r| r.get("usage")) {
                    usage.input_tokens =
                        u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
                    usage.output_tokens =
                        u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
                }
                out.push(StreamEvent::Usage(usage.clone()));
                out.push(StreamEvent::MessageDelta {
                    stop_reason: StopReason::EndTurn,
                });
                out.push(StreamEvent::Done);
            }
            Some("response.failed") => {
                let msg = value
                    .pointer("/response/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex response failed");
                out.push(StreamEvent::ContentDelta(format!(
                    "\n[OpenAI error: {msg}]"
                )));
            }
            _ => {}
        }
    }

    out
}
