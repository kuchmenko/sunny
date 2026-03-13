use std::collections::HashMap;

use crate::error::LlmError;
use crate::stream::StreamEvent;
use crate::types::TokenUsage;

#[derive(Debug, Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments_buf: String,
}

#[derive(Debug, Default)]
pub(crate) struct StreamParser {
    tool_calls: HashMap<usize, ToolCallAccumulator>,
    block_types: HashMap<usize, String>,
}

impl StreamParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn parse_event(
        &mut self,
        event_type: &str,
        data: &str,
    ) -> Vec<Result<StreamEvent, LlmError>> {
        if data == "[DONE]" || data.is_empty() {
            return vec![];
        }

        let json: serde_json::Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(e) => {
                return vec![Err(LlmError::InvalidResponse {
                    message: format!("SSE parse error for event '{event_type}': {e}"),
                })]
            }
        };

        match event_type {
            "content_block_start" => self.handle_content_block_start(&json),
            "content_block_delta" => self.handle_content_block_delta(&json),
            "content_block_stop" => self.handle_content_block_stop(&json),
            "message_delta" => self.handle_message_delta(&json),
            "message_stop" => vec![Ok(StreamEvent::Done)],
            "error" => {
                let msg = json
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("unknown stream error")
                    .to_string();
                vec![Ok(StreamEvent::Error { message: msg })]
            }
            _ => vec![],
        }
    }

    fn handle_content_block_start(
        &mut self,
        json: &serde_json::Value,
    ) -> Vec<Result<StreamEvent, LlmError>> {
        let index = json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let block = match json.get("content_block") {
            Some(block) => block,
            None => return vec![],
        };

        let block_type = block
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        self.block_types.insert(index, block_type.clone());

        if block_type == "tool_use" {
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
            self.tool_calls.insert(
                index,
                ToolCallAccumulator {
                    id: id.clone(),
                    name: name.clone(),
                    arguments_buf: String::new(),
                },
            );
            vec![Ok(StreamEvent::ToolCallStart { id, name })]
        } else {
            vec![]
        }
    }

    fn handle_content_block_delta(
        &mut self,
        json: &serde_json::Value,
    ) -> Vec<Result<StreamEvent, LlmError>> {
        let index = json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let delta = match json.get("delta") {
            Some(delta) => delta,
            None => return vec![],
        };
        let delta_type = delta.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match delta_type {
            "text_delta" => {
                let text = delta
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if text.is_empty() {
                    vec![]
                } else {
                    vec![Ok(StreamEvent::ContentDelta { text })]
                }
            }
            "thinking_delta" => {
                let text = delta
                    .get("thinking")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if text.is_empty() {
                    vec![]
                } else {
                    vec![Ok(StreamEvent::ThinkingDelta { text })]
                }
            }
            "input_json_delta" => {
                if self.block_types.get(&index).map(String::as_str) != Some("tool_use") {
                    return vec![];
                }

                let fragment = delta
                    .get("partial_json")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if let Some(acc) = self.tool_calls.get_mut(&index) {
                    let id = acc.id.clone();
                    acc.arguments_buf.push_str(fragment);
                    vec![Ok(StreamEvent::ToolCallDelta {
                        id,
                        arguments_fragment: fragment.to_string(),
                    })]
                } else {
                    vec![]
                }
            }
            _ => vec![],
        }
    }

    fn handle_content_block_stop(
        &mut self,
        json: &serde_json::Value,
    ) -> Vec<Result<StreamEvent, LlmError>> {
        let index = json.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        self.block_types.remove(&index);

        if let Some(acc) = self.tool_calls.remove(&index) {
            // If the model called a no-argument tool, the Anthropic API sends no
            // input_json_delta events, leaving the buffer empty. Default to "{}".
            let arguments = if acc.arguments_buf.is_empty() {
                "{}".to_string()
            } else {
                acc.arguments_buf
            };
            vec![Ok(StreamEvent::ToolCallComplete {
                id: acc.id,
                name: acc.name,
                arguments,
            })]
        } else {
            vec![]
        }
    }

    fn handle_message_delta(&self, json: &serde_json::Value) -> Vec<Result<StreamEvent, LlmError>> {
        if let Some(usage_json) = json.get("usage") {
            let output_tokens = usage_json
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let usage = TokenUsage {
                input_tokens: 0,
                output_tokens: output_tokens as u32,
                total_tokens: output_tokens as u32,
            };
            vec![Ok(StreamEvent::Usage { usage })]
        } else {
            vec![]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_parser_text_delta() {
        let mut parser = StreamParser::new();
        let events = parser.parse_event(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"text","text":""}}"#,
        );
        assert!(events.is_empty() || events.iter().all(Result::is_ok));

        let events = parser.parse_event(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(StreamEvent::ContentDelta { text }) if text == "Hello"));

        let events = parser.parse_event(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"text_delta","text":" world"}}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(StreamEvent::ContentDelta { text }) if text == " world"));

        let events = parser.parse_event("message_stop", "{}");
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(StreamEvent::Done)));
    }

    #[test]
    fn test_stream_parser_tool_call_accumulation() {
        let mut parser = StreamParser::new();

        let events = parser.parse_event(
            "content_block_start",
            r#"{"index":0,"content_block":{"type":"tool_use","id":"tc_1","name":"fs_read"}}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], Ok(StreamEvent::ToolCallStart { id, name }) if id == "tc_1" && name == "fs_read")
        );

        let events = parser.parse_event(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"{\"pa"}}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Ok(StreamEvent::ToolCallDelta { .. })));

        let events = parser.parse_event(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"th\": \"main.rs\""}}"#,
        );
        assert_eq!(events.len(), 1);

        let events = parser.parse_event(
            "content_block_delta",
            r#"{"index":0,"delta":{"type":"input_json_delta","partial_json":"}"}}"#,
        );
        assert_eq!(events.len(), 1);

        let events = parser.parse_event("content_block_stop", r#"{"index":0}"#);
        assert_eq!(events.len(), 1);
        match &events[0] {
            Ok(StreamEvent::ToolCallComplete {
                id,
                name,
                arguments,
            }) => {
                assert_eq!(id, "tc_1");
                assert_eq!(name, "fs_read");
                assert!(arguments.contains("main.rs"), "got: {arguments}");
            }
            other => panic!("expected ToolCallComplete, got {other:?}"),
        }
    }

    #[test]
    fn test_stream_parser_error_event() {
        let mut parser = StreamParser::new();
        let events = parser.parse_event(
            "error",
            r#"{"error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        );
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], Ok(StreamEvent::Error { message }) if message.contains("Overloaded"))
        );
    }

    #[test]
    fn test_stream_parser_ping_ignored() {
        let mut parser = StreamParser::new();
        let events = parser.parse_event("ping", r#"{"type":"ping"}"#);
        assert!(events.is_empty());
    }
}
