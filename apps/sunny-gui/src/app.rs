//! SunnyApp — main eframe application state and rendering.

use chrono::Utc;
use egui::{CentralPanel, SidePanel};
use sunny_mind::StreamEvent;
use tokio::sync::mpsc;

use crate::approval::{ApprovalRequest, PendingApprovals};
use crate::bridge::{
    AgentToGui, DisplayMessage, DisplayRole, GuiToAgent, SavedSessionInfo, ToolCallDisplay,
};
use crate::widgets::session_list::render_session_sidebar;
use crate::widgets::thread_view::render_thread_view;

/// Main application state for the Sunny GUI.
#[allow(dead_code)]
pub struct SunnyApp {
    /// Receives events from the agent bridge thread.
    pub rx: mpsc::Receiver<AgentToGui>,
    /// Sends commands to the agent bridge thread.
    pub tx: mpsc::Sender<GuiToAgent>,
    /// Receives approval requests for display.
    pub approval_rx: mpsc::Receiver<ApprovalRequest>,
    /// Shared map for sending approval responses back to the bridge.
    pub pending_approvals: PendingApprovals,
    /// Thread view message list.
    pub messages: Vec<DisplayMessage>,
    /// Saved session list for sidebar.
    pub sessions: Vec<SavedSessionInfo>,
    /// Current text input from user.
    pub current_input: String,
    /// Whether the LLM is currently streaming a response.
    pub is_streaming: bool,
    /// Pending approval request to show in modal dialog.
    pub pending_approval: Option<ApprovalRequest>,
    /// Current session ID.
    pub current_session_id: Option<String>,
    /// Approximate token usage (for status bar).
    pub token_used: u32,
    /// Token budget ceiling.
    pub token_total: u32,
}

impl SunnyApp {
    /// Create a new `SunnyApp` with the given channels.
    pub fn new(
        rx: mpsc::Receiver<AgentToGui>,
        tx: mpsc::Sender<GuiToAgent>,
        approval_rx: mpsc::Receiver<ApprovalRequest>,
        pending_approvals: PendingApprovals,
    ) -> Self {
        Self {
            rx,
            tx,
            approval_rx,
            pending_approvals,
            messages: Vec::new(),
            sessions: Vec::new(),
            current_input: String::new(),
            is_streaming: false,
            pending_approval: None,
            current_session_id: None,
            token_used: 0,
            token_total: 200_000,
        }
    }

    fn handle_stream_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::ContentDelta { text } => {
                if let Some(last) = self.messages.last_mut() {
                    if last.is_streaming {
                        last.content.push_str(&text);
                    }
                }
            }
            StreamEvent::ToolCallStart { id, name } => {
                if let Some(last) = self.messages.last_mut() {
                    last.tool_calls.push(ToolCallDisplay {
                        id,
                        name,
                        arguments: String::new(),
                        result: None,
                        collapsed: false,
                    });
                }
            }
            StreamEvent::ToolCallDelta {
                id,
                arguments_fragment,
            } => {
                if let Some(last) = self.messages.last_mut() {
                    if let Some(tc) = last.tool_calls.iter_mut().find(|tc| tc.id == id) {
                        tc.arguments.push_str(&arguments_fragment);
                    }
                }
            }
            StreamEvent::ToolCallComplete {
                id,
                name: _,
                arguments,
            } => {
                if let Some(last) = self.messages.last_mut() {
                    if let Some(tc) = last.tool_calls.iter_mut().find(|tc| tc.id == id) {
                        tc.arguments = arguments;
                    }
                }
            }
            StreamEvent::ThinkingDelta { text: _ } => {}
            StreamEvent::Usage { usage } => {
                self.token_used = usage.input_tokens + usage.output_tokens;
            }
            StreamEvent::Error { message } => {
                self.messages.push(DisplayMessage {
                    role: DisplayRole::System,
                    content: format!("Error: {message}"),
                    tool_calls: Vec::new(),
                    is_streaming: false,
                    timestamp: Some(Utc::now()),
                });
                self.is_streaming = false;
            }
            StreamEvent::Done => {
                if let Some(last) = self.messages.last_mut() {
                    last.is_streaming = false;
                }
                self.is_streaming = false;
            }
        }
    }
}

impl eframe::App for SunnyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll bridge events
        while let Ok(event) = self.rx.try_recv() {
            match event {
                AgentToGui::StreamingStarted => {
                    self.is_streaming = true;
                    self.messages.push(DisplayMessage::assistant_streaming());
                }
                AgentToGui::StreamingDone => {
                    self.is_streaming = false;
                    if let Some(last) = self.messages.last_mut() {
                        last.is_streaming = false;
                    }
                }
                AgentToGui::StreamEvent(event) => {
                    self.handle_stream_event(event);
                }
                AgentToGui::SessionLoaded {
                    id,
                    title: _,
                    messages,
                } => {
                    self.current_session_id = Some(id);
                    self.messages = messages;
                }
                AgentToGui::SessionList(sessions) => {
                    self.sessions = sessions;
                }
                AgentToGui::Error(msg) => {
                    self.messages.push(DisplayMessage {
                        role: DisplayRole::System,
                        content: format!("⚠ {msg}"),
                        tool_calls: Vec::new(),
                        is_streaming: false,
                        timestamp: Some(Utc::now()),
                    });
                }
                AgentToGui::ApprovalRequest {
                    id,
                    tool,
                    command,
                    reason,
                } => {
                    self.pending_approval = Some(ApprovalRequest {
                        id,
                        tool,
                        command,
                        reason,
                    });
                }
                AgentToGui::ApprovalCompleted => {
                    self.pending_approval = None;
                }
            }
        }

        while let Ok(req) = self.approval_rx.try_recv() {
            self.pending_approval = Some(req);
        }

        // Bottom status bar (must be added BEFORE CentralPanel)
        egui::TopBottomPanel::bottom("status_bar")
            .min_height(28.0)
            .frame(egui::Frame::side_top_panel(&ctx.style()).fill(crate::theme::PANEL_BG))
            .show(ctx, |ui| {
                crate::widgets::status_bar::render_status_bar(ui, self);
            });

        SidePanel::left("sessions")
            .resizable(true)
            .min_width(180.0)
            .max_width(280.0)
            .frame(egui::Frame::side_top_panel(&ctx.style()))
            .show(ctx, |ui| {
                render_session_sidebar(ui, self);
            });

        CentralPanel::default().show(ctx, |ui| {
            render_thread_view(ui, self);
        });

        // Approval dialog (modal overlay — render last so it's on top)
        crate::widgets::approval_dialog::render_approval_dialog(ctx, self);
    }
}
