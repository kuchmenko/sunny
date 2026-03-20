//! Channel protocol types for TUI ↔ agent bridge.
//!
//! This is the TUI's own bridge protocol — NOT shared with sunny-gui.
//! AgentToTui carries events from the bridge to the TUI.
//! TuiToAgent carries commands from the TUI to the bridge.

use sunny_core::tool::{InterviewAnswer, InterviewQuestion};
use sunny_mind::StreamEvent;
use tokio::sync::mpsc;

// ── TUI-local agent mode ──────────────────────────────────────────────────────

/// Agent operating mode, mirrored locally to avoid pulling sunny_plans into
/// the TUI's bridge protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentMode {
    Quick,
    Smart,
}

// ── TUI → Agent commands ──────────────────────────────────────────────────────

/// Commands sent from the TUI to the agent bridge.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum TuiToAgent {
    /// Send a user message to the LLM.
    SendMessage(String),
    /// Cancel the current streaming request.
    CancelStream,
    /// Respond to an approval request.
    ApprovalResponse {
        /// ID of the approval request being responded to.
        id: String,
        /// Whether the user approved the tool call.
        approved: bool,
        /// Whether to always approve this tool type.
        remember: bool,
    },
    /// Switch the agent operating mode.
    SetMode(AgentMode),
}

// ── Agent → TUI events ────────────────────────────────────────────────────────

/// Events sent from the agent bridge to the TUI.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum AgentToTui {
    /// A streaming event from the LLM (content delta, tool call, etc.).
    StreamChunk(StreamEvent),
    /// Agent session is ready.
    SessionReady {
        /// Session ID.
        session_id: String,
    },
    /// A non-fatal error occurred.
    Error(String),
    /// The agent requires user approval for a capability request.
    ApprovalRequest {
        /// Unique approval request ID.
        id: String,
        /// The shell command or operation requiring approval.
        command: String,
        /// Human-readable description of why approval is needed.
        description: String,
    },
    /// Streaming started.
    StreamingStarted,
    /// Streaming finished.
    StreamingDone,
    /// The agent mode was changed successfully.
    ModeChanged(AgentMode),
    /// The agent is requesting user input via the interview tool.
    InterviewRequest {
        /// Unique request ID (for logging/correlation).
        id: String,
        /// Questions to present to the user.
        questions: Vec<InterviewQuestion>,
        /// Channel to send answers back through.
        response_tx: mpsc::Sender<Vec<InterviewAnswer>>,
    },
}

// ── Channel type aliases ──────────────────────────────────────────────────────

/// Sender for commands from TUI → agent.
#[allow(dead_code)]
pub type TuiCommandSender = mpsc::Sender<TuiToAgent>;
/// Receiver for commands from TUI → agent.
#[allow(dead_code)]
pub type TuiCommandReceiver = mpsc::Receiver<TuiToAgent>;
/// Sender for events from agent → TUI.
#[allow(dead_code)]
pub type AgentEventSender = mpsc::Sender<AgentToTui>;
/// Receiver for events from agent → TUI.
#[allow(dead_code)]
pub type AgentEventReceiver = mpsc::Receiver<AgentToTui>;

// ── StreamEvent → thread model helpers ───────────────────────────────────────

/// Map a StreamEvent to a brief description for logging/debug.
#[allow(dead_code)]
pub fn stream_event_kind(event: &StreamEvent) -> &'static str {
    match event {
        StreamEvent::ContentDelta { .. } => "content_delta",
        StreamEvent::ThinkingDelta { .. } => "thinking_delta",
        StreamEvent::ToolCallStart { .. } => "tool_call_start",
        StreamEvent::ToolCallDelta { .. } => "tool_call_delta",
        StreamEvent::ToolCallComplete { .. } => "tool_call_complete",
        StreamEvent::Usage { .. } => "usage",
        StreamEvent::Error { .. } => "error",
        StreamEvent::Done => "done",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sunny_mind::TokenUsage;

    #[test]
    fn test_tui_to_agent_send_message_constructs() {
        let cmd = TuiToAgent::SendMessage("hello".into());
        assert!(matches!(cmd, TuiToAgent::SendMessage(_)));
    }

    #[test]
    fn test_tui_to_agent_all_variants_clone() {
        let _ = TuiToAgent::SendMessage("x".into()).clone();
        let _ = TuiToAgent::CancelStream.clone();
        let _ = TuiToAgent::ApprovalResponse {
            id: "id".into(),
            approved: true,
            remember: false,
        }
        .clone();
        let _ = TuiToAgent::SetMode(AgentMode::Quick).clone();
        let _ = TuiToAgent::SetMode(AgentMode::Smart).clone();
    }

    #[test]
    fn test_agent_to_tui_all_variants_clone() {
        let event = StreamEvent::ContentDelta { text: "hi".into() };
        let _ = AgentToTui::StreamChunk(event).clone();
        let _ = AgentToTui::SessionReady {
            session_id: "s1".into(),
        }
        .clone();
        let _ = AgentToTui::Error("err".into()).clone();
        let _ = AgentToTui::ApprovalRequest {
            id: "r1".into(),
            command: "rm -rf /".into(),
            description: "destructive".into(),
        }
        .clone();
        let _ = AgentToTui::StreamingStarted.clone();
        let _ = AgentToTui::StreamingDone.clone();
        let _ = AgentToTui::ModeChanged(AgentMode::Quick).clone();
        let _ = AgentToTui::ModeChanged(AgentMode::Smart).clone();
    }

    #[test]
    fn test_agent_mode_equality() {
        assert_eq!(AgentMode::Quick, AgentMode::Quick);
        assert_eq!(AgentMode::Smart, AgentMode::Smart);
        assert_ne!(AgentMode::Quick, AgentMode::Smart);
    }

    #[test]
    fn test_stream_event_kind_returns_correct_label() {
        assert_eq!(
            stream_event_kind(&StreamEvent::ContentDelta { text: "".into() }),
            "content_delta"
        );
        assert_eq!(
            stream_event_kind(&StreamEvent::ThinkingDelta { text: "".into() }),
            "thinking_delta"
        );
        assert_eq!(stream_event_kind(&StreamEvent::Done), "done");
        assert_eq!(
            stream_event_kind(&StreamEvent::ToolCallStart {
                id: "".into(),
                name: "".into()
            }),
            "tool_call_start"
        );
        assert_eq!(
            stream_event_kind(&StreamEvent::Usage {
                usage: TokenUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0
                }
            }),
            "usage"
        );
    }

    #[test]
    fn test_channel_type_aliases_compile() {
        let (tx, _rx): (TuiCommandSender, TuiCommandReceiver) = tokio::sync::mpsc::channel(1);
        let (_, _): (AgentEventSender, AgentEventReceiver) = tokio::sync::mpsc::channel(1);
        drop(tx);
    }
}
