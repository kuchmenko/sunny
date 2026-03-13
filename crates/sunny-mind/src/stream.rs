use std::pin::Pin;

use futures_core::Stream;

use crate::error::LlmError;
use crate::types::TokenUsage;

/// A single event in a streaming LLM response.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A text fragment from the model
    ContentDelta { text: String },
    /// A tool call has started (name and ID known, arguments streaming)
    ToolCallStart { id: String, name: String },
    /// A partial JSON fragment for tool call arguments (provider accumulates internally)
    ToolCallDelta {
        id: String,
        arguments_fragment: String,
    },
    /// Full tool call with accumulated arguments (provider-side accumulation complete)
    ToolCallComplete {
        id: String,
        name: String,
        arguments: String,
    },
    /// A reasoning/thinking content fragment
    ThinkingDelta { text: String },
    /// Token usage from message_delta event
    Usage { usage: TokenUsage },
    /// Stream error event
    Error { message: String },
    /// Stream complete
    Done,
}

/// A streaming result from an LLM provider.
///
/// Items are `Result<StreamEvent, LlmError>` — the stream can fail mid-flight.
pub type StreamResult = Pin<Box<dyn Stream<Item = Result<StreamEvent, LlmError>> + Send>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_event_debug() {
        let e = StreamEvent::ContentDelta {
            text: "hello".to_string(),
        };
        let s = format!("{e:?}");
        assert!(s.contains("ContentDelta"));
        assert!(s.contains("hello"));
    }

    #[test]
    fn test_stream_event_all_variants_construct() {
        // Verify all 8 variants can be constructed
        let _ = StreamEvent::ContentDelta { text: "t".into() };
        let _ = StreamEvent::ToolCallStart {
            id: "1".into(),
            name: "f".into(),
        };
        let _ = StreamEvent::ToolCallDelta {
            id: "1".into(),
            arguments_fragment: "{".into(),
        };
        let _ = StreamEvent::ToolCallComplete {
            id: "1".into(),
            name: "f".into(),
            arguments: "{}".into(),
        };
        let _ = StreamEvent::ThinkingDelta { text: "t".into() };
        let _ = StreamEvent::Usage {
            usage: TokenUsage {
                input_tokens: 0,
                output_tokens: 0,
                total_tokens: 0,
            },
        };
        let _ = StreamEvent::Error {
            message: "e".into(),
        };
        let _ = StreamEvent::Done;
    }
}
