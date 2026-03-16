//! Channel protocol types for GUI ↔ agent communication.
//!
//! `GuiToAgent` carries commands from the UI to the bridge thread.
//! `AgentToGui` carries events from the bridge thread back to the UI.
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use sunny_mind::{ChatMessage, ChatRole, StreamEvent, ToolCall};
use tokio::sync::mpsc;
use tokio::task::LocalSet;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::approval::{ApprovalRequest, GuiApprovalGate, PendingApprovals};
use sunny_boys::{AgentSession, SharedApprovalGate};
use sunny_mind::{AnthropicProvider, LlmProvider};
use sunny_store::{Database, SessionStore};

// ── GUI → Agent commands ──────────────────────────────────────────────────────

/// Commands sent from the GUI thread to the agent bridge thread.
#[derive(Debug, Clone)]
pub enum GuiToAgent {
    /// Send a user message to the LLM.
    SendMessage(String),
    /// Cancel the current streaming request.
    CancelCurrent,
    /// Create a new empty session.
    NewSession,
    /// Switch to an existing session by ID.
    SwitchSession(String),
    /// Shut down the bridge thread.
    Shutdown,
}

// ── Agent → GUI events ────────────────────────────────────────────────────────

/// Events sent from the agent bridge thread back to the GUI.
#[derive(Debug, Clone)]
pub enum AgentToGui {
    /// A streaming event from the LLM (content delta, tool call, etc.).
    StreamEvent(StreamEvent),
    /// A session was loaded — replace current message list.
    SessionLoaded {
        id: String,
        title: Option<String>,
        messages: Vec<DisplayMessage>,
    },
    /// Updated list of all saved sessions.
    SessionList(Vec<SavedSessionInfo>),
    /// A non-fatal error occurred.
    Error(String),
    /// The agent requires user approval for a capability request.
    ApprovalRequest {
        id: String,
        tool: String,
        command: String,
        reason: String,
    },
    /// An approval request was resolved (approved or denied).
    ApprovalCompleted,
    /// LLM streaming has started.
    StreamingStarted,
    /// LLM streaming has finished.
    StreamingDone,
}

// ── Display model ─────────────────────────────────────────────────────────────

/// Role of a message in the thread display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisplayRole {
    User,
    Assistant,
    System,
    Tool,
}

/// A single message for display in the thread view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayMessage {
    /// Role of the sender.
    pub role: DisplayRole,
    /// Message text content.
    pub content: String,
    /// Tool calls associated with this message (assistant messages only).
    pub tool_calls: Vec<ToolCallDisplay>,
    /// Whether this message is still streaming.
    pub is_streaming: bool,
    /// Timestamp when the message was created.
    pub timestamp: Option<DateTime<Utc>>,
}

impl DisplayMessage {
    /// Create a new user message with the current timestamp.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: DisplayRole::User,
            content: content.into(),
            tool_calls: Vec::new(),
            is_streaming: false,
            timestamp: Some(Utc::now()),
        }
    }

    /// Create a new assistant message (starts as streaming).
    pub fn assistant_streaming() -> Self {
        Self {
            role: DisplayRole::Assistant,
            content: String::new(),
            tool_calls: Vec::new(),
            is_streaming: true,
            timestamp: Some(Utc::now()),
        }
    }
}

impl From<ChatMessage> for DisplayMessage {
    fn from(msg: ChatMessage) -> Self {
        let role = match msg.role {
            ChatRole::User => DisplayRole::User,
            ChatRole::Assistant => DisplayRole::Assistant,
            ChatRole::System => DisplayRole::System,
            ChatRole::Tool => DisplayRole::Tool,
        };

        let tool_calls = msg
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(ToolCallDisplay::from)
            .collect();

        Self {
            role,
            content: msg.content,
            tool_calls,
            is_streaming: false,
            timestamp: None,
        }
    }
}

// ── Tool call display ─────────────────────────────────────────────────────────

/// A tool invocation for display in the thread view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallDisplay {
    /// Unique ID of the tool call.
    pub id: String,
    /// Tool name (e.g. "shell_exec", "fs_read").
    pub name: String,
    /// JSON-encoded arguments string.
    pub arguments: String,
    /// Result returned by the tool, if execution has completed.
    pub result: Option<String>,
    /// Whether the detail view is collapsed.
    pub collapsed: bool,
}

impl From<ToolCall> for ToolCallDisplay {
    fn from(tc: ToolCall) -> Self {
        Self {
            id: tc.id,
            name: tc.name,
            arguments: tc.arguments,
            result: None,
            collapsed: true,
        }
    }
}

// ── Session summary ───────────────────────────────────────────────────────────

/// Summary of a saved session for the session sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSessionInfo {
    /// Session UUID.
    pub id: String,
    /// Optional human-readable title.
    pub title: Option<String>,
    /// Number of messages in the session.
    pub message_count: i64,
    /// ISO 8601 timestamp of last update.
    pub updated_at: String,
}

pub fn spawn_agent_bridge(
    workspace_root: PathBuf,
    ctx: egui::Context,
) -> (
    mpsc::Sender<GuiToAgent>,
    mpsc::Receiver<AgentToGui>,
    mpsc::Receiver<ApprovalRequest>,
    PendingApprovals,
) {
    let (tx_cmd, rx_cmd) = mpsc::channel::<GuiToAgent>(32);
    let (tx_evt, rx_evt) = mpsc::channel::<AgentToGui>(64);
    let (approval_gate, approval_rx) = GuiApprovalGate::new();
    let pending_approvals = approval_gate.pending_approvals();

    std::thread::Builder::new()
        .name("agent-bridge".to_string())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build agent bridge runtime");
            let local = LocalSet::new();

            rt.block_on(local.run_until(async move {
                bridge_loop(workspace_root, ctx, tx_evt, rx_cmd, approval_gate).await;
            }));
        })
        .expect("failed to spawn agent bridge thread");

    (tx_cmd, rx_evt, approval_rx, pending_approvals)
}

async fn bridge_loop(
    workspace_root: PathBuf,
    ctx: egui::Context,
    tx: mpsc::Sender<AgentToGui>,
    mut rx: mpsc::Receiver<GuiToAgent>,
    approval_gate: GuiApprovalGate,
) {
    let provider: Arc<dyn LlmProvider> = match AnthropicProvider::new("claude-sonnet-4-6") {
        Ok(provider) => Arc::new(provider),
        Err(err) => {
            error!(error = %err, "failed to create AnthropicProvider");
            if tx
                .send(AgentToGui::Error(format!("Provider init failed: {err}")))
                .await
                .is_ok()
            {
                ctx.request_repaint();
            }
            return;
        }
    };

    let db = match Database::open_default() {
        Ok(db) => db,
        Err(err) => {
            error!(error = %err, "failed to open session DB");
            if tx
                .send(AgentToGui::Error(format!("DB open failed: {err}")))
                .await
                .is_ok()
            {
                ctx.request_repaint();
            }
            return;
        }
    };
    #[allow(clippy::arc_with_non_send_sync)]
    let store = Arc::new(SessionStore::new(db));

    {
        let tx_clone = tx.clone();
        let ctx_clone = ctx.clone();
        tokio::task::spawn_blocking(move || match Database::open_default() {
            Ok(db) => {
                let store = SessionStore::new(db);
                match store.list_sessions(None) {
                    Ok(sessions) => {
                        let summaries = sessions
                            .into_iter()
                            .map(|session| SavedSessionInfo {
                                id: session.id,
                                title: session.title,
                                message_count: 0,
                                updated_at: session.updated_at.to_rfc3339(),
                            })
                            .collect::<Vec<_>>();
                        if tx_clone
                            .blocking_send(AgentToGui::SessionList(summaries))
                            .is_ok()
                        {
                            ctx_clone.request_repaint();
                        }
                    }
                    Err(err) => warn!(error = %err, "failed to list sessions"),
                }
            }
            Err(err) => warn!(error = %err, "failed to open DB for session list"),
        });
    }

    let shared_gate: SharedApprovalGate = Arc::new(approval_gate);

    let session_id = uuid::Uuid::new_v4().to_string();
    let mut session = AgentSession::new(
        Arc::clone(&provider),
        workspace_root.clone(),
        session_id.clone(),
        Arc::clone(&store),
    )
    .with_approval_gate(Arc::clone(&shared_gate));

    if tx
        .send(AgentToGui::SessionLoaded {
            id: session_id,
            title: None,
            messages: Vec::new(),
        })
        .await
        .is_ok()
    {
        ctx.request_repaint();
    }

    while let Some(cmd) = rx.recv().await {
        match cmd {
            GuiToAgent::SendMessage(text) => {
                if tx.send(AgentToGui::StreamingStarted).await.is_ok() {
                    ctx.request_repaint();
                }

                let tx_clone = tx.clone();
                let ctx_clone = ctx.clone();
                let result = session
                    .send(&text, move |event| {
                        if tx_clone.try_send(AgentToGui::StreamEvent(event)).is_ok() {
                            ctx_clone.request_repaint();
                        }
                    })
                    .await;

                match result {
                    Ok(_) => {
                        if tx.send(AgentToGui::StreamingDone).await.is_ok() {
                            ctx.request_repaint();
                        }
                    }
                    Err(err) => {
                        if tx
                            .send(AgentToGui::Error(format!("Agent error: {err}")))
                            .await
                            .is_ok()
                        {
                            ctx.request_repaint();
                        }
                    }
                }
            }
            GuiToAgent::CancelCurrent => {
                session.cancel_current();
            }
            GuiToAgent::NewSession => {
                let new_id = uuid::Uuid::new_v4().to_string();
                session = AgentSession::new(
                    Arc::clone(&provider),
                    workspace_root.clone(),
                    new_id.clone(),
                    Arc::clone(&store),
                )
                .with_approval_gate(Arc::clone(&shared_gate));

                if tx
                    .send(AgentToGui::SessionLoaded {
                        id: new_id,
                        title: None,
                        messages: Vec::new(),
                    })
                    .await
                    .is_ok()
                {
                    ctx.request_repaint();
                }
            }
            GuiToAgent::SwitchSession(session_id) => {
                let loaded = tokio::task::spawn_blocking({
                    let session_id = session_id.clone();
                    move || -> Result<_, String> {
                        let db =
                            Database::open_default().map_err(|err| format!("DB error: {err}"))?;
                        let store = SessionStore::new(db);
                        let saved = store
                            .load_session(&session_id)
                            .map_err(|err| format!("load session error: {err}"))?;
                        let messages = store
                            .load_messages(&session_id)
                            .map_err(|err| format!("load messages error: {err}"))?;
                        Ok((saved, messages))
                    }
                })
                .await;

                match loaded {
                    Ok(Ok((Some(saved), messages))) => {
                        let display_messages = messages
                            .clone()
                            .into_iter()
                            .map(DisplayMessage::from)
                            .collect();
                        session = AgentSession::from_saved(
                            Arc::clone(&store),
                            saved.clone(),
                            messages,
                            Arc::clone(&provider),
                            workspace_root.clone(),
                            CancellationToken::new(),
                        )
                        .with_approval_gate(Arc::clone(&shared_gate));

                        if tx
                            .send(AgentToGui::SessionLoaded {
                                id: saved.id,
                                title: saved.title,
                                messages: display_messages,
                            })
                            .await
                            .is_ok()
                        {
                            ctx.request_repaint();
                        }
                    }
                    Ok(Ok((None, _))) => {
                        if tx
                            .send(AgentToGui::Error(format!("Session {session_id} not found")))
                            .await
                            .is_ok()
                        {
                            ctx.request_repaint();
                        }
                    }
                    Ok(Err(err)) => {
                        if tx.send(AgentToGui::Error(err)).await.is_ok() {
                            ctx.request_repaint();
                        }
                    }
                    Err(err) => {
                        if tx
                            .send(AgentToGui::Error(format!(
                                "failed to join session load task: {err}"
                            )))
                            .await
                            .is_ok()
                        {
                            ctx.request_repaint();
                        }
                    }
                }
            }
            GuiToAgent::Shutdown => {
                info!("bridge shutting down");
                break;
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use sunny_mind::{ChatRole, ToolCall};

    #[test]
    fn test_bridge_from_chat_message_user() {
        let msg = ChatMessage {
            role: ChatRole::User,
            content: "Hello".to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let display = DisplayMessage::from(msg);
        assert_eq!(display.role, DisplayRole::User);
        assert_eq!(display.content, "Hello");
        assert!(display.tool_calls.is_empty());
        assert!(!display.is_streaming);
    }

    #[test]
    fn test_bridge_from_chat_message_assistant() {
        let msg = ChatMessage {
            role: ChatRole::Assistant,
            content: "I can help with that.".to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let display = DisplayMessage::from(msg);
        assert_eq!(display.role, DisplayRole::Assistant);
        assert_eq!(display.content, "I can help with that.");
    }

    #[test]
    fn test_bridge_from_chat_message_system() {
        let msg = ChatMessage {
            role: ChatRole::System,
            content: "You are Sunny.".to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };
        let display = DisplayMessage::from(msg);
        assert_eq!(display.role, DisplayRole::System);
    }

    #[test]
    fn test_bridge_from_chat_message_with_tool_calls() {
        let tc = ToolCall {
            id: "call_1".to_string(),
            name: "shell_exec".to_string(),
            arguments: r#"{"command":"ls"}"#.to_string(),
            execution_depth: 0,
        };
        let msg = ChatMessage {
            role: ChatRole::Assistant,
            content: "".to_string(),
            tool_calls: Some(vec![tc]),
            tool_call_id: None,
            reasoning_content: None,
        };
        let display = DisplayMessage::from(msg);
        assert_eq!(display.tool_calls.len(), 1);
        assert_eq!(display.tool_calls[0].name, "shell_exec");
        assert!(display.tool_calls[0].collapsed);
        assert!(display.tool_calls[0].result.is_none());
    }

    #[test]
    fn test_bridge_tool_call_display_from_tool_call() {
        let tc = ToolCall {
            id: "tc_abc".to_string(),
            name: "fs_read".to_string(),
            arguments: r#"{"path":"/foo/bar"}"#.to_string(),
            execution_depth: 0,
        };
        let display = ToolCallDisplay::from(tc);
        assert_eq!(display.id, "tc_abc");
        assert_eq!(display.name, "fs_read");
        assert!(display.collapsed, "tool calls default to collapsed");
        assert!(display.result.is_none());
    }

    #[test]
    fn test_bridge_display_message_user_constructor() {
        let msg = DisplayMessage::user("test message");
        assert_eq!(msg.role, DisplayRole::User);
        assert_eq!(msg.content, "test message");
        assert!(!msg.is_streaming);
        assert!(msg.timestamp.is_some());
    }

    #[test]
    fn test_bridge_display_message_assistant_streaming() {
        let msg = DisplayMessage::assistant_streaming();
        assert_eq!(msg.role, DisplayRole::Assistant);
        assert!(msg.content.is_empty());
        assert!(msg.is_streaming);
    }

    #[test]
    fn test_bridge_all_types_derive_debug_clone() {
        let gui_to_agent = GuiToAgent::SendMessage("hello".into());
        let _cloned = gui_to_agent.clone();

        let agent_to_gui = AgentToGui::StreamingStarted;
        let _cloned = agent_to_gui.clone();

        let info = SavedSessionInfo {
            id: "sess-1".into(),
            title: Some("My Session".into()),
            message_count: 5,
            updated_at: "2026-01-01T00:00:00Z".into(),
        };
        let _cloned = info.clone();
    }
}
