use std::collections::HashMap;

use crate::error::LlmError;
use crate::stream::StreamEvent;
use crate::types::{TokenUsage, ToolCall};

/// Accumulator for a single tool call being assembled from stream deltas.
#[derive(Debug, Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments_buf: String,
}

/// SSE stream parser for the OpenAI chat completions API.
///
/// OpenAI uses `data: {json}\n\n` lines with a `data: [DONE]\n\n` sentinel.
/// Each chunk carries `choices[0].delta` with optional `content` or `tool_calls`.
#[derive(Debug, Default)]
pub(crate) struct StreamParser {
    tool_calls: HashMap<usize, ToolCallAccumulator>,
}

impl StreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse one SSE data line and return zero or more stream events.
    ///
    /// `data` should be the raw JSON string from the `data:` field (without the prefix).
    pub fn parse_chunk(&mut self, data: &str) -> Vec<Result<StreamEvent, LlmError>> {
        let data = data.trim();
        if data == "[DONE]" || data.is_empty() {
            // Emit Done on the [DONE] sentinel.
            return if data == "[DONE]" {
                // Flush any incomplete tool calls before done.
                let mut events: Vec<Result<StreamEvent, LlmError>> =
                    self.drain_tool_calls().into_iter().map(Ok).collect();
                events.push(Ok(StreamEvent::Done));
                events
            } else {
                vec![]
            };
        }

        let json: serde_json::Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(e) => {
                return vec![Err(LlmError::InvalidResponse {
                    message: format!("OpenAI SSE parse error: {e}"),
                })]
            }
        };

        let mut events = Vec::new();

        // Extract top-level usage (may appear in the final non-[DONE] chunk).
        if let Some(usage_json) = json.get("usage") {
            let input = usage_json
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let output = usage_json
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            if input > 0 || output > 0 {
                events.push(Ok(StreamEvent::Usage {
                    usage: TokenUsage {
                        input_tokens: input,
                        output_tokens: output,
                        total_tokens: input + output,
                    },
                }));
            }
        }

        // Process choices[0].delta.
        let choice = json
            .get("choices")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first());

        let Some(choice) = choice else {
            return events;
        };

        let delta = choice.get("delta");
        let finish_reason = choice
            .get("finish_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if let Some(delta) = delta {
            // Content delta.
            if let Some(text) = delta.get("content").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    events.push(Ok(StreamEvent::ContentDelta {
                        text: text.to_string(),
                    }));
                }
            }

            // Tool call deltas.
            if let Some(tool_calls_arr) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                for tc in tool_calls_arr {
                    let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                    // Initial chunk for this tool call (has id + name).
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let acc = self.tool_calls.entry(index).or_default();
                        if acc.id.is_empty() {
                            acc.id = id.to_string();
                        }
                        if acc.name.is_empty() && !name.is_empty() {
                            acc.name = name.to_string();
                        }
                        if !id.is_empty() {
                            events.push(Ok(StreamEvent::ToolCallStart {
                                id: id.to_string(),
                                name: name.to_string(),
                            }));
                        }
                    }

                    // Arguments fragment.
                    if let Some(args_fragment) = tc
                        .get("function")
                        .and_then(|f| f.get("arguments"))
                        .and_then(|v| v.as_str())
                    {
                        if !args_fragment.is_empty() {
                            let acc = self.tool_calls.entry(index).or_default();
                            let id = acc.id.clone();
                            acc.arguments_buf.push_str(args_fragment);
                            events.push(Ok(StreamEvent::ToolCallDelta {
                                id,
                                arguments_fragment: args_fragment.to_string(),
                            }));
                        }
                    }
                }
            }
        }

        // When finish_reason = "tool_calls" or "stop", flush accumulated tool calls.
        if matches!(finish_reason, "tool_calls" | "stop") {
            for ev in self.drain_tool_calls() {
                events.push(Ok(ev));
            }
        }

        events
    }

    fn drain_tool_calls(&mut self) -> Vec<StreamEvent> {
        let mut calls: Vec<(usize, ToolCallAccumulator)> = self.tool_calls.drain().collect();
        calls.sort_by_key(|(idx, _)| *idx);
        calls
            .into_iter()
            .map(|(_, acc)| {
                let arguments = if acc.arguments_buf.is_empty() {
                    "{}".to_string()
                } else {
                    acc.arguments_buf
                };
                StreamEvent::ToolCallComplete {
                    id: acc.id,
                    name: acc.name,
                    arguments,
                }
            })
            .collect()
    }

    /// Convert a non-streaming OpenAI response's `tool_calls` array into [`ToolCall`]s.
    pub(crate) fn parse_tool_calls_from_response(
        tool_calls_json: &serde_json::Value,
    ) -> Vec<ToolCall> {
        let Some(arr) = tool_calls_json.as_array() else {
            return vec![];
        };
        arr.iter()
            .filter_map(|tc| {
                let id = tc.get("id")?.as_str()?.to_string();
                let func = tc.get("function")?;
                let name = func.get("name")?.as_str()?.to_string();
                let arguments = func
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}")
                    .to_string();
                Some(ToolCall {
                    id,
                    name,
                    arguments,
                    execution_depth: 0,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_stream_parser_content() {
        let mut parser = StreamParser::new();

        let events = parser.parse_chunk(
            r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"content":"Hello"}}]}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(StreamEvent::ContentDelta { text }) if text == "Hello"));

        let events = parser.parse_chunk(
            r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"content":" world"}}]}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(StreamEvent::ContentDelta { text }) if text == " world"));
    }

    #[test]
    fn test_openai_stream_parser_done_sentinel() {
        let mut parser = StreamParser::new();
        let events = parser.parse_chunk("[DONE]");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(StreamEvent::Done)));
    }

    #[test]
    fn test_openai_stream_parser_tool_call() {
        let mut parser = StreamParser::new();

        // Initial chunk with id + name.
        let events = parser.parse_chunk(
            r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_abc","function":{"name":"fs_read","arguments":""}}]}}]}"#,
        );
        assert!(events.iter().any(
            |e| matches!(e, Ok(StreamEvent::ToolCallStart { id, name }) if id == "call_abc" && name == "fs_read")
        ));

        // Arguments fragment chunks.
        let _ = parser.parse_chunk(
            r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]}}]}"#,
        );
        let _ = parser.parse_chunk(
            r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"th\":\"main.rs\"}"}}]}}]}"#,
        );

        // Finish reason flushes the tool call.
        let events = parser.parse_chunk(
            r#"{"id":"chatcmpl-1","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        );
        let complete = events
            .iter()
            .find(|e| matches!(e, Ok(StreamEvent::ToolCallComplete { .. })));
        assert!(
            complete.is_some(),
            "expected ToolCallComplete, got: {events:?}"
        );
        match complete.unwrap() {
            Ok(StreamEvent::ToolCallComplete {
                id,
                name,
                arguments,
            }) => {
                assert_eq!(id, "call_abc");
                assert_eq!(name, "fs_read");
                assert!(arguments.contains("main.rs"), "got: {arguments}");
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn test_openai_stream_parser_usage() {
        let mut parser = StreamParser::new();
        let events = parser.parse_chunk(
            r#"{"id":"chatcmpl-1","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}"#,
        );
        let usage_ev = events
            .iter()
            .find(|e| matches!(e, Ok(StreamEvent::Usage { .. })));
        assert!(usage_ev.is_some(), "expected Usage event, got: {events:?}");
    }

    #[test]
    fn test_parse_tool_calls_from_response() {
        let json = serde_json::json!([
            {
                "id": "call_abc",
                "type": "function",
                "function": {
                    "name": "fs_read",
                    "arguments": "{\"path\":\"src/main.rs\"}"
                }
            }
        ]);
        let calls = StreamParser::parse_tool_calls_from_response(&json);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_abc");
        assert_eq!(calls[0].name, "fs_read");
        assert!(calls[0].arguments.contains("main.rs"));
    }
}
