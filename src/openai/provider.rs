use std::collections::{HashMap, VecDeque};

use async_trait::async_trait;
use futures::{Stream, StreamExt};
use serde_json::{json, Value};
use tkach::{
    Content, LlmProvider, Message, ProviderError, ProviderEventStream, Request, Response, Role,
    StopReason, StreamEvent, ThinkingMetadata, ThinkingProvider, ToolDefinition, Usage,
};
use tokio::sync::Mutex;

use crate::credentials::CredentialsManager;

use super::OAuthCredentials;

const CODEX_BASE_URL: &str = "https://chatgpt.com/backend-api";
const REFRESH_WINDOW_MS: u64 = 5 * 60 * 1000;
const UNAUTHORIZED: u16 = 401;

pub struct OpenAICodex {
    credentials: Mutex<OAuthCredentials>,
    credentials_manager: CredentialsManager,
    client: reqwest::Client,
}

impl OpenAICodex {
    pub fn new(credentials: OAuthCredentials, credentials_manager: CredentialsManager) -> Self {
        Self {
            credentials: Mutex::new(credentials),
            credentials_manager,
            client: reqwest::Client::new(),
        }
    }

    async fn fresh_credentials(&self) -> Result<OAuthCredentials, ProviderError> {
        let mut credentials = self.credentials.lock().await;
        if !should_refresh(&credentials) {
            return Ok(credentials.clone());
        }

        self.refresh_locked(&mut credentials).await
    }

    async fn refresh_after_unauthorized(
        &self,
        rejected_access: &str,
    ) -> Result<OAuthCredentials, ProviderError> {
        let mut credentials = self.credentials.lock().await;
        if credentials.access != rejected_access {
            return Ok(credentials.clone());
        }

        self.refresh_locked(&mut credentials).await
    }

    async fn refresh_locked(
        &self,
        credentials: &mut OAuthCredentials,
    ) -> Result<OAuthCredentials, ProviderError> {
        let refreshed = super::refresh_oauth_credentials(&self.client, &credentials.refresh)
            .await
            .map_err(|err| {
                ProviderError::Other(format!(
                    "OpenAI OAuth token refresh failed: {err}. Run .login to re-authenticate."
                ))
            })?;

        *credentials = refreshed.clone();
        self.credentials_manager
            .set_openai(refreshed.clone())
            .map_err(|err| {
                ProviderError::Other(format!(
                    "failed to save refreshed OpenAI OAuth credentials: {err}"
                ))
            })?;

        Ok(refreshed)
    }

    async fn send_codex_request(
        &self,
        body: &Value,
        credentials: &OAuthCredentials,
    ) -> Result<reqwest::Response, ProviderError> {
        Ok(self
            .client
            .post(format!("{CODEX_BASE_URL}/codex/responses"))
            .bearer_auth(&credentials.access)
            .header("chatgpt-account-id", &credentials.account_id)
            .header("originator", "sunny")
            .header("OpenAI-Beta", "responses=experimental")
            .header("accept", "text/event-stream")
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await?)
    }
}

fn should_refresh(credentials: &OAuthCredentials) -> bool {
    now_ms().saturating_add(REFRESH_WINDOW_MS) >= credentials.expires
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[async_trait]
impl LlmProvider for OpenAICodex {
    async fn complete(&self, request: Request) -> Result<Response, ProviderError> {
        let mut stream = self.stream(request).await?;
        let mut current_text = String::new();
        let mut content: Vec<Content> = Vec::new();
        let mut usage = Usage::default();
        let mut stop_reason = StopReason::EndTurn;

        while let Some(event) = stream.next().await {
            match event? {
                StreamEvent::ContentDelta(delta) => {
                    current_text.push_str(&delta);
                }
                StreamEvent::ThinkingDelta { .. } => {}
                StreamEvent::ThinkingBlock {
                    text,
                    provider,
                    metadata,
                } => {
                    push_pending_text(&mut content, &mut current_text);
                    content.push(Content::Thinking {
                        text,
                        provider,
                        metadata,
                    });
                }
                StreamEvent::ToolUse { id, name, input } => {
                    push_pending_text(&mut content, &mut current_text);
                    content.push(Content::ToolUse { id, name, input });
                }
                StreamEvent::Usage(u) => usage.merge_max(&u),
                StreamEvent::MessageDelta { stop_reason: sr } => stop_reason = sr,
                StreamEvent::ToolCallPending { .. } | StreamEvent::Done => {}
            }
        }
        push_pending_text(&mut content, &mut current_text);

        Ok(Response {
            content,
            stop_reason,
            usage,
        })
    }

    async fn stream(&self, request: Request) -> Result<ProviderEventStream, ProviderError> {
        let body = build_codex_body(&request);
        let credentials = self.fresh_credentials().await?;
        let mut response = self.send_codex_request(&body, &credentials).await?;

        if response.status().as_u16() == UNAUTHORIZED {
            let credentials = self.refresh_after_unauthorized(&credentials.access).await?;
            response = self.send_codex_request(&body, &credentials).await?;
        }

        let status = response.status().as_u16();
        if status >= 400 {
            let text = response.text().await.unwrap_or_default();
            return Err(ProviderError::Api {
                status,
                message: text,
                retryable: status == 429 || status >= 500,
            });
        }

        Ok(codex_event_stream(response.bytes_stream()))
    }
}

fn push_pending_text(content: &mut Vec<Content>, text: &mut String) {
    if !text.is_empty() {
        content.push(Content::text(std::mem::take(text)));
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

    let mut body = json!({
        "model": request.model,
        "store": false,
        "stream": true,
        "instructions": instructions,
        "input": build_codex_input(&request.messages),
        "text": { "verbosity": "low" },
        "include": ["reasoning.encrypted_content"],
    });

    let tools = build_codex_tools(&request.tools);
    if !tools.is_empty() {
        let body = body.as_object_mut().expect("body is an object");
        body.insert("tools".to_owned(), Value::Array(tools));
        body.insert("tool_choice".to_owned(), json!("auto"));
        body.insert("parallel_tool_calls".to_owned(), json!(true));
    }

    body
}

fn build_codex_input(messages: &[Message]) -> Vec<Value> {
    let mut input = Vec::new();

    for message in messages {
        match message.role {
            Role::User => push_user_message(&mut input, message),
            Role::Assistant => push_assistant_message(&mut input, message),
        }
    }

    input
}

fn push_user_message(input: &mut Vec<Value>, message: &Message) {
    let mut text = Vec::new();

    for content in &message.content {
        match content {
            Content::Text { text: chunk, .. } => text.push(chunk.as_str()),
            Content::ToolResult {
                tool_use_id,
                content,
                is_error,
                ..
            } => {
                push_text_message(input, "user", &text.join("\n"));
                text.clear();

                let output = if *is_error {
                    format!("Error: {content}")
                } else {
                    content.clone()
                };
                input.push(json!({
                    "type": "function_call_output",
                    "call_id": tool_call_id_for_output(tool_use_id),
                    "output": output,
                }));
            }
            Content::ToolUse { .. } | Content::Thinking { .. } => {}
        }
    }

    push_text_message(input, "user", &text.join("\n"));
}

fn push_assistant_message(input: &mut Vec<Value>, message: &Message) {
    let mut text = Vec::new();

    for content in &message.content {
        match content {
            Content::Text { text: chunk, .. } => text.push(chunk.as_str()),
            Content::ToolUse {
                id,
                name,
                input: args,
            } => {
                push_text_message(input, "assistant", &text.join("\n"));
                text.clear();

                let (call_id, item_id) = split_tool_use_id(id);
                let mut item = json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": name,
                    "arguments": args.to_string(),
                });
                if let Some(item_id) = item_id {
                    item.as_object_mut()
                        .expect("function call item is an object")
                        .insert("id".to_owned(), json!(item_id));
                }
                input.push(item);
            }
            Content::Thinking {
                text: summary,
                provider: ThinkingProvider::OpenAIResponses,
                metadata:
                    ThinkingMetadata::OpenAIResponses {
                        item_id,
                        encrypted_content,
                        ..
                    },
            } => {
                push_text_message(input, "assistant", &text.join("\n"));
                text.clear();
                input.push(openai_reasoning_item(
                    item_id.as_deref(),
                    summary,
                    encrypted_content.as_deref(),
                ));
            }
            Content::Thinking { .. } | Content::ToolResult { .. } => {}
        }
    }

    push_text_message(input, "assistant", &text.join("\n"));
}

fn push_text_message(input: &mut Vec<Value>, role: &str, content: &str) {
    if !content.is_empty() {
        input.push(json!({ "role": role, "content": content }));
    }
}

fn openai_reasoning_item(
    item_id: Option<&str>,
    summary: &str,
    encrypted_content: Option<&str>,
) -> Value {
    let mut item = json!({
        "type": "reasoning",
        "summary": [{ "type": "summary_text", "text": summary }],
    });
    let obj = item.as_object_mut().expect("reasoning item is an object");
    if let Some(item_id) = item_id.filter(|id| !id.is_empty()) {
        obj.insert("id".to_owned(), json!(item_id));
    }
    if let Some(encrypted_content) = encrypted_content.filter(|value| !value.is_empty()) {
        obj.insert("encrypted_content".to_owned(), json!(encrypted_content));
    }
    item
}

fn build_codex_tools(tools: &[ToolDefinition]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.input_schema,
                "strict": null,
            })
        })
        .collect()
}

fn split_tool_use_id(id: &str) -> (&str, Option<&str>) {
    id.split_once('|').map_or((id, None), |(call_id, item_id)| {
        (call_id, (!item_id.is_empty()).then_some(item_id))
    })
}

fn tool_call_id_for_output(id: &str) -> &str {
    split_tool_use_id(id).0
}

struct CodexStreamState<S> {
    byte_stream: S,
    buffer: Vec<u8>,
    parser: CodexSseParser,
    outbox: VecDeque<Result<StreamEvent, ProviderError>>,
    done: bool,
}

#[derive(Default)]
struct CodexSseParser {
    usage: Usage,
    pending_tools: HashMap<String, PendingToolCall>,
    saw_tool_use: bool,
    emitted_terminal: bool,
}

#[derive(Default)]
struct PendingToolCall {
    call_id: String,
    item_id: String,
    name: String,
    arguments: String,
}

struct ReasoningItem {
    item_id: Option<String>,
    summaries: Vec<String>,
    encrypted_content: Option<String>,
}

fn codex_event_stream<S, B>(byte_stream: S) -> ProviderEventStream
where
    S: Stream<Item = Result<B, reqwest::Error>> + Send + Unpin + 'static,
    B: AsRef<[u8]> + Send + 'static,
{
    Box::pin(futures::stream::unfold(
        CodexStreamState {
            byte_stream,
            buffer: Vec::new(),
            parser: CodexSseParser::default(),
            outbox: VecDeque::new(),
            done: false,
        },
        |mut state| async move {
            loop {
                if let Some(event) = state.outbox.pop_front() {
                    return Some((event, state));
                }

                if state.done {
                    return None;
                }

                if let Some(frame) = next_sse_frame(&mut state.buffer) {
                    match state.parser.process_frame(&frame, &mut state.outbox) {
                        Ok(terminal) => state.done = terminal,
                        Err(err) => return Some((Err(err), state)),
                    }
                    continue;
                }

                match state.byte_stream.next().await {
                    Some(Ok(bytes)) => state.buffer.extend_from_slice(bytes.as_ref()),
                    Some(Err(err)) => return Some((Err(ProviderError::Http(err)), state)),
                    None => {
                        if !state.buffer.is_empty() {
                            let frame = std::mem::take(&mut state.buffer);
                            if let Err(err) = state.parser.process_frame(&frame, &mut state.outbox)
                            {
                                return Some((Err(err), state));
                            }
                        }
                        state.parser.finish(&mut state.outbox);
                        state.done = true;
                    }
                }
            }
        },
    ))
}

impl CodexSseParser {
    fn process_frame(
        &mut self,
        frame: &[u8],
        out: &mut VecDeque<Result<StreamEvent, ProviderError>>,
    ) -> Result<bool, ProviderError> {
        let frame = String::from_utf8(frame.to_vec())
            .map_err(|err| ProviderError::Other(format!("invalid SSE UTF-8: {err}")))?;
        let data = sse_data(&frame);

        if data.is_empty() {
            return Ok(false);
        }

        if data == "[DONE]" {
            self.finish(out);
            return Ok(true);
        }

        let Ok(value) = serde_json::from_str::<Value>(&data) else {
            return Ok(false);
        };

        Ok(self.process_value(value, out))
    }

    fn process_value(
        &mut self,
        value: Value,
        out: &mut VecDeque<Result<StreamEvent, ProviderError>>,
    ) -> bool {
        match value.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    out.push_back(Ok(StreamEvent::ContentDelta(delta.to_owned())));
                }
            }
            Some("response.output_item.added") => {
                if let Some(tool) = pending_tool_from_item(value.get("item")) {
                    self.pending_tools.insert(tool.item_id.clone(), tool);
                }
            }
            Some("response.reasoning_summary_text.delta") => {
                if let Some(delta) = value.get("delta").and_then(Value::as_str) {
                    out.push_back(Ok(StreamEvent::ThinkingDelta {
                        text: delta.to_owned(),
                    }));
                }
            }
            Some("response.reasoning_summary_part.added")
            | Some("response.reasoning_text.delta") => {
                // Summary-part added carries indexes but no text. Raw reasoning_text
                // is deliberately ignored: Sunny displays provider summaries only,
                // never raw reasoning traces.
            }
            Some("response.function_call_arguments.delta") => {
                if let (Some(item_id), Some(delta)) = (
                    value.get("item_id").and_then(Value::as_str),
                    value.get("delta").and_then(Value::as_str),
                ) {
                    self.pending_tools
                        .entry(item_id.to_owned())
                        .or_insert_with(|| PendingToolCall {
                            item_id: item_id.to_owned(),
                            ..PendingToolCall::default()
                        })
                        .arguments
                        .push_str(delta);
                }
            }
            Some("response.function_call_arguments.done") => {
                if let (Some(item_id), Some(arguments)) = (
                    value.get("item_id").and_then(Value::as_str),
                    value.get("arguments").and_then(Value::as_str),
                ) {
                    let tool = self
                        .pending_tools
                        .entry(item_id.to_owned())
                        .or_insert_with(|| PendingToolCall {
                            item_id: item_id.to_owned(),
                            ..PendingToolCall::default()
                        });
                    tool.arguments = arguments.to_owned();
                    if let Some(name) = value.get("name").and_then(Value::as_str) {
                        tool.name = name.to_owned();
                    }
                }
            }
            Some("response.output_item.done") => {
                if let Some(tool) = completed_tool_call(&value, &mut self.pending_tools) {
                    self.emit_tool_use(tool, out);
                } else if let Some(reasoning) = reasoning_item(value.get("item")) {
                    self.emit_reasoning(reasoning, out);
                }
            }
            Some("response.completed") | Some("response.done") | Some("response.incomplete") => {
                self.update_usage(&value);
                self.emit_terminal(
                    value.pointer("/response/status").and_then(Value::as_str),
                    out,
                );
                return true;
            }
            Some("response.failed") => {
                let msg = value
                    .pointer("/response/error/message")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex response failed");
                out.push_back(Ok(StreamEvent::ContentDelta(format!(
                    "\n[OpenAI error: {msg}]"
                ))));
            }
            _ => {}
        }

        false
    }

    fn update_usage(&mut self, value: &Value) {
        if let Some(u) = value.get("response").and_then(|r| r.get("usage")) {
            self.usage.input_tokens =
                u.get("input_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
            self.usage.output_tokens =
                u.get("output_tokens").and_then(Value::as_u64).unwrap_or(0) as u32;
            self.usage.cache_read_input_tokens = u
                .pointer("/input_tokens_details/cached_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0) as u32;
        }
    }

    fn emit_tool_use(
        &mut self,
        tool: PendingToolCall,
        out: &mut VecDeque<Result<StreamEvent, ProviderError>>,
    ) {
        self.saw_tool_use = true;
        out.push_back(Ok(StreamEvent::ToolUse {
            id: combined_tool_use_id(&tool.call_id, &tool.item_id),
            name: tool.name,
            input: parse_tool_arguments(&tool.arguments),
        }));
    }

    fn emit_reasoning(
        &mut self,
        reasoning: ReasoningItem,
        out: &mut VecDeque<Result<StreamEvent, ProviderError>>,
    ) {
        let summaries = if reasoning.summaries.is_empty() {
            vec![String::new()]
        } else {
            reasoning.summaries
        };

        for (summary_index, text) in summaries.into_iter().enumerate() {
            out.push_back(Ok(StreamEvent::ThinkingBlock {
                text,
                provider: ThinkingProvider::OpenAIResponses,
                metadata: ThinkingMetadata::openai_responses(
                    reasoning.item_id.clone(),
                    None,
                    summary_index,
                    reasoning.encrypted_content.clone(),
                ),
            }));
        }
    }

    fn emit_terminal(
        &mut self,
        status: Option<&str>,
        out: &mut VecDeque<Result<StreamEvent, ProviderError>>,
    ) {
        if self.emitted_terminal {
            return;
        }

        out.push_back(Ok(StreamEvent::Usage(self.usage.clone())));
        out.push_back(Ok(StreamEvent::MessageDelta {
            stop_reason: codex_stop_reason(status, self.saw_tool_use),
        }));
        out.push_back(Ok(StreamEvent::Done));
        self.emitted_terminal = true;
    }

    fn finish(&mut self, out: &mut VecDeque<Result<StreamEvent, ProviderError>>) {
        if self.emitted_terminal {
            return;
        }

        for (_, tool) in std::mem::take(&mut self.pending_tools) {
            self.emit_tool_use(tool, out);
        }
        self.emit_terminal(None, out);
    }
}

fn next_sse_frame(buffer: &mut Vec<u8>) -> Option<Vec<u8>> {
    let (index, separator_len) = find_sse_separator(buffer)?;
    let frame = buffer[..index].to_vec();
    buffer.drain(..index + separator_len);
    Some(frame)
}

fn find_sse_separator(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = find_subslice(buffer, b"\n\n").map(|index| (index, 2));
    let crlf = find_subslice(buffer, b"\r\n\r\n").map(|index| (index, 4));

    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (Some(found), None) | (None, Some(found)) => Some(found),
        (None, None) => None,
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn sse_data(frame: &str) -> String {
    frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n")
}

fn pending_tool_from_item(item: Option<&Value>) -> Option<PendingToolCall> {
    let item = item?;
    (item.get("type").and_then(Value::as_str) == Some("function_call")).then(|| PendingToolCall {
        call_id: item
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        item_id: item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        name: item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        arguments: item
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    })
}

fn reasoning_item(item: Option<&Value>) -> Option<ReasoningItem> {
    let item = item?;
    (item.get("type").and_then(Value::as_str) == Some("reasoning")).then(|| {
        let summaries = item
            .get("summary")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                (entry.get("type").and_then(Value::as_str) == Some("summary_text"))
                    .then(|| entry.get("text").and_then(Value::as_str))
                    .flatten()
                    .map(str::to_owned)
            })
            .collect();

        ReasoningItem {
            item_id: item.get("id").and_then(Value::as_str).map(str::to_owned),
            summaries,
            encrypted_content: item
                .get("encrypted_content")
                .and_then(Value::as_str)
                .map(str::to_owned),
        }
    })
}

fn completed_tool_call(
    value: &Value,
    pending_tools: &mut std::collections::HashMap<String, PendingToolCall>,
) -> Option<PendingToolCall> {
    let item = value.get("item")?;
    if item.get("type").and_then(Value::as_str) != Some("function_call") {
        return None;
    }

    let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
    let mut tool = pending_tools
        .remove(item_id)
        .or_else(|| pending_tool_from_item(Some(item)))?;

    if tool.call_id.is_empty() {
        tool.call_id = item
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
    }
    if tool.item_id.is_empty() {
        tool.item_id = item_id.to_owned();
    }
    if tool.name.is_empty() {
        tool.name = item
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
    }
    if tool.arguments.is_empty() {
        tool.arguments = item
            .get("arguments")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
    }

    Some(tool)
}

fn combined_tool_use_id(call_id: &str, item_id: &str) -> String {
    if item_id.is_empty() {
        call_id.to_owned()
    } else {
        format!("{call_id}|{item_id}")
    }
}

fn parse_tool_arguments(arguments: &str) -> Value {
    if arguments.trim().is_empty() {
        return Value::Object(Default::default());
    }

    serde_json::from_str(arguments).unwrap_or_else(|_| Value::Object(Default::default()))
}

fn codex_stop_reason(status: Option<&str>, saw_tool_use: bool) -> StopReason {
    if saw_tool_use {
        return StopReason::ToolUse;
    }

    match status {
        Some("incomplete") => StopReason::MaxTokens,
        _ => StopReason::EndTurn,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::StreamExt;

    fn sse(events: Vec<Value>) -> Vec<u8> {
        events
            .into_iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>()
            .into_bytes()
    }

    async fn collect_events(events: Vec<Value>) -> Vec<StreamEvent> {
        let bytes = sse(events);
        let stream = futures::stream::iter(vec![Ok::<Vec<u8>, reqwest::Error>(bytes)]);
        let mut stream = codex_event_stream(stream);
        let mut out = Vec::new();
        while let Some(event) = stream.next().await {
            out.push(event.unwrap());
        }
        out
    }

    #[tokio::test]
    async fn reasoning_summary_delta_and_done_emit_thinking_events() {
        let events = collect_events(vec![
            json!({
                "type": "response.reasoning_summary_text.delta",
                "delta": "Inspecting files",
                "summary_index": 0
            }),
            json!({
                "type": "response.reasoning_text.delta",
                "delta": "raw hidden trace",
                "content_index": 0
            }),
            json!({
                "type": "response.output_item.done",
                "item": {
                    "type": "reasoning",
                    "id": "rs_1",
                    "summary": [{"type": "summary_text", "text": "Inspecting files"}],
                    "content": [{"type": "reasoning_text", "text": "raw hidden trace"}],
                    "encrypted_content": "enc_1"
                }
            }),
            json!({"type": "response.output_text.delta", "delta": "Done"}),
            json!({"type": "response.completed", "response": {"id": "resp_1", "status": "completed"}}),
        ]).await;

        assert!(matches!(
            &events[0],
            StreamEvent::ThinkingDelta { text } if text == "Inspecting files"
        ));
        assert!(matches!(
            &events[1],
            StreamEvent::ThinkingBlock {
                text,
                provider: ThinkingProvider::OpenAIResponses,
                metadata: ThinkingMetadata::OpenAIResponses {
                    item_id: Some(item_id),
                    output_index: None,
                    summary_index: 0,
                    encrypted_content: Some(encrypted),
                },
            } if text == "Inspecting files" && item_id == "rs_1" && encrypted == "enc_1"
        ));
        assert!(matches!(&events[2], StreamEvent::ContentDelta(text) if text == "Done"));
        assert!(matches!(&events[3], StreamEvent::Usage(_)));
        assert!(matches!(&events[4], StreamEvent::MessageDelta { .. }));
        assert!(matches!(&events[5], StreamEvent::Done));
    }

    #[test]
    fn assistant_thinking_replays_as_reasoning_item() {
        let message = Message::assistant(vec![Content::Thinking {
            text: "Checked project structure".into(),
            provider: ThinkingProvider::OpenAIResponses,
            metadata: ThinkingMetadata::openai_responses(
                Some("rs_1".into()),
                None,
                0,
                Some("enc_1".into()),
            ),
        }]);

        let input = build_codex_input(&[message]);
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["type"], "reasoning");
        assert_eq!(input[0]["id"], "rs_1");
        assert_eq!(input[0]["summary"][0]["type"], "summary_text");
        assert_eq!(input[0]["summary"][0]["text"], "Checked project structure");
        assert_eq!(input[0]["encrypted_content"], "enc_1");
    }
}
