//! App state and main application struct for sunny-tui.

use crate::agent::spawn_agent_bridge;
use crate::bridge::{AgentEventReceiver, AgentMode, AgentToTui, TuiCommandSender, TuiToAgent};
use crate::thread::{Thread, ThreadMessage, ToolCallDisplay};
use crate::ui::approval::{ApprovalOverlay, PendingApproval};
use crate::ui::command_palette::CommandPalette;
use crate::ui::focus::Focus;
use crate::ui::dispatch_console::DispatchConsole;
use crate::ui::interview_card::InterviewCard;
use crate::ui::session_manager::{SessionAction, SessionManager};
use crate::ui::status_bar::StatusBar;
use crate::ui::thread_view::ThreadView;
use sunny_core::tool::InterviewAnswer;
use sunny_mind::{ChatMessage, ChatRole};
use sunny_store::{Database, SessionStore};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TryRecvError;

const DEFAULT_THREAD_VIEW_HEIGHT: u16 = 20;

/// Application runtime state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppState {
    Running,
    Quitting,
}

/// Main application struct holding all runtime state.
pub struct App {
    pub state: AppState,
    pub event_rx: Option<AgentEventReceiver>,
    pub cmd_tx: Option<TuiCommandSender>,
    pub thread: Thread,
    pub session_id: Option<String>,
    pub is_streaming: bool,
    pub tick_count: usize,
    pub focus: Focus,
    pub dispatch_console: DispatchConsole,
    pub completion_rx: Option<tokio::sync::mpsc::Receiver<String>>,
    pub thread_view: ThreadView,
    pub thread_view_height: u16,
    pub status_bar: StatusBar,
    pub pending_approval: Option<PendingApproval>,
    pub current_mode: AgentMode,
    pub command_palette: CommandPalette,
    pub session_manager: SessionManager,
    pub interview_card: Option<InterviewCard>,
    pub interview_resp_tx: Option<mpsc::Sender<Vec<InterviewAnswer>>>,
    pub workspace_dir: String,
}

impl App {
    pub fn new() -> Self {
        Self {
            state: AppState::Running,
            event_rx: None,
            cmd_tx: None,
            thread: Thread::new("New Thread"),
            session_id: None,
            is_streaming: false,
            tick_count: 0,
            focus: Focus::default(),
            dispatch_console: DispatchConsole::new(),
            completion_rx: None,
            thread_view: ThreadView::new(),
            thread_view_height: DEFAULT_THREAD_VIEW_HEIGHT,
            status_bar: StatusBar::new("claude-sonnet-4-6"),
            pending_approval: None,
            current_mode: AgentMode::Quick,
            command_palette: CommandPalette::new(),
            session_manager: SessionManager::new(String::new(), None),
            interview_card: None,
            interview_resp_tx: None,
            workspace_dir: String::new(),
        }
    }

    pub fn set_workspace_dir(&mut self, dir: String) {
        self.session_manager.workspace_dir = dir.clone();
        self.workspace_dir = dir;
    }

    pub fn set_bridge(&mut self, event_rx: AgentEventReceiver, cmd_tx: TuiCommandSender) {
        self.event_rx = Some(event_rx);
        self.cmd_tx = Some(cmd_tx);
    }

    pub fn restore_history(&mut self, messages: Vec<ChatMessage>) {
        let mut restored_any = false;

        for message in messages {
            if let Some(message) = chat_message_to_thread_message(message) {
                self.thread.messages.push(message);
                restored_any = true;
            }
        }

        if restored_any {
            self.thread.updated_at = chrono::Utc::now();
            self.thread_view.on_new_content(self.thread_view_height);
        }
    }

    pub fn send_to_agent(&mut self, cmd: TuiToAgent) {
        match &self.cmd_tx {
            None => {
                self.thread.messages.push(ThreadMessage::System {
                    content: "⚠ Not connected to agent".into(),
                    timestamp: chrono::Utc::now(),
                });
                self.thread_view.on_new_content(self.thread_view_height);
            }
            Some(tx) => {
                if let Err(err) = tx.try_send(cmd) {
                    tracing::warn!("failed to send: {err}");
                    self.thread.messages.push(ThreadMessage::System {
                        content: format!("⚠ Failed to send: {err}"),
                        timestamp: chrono::Utc::now(),
                    });
                    self.thread_view.on_new_content(self.thread_view_height);
                }
            }
        }
    }

    pub fn handle_agent_event(&mut self, event: AgentToTui) {
        match event {
            AgentToTui::StreamChunk(stream_event) => {
                self.handle_stream_chunk(stream_event);
            }
            AgentToTui::SessionReady { session_id } => {
                self.session_id = Some(session_id.clone());
                self.session_manager.active_session_id = Some(session_id);
            }
            AgentToTui::StreamingStarted => {
                self.is_streaming = true;
                self.thread.start_assistant_message();
                self.thread_view.on_new_content(self.thread_view_height);
            }
            AgentToTui::StreamingDone => {
                self.is_streaming = false;
                self.thread.finish_assistant_message();
                self.thread_view.on_new_content(self.thread_view_height);
            }
            AgentToTui::Error(msg) => {
                tracing::warn!("agent error: {msg}");
                self.is_streaming = false;
                self.thread.finish_assistant_message_with_error(&msg);
                self.thread_view.on_new_content(self.thread_view_height);
            }
            AgentToTui::ApprovalRequest {
                id,
                command,
                description,
            } => {
                tracing::info!(id = %id, "approval request received");
                self.pending_approval = Some(PendingApproval {
                    id,
                    command,
                    description,
                });
                self.focus = Focus::ApprovalOverlay;
            }
            AgentToTui::ModeChanged(mode) => {
                self.current_mode = mode.clone();
                let label = match mode {
                    AgentMode::Quick => "quick",
                    AgentMode::Smart => "smart",
                };
                self.dispatch_console.set_mode(label);
            }
            AgentToTui::InterviewRequest {
                id,
                questions,
                response_tx,
            } => {
                tracing::info!(id = %id, "interview request received");
                self.interview_card = Some(InterviewCard::new(id, questions));
                self.interview_resp_tx = Some(response_tx);
                self.focus = Focus::InterviewCard;
            }
        }
    }

    pub fn drain_agent_events(&mut self) {
        self.drain_completions();
        while let Some(rx) = self.event_rx.as_mut() {
            let event = match rx.try_recv() {
                Ok(event) => event,
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            };
            self.handle_agent_event(event);
        }
    }

    fn drain_completions(&mut self) {
        // Check completion_requested flag (stub — no provider wired yet)
        if self.dispatch_console.completion_requested {
            self.dispatch_console.completion_requested = false;
            // Future: fire Haiku API request here, pipe result to completion_rx
        }
        // Drain any pending completions from channel
        if let Some(rx) = &mut self.completion_rx {
            if let Ok(suggestion) = rx.try_recv() {
                self.dispatch_console.set_ghost_text(suggestion);
            }
        }
    }

    fn handle_stream_chunk(&mut self, event: sunny_mind::StreamEvent) {
        use sunny_mind::StreamEvent;
        let mut content_changed = false;

        match event {
            StreamEvent::ContentDelta { text } => {
                if let Some(ThreadMessage::Assistant { content, .. }) =
                    self.thread.current_assistant_message_mut()
                {
                    content.push_str(&text);
                    content_changed = true;
                }
            }
            StreamEvent::ThinkingDelta { text } => {
                if let Some(ThreadMessage::Assistant { thinking, .. }) =
                    self.thread.current_assistant_message_mut()
                {
                    thinking.push_str(&text);
                    content_changed = true;
                }
            }
            StreamEvent::ToolCallStart { id, name } => {
                if let Some(ThreadMessage::Assistant { tool_calls, .. }) =
                    self.thread.current_assistant_message_mut()
                {
                    tool_calls.push(ToolCallDisplay::new_running(id, name, ""));
                    content_changed = true;
                }
            }
            StreamEvent::ToolCallDelta {
                id,
                arguments_fragment,
            } => {
                if let Some(ThreadMessage::Assistant { tool_calls, .. }) =
                    self.thread.current_assistant_message_mut()
                {
                    if let Some(tool_call) =
                        tool_calls.iter_mut().find(|tool_call| tool_call.id == id)
                    {
                        tool_call.full_args.push_str(&arguments_fragment);
                        let preview: String = tool_call.full_args.chars().take(60).collect();
                        tool_call.args_preview = if tool_call.full_args.len() > 60 {
                            format!("{preview}…")
                        } else {
                            preview
                        };
                        content_changed = true;
                    }
                }
            }
            StreamEvent::ToolCallComplete {
                id,
                name: _,
                arguments,
            } => {
                if let Some(ThreadMessage::Assistant { tool_calls, .. }) =
                    self.thread.current_assistant_message_mut()
                {
                    if let Some(tool_call) =
                        tool_calls.iter_mut().find(|tool_call| tool_call.id == id)
                    {
                        tool_call.full_args = arguments.clone();
                        let preview: String = arguments.chars().take(60).collect();
                        tool_call.args_preview = if arguments.len() > 60 {
                            format!("{preview}…")
                        } else {
                            preview
                        };
                        content_changed = true;
                    }
                }
            }
            StreamEvent::Usage { usage } => {
                self.status_bar.usage.input = usage.input_tokens;
                self.status_bar.usage.output = usage.output_tokens;
                self.status_bar.usage.total = usage.total_tokens;
            }
            StreamEvent::Error { message } => {
                tracing::warn!("stream error: {message}");
            }
            StreamEvent::Done => {
                self.is_streaming = false;
                content_changed = true;
            }
        }

        if content_changed {
            self.thread_view.on_new_content(self.thread_view_height);
        }
    }

    pub fn draw(&mut self, frame: &mut ratatui::Frame) {
        use crate::ui::layout::centered_content;
        use crate::ui::margin;
        use ratatui::layout::{Constraint, Direction, Layout};

        let full_area = frame.area();
        let area = centered_content(full_area);

        // Render animated side margins before content so content draws on top.
        let flash_t = self.dispatch_console.send_flash_ticks as f32 / 8.0;
        margin::render_margins(
            frame,
            full_area,
            area,
            self.tick_count,
            self.is_streaming,
            self.focus == Focus::Input,
            flash_t,
        );

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),
                Constraint::Length(6),
                Constraint::Length(1),
            ])
            .split(area);

        self.thread_view_height = chunks[0].height;
        self.thread_view
            .render(frame, chunks[0], &self.thread, self.tick_count);

        self.dispatch_console.render(
            frame,
            chunks[1],
            self.focus == Focus::Input,
            self.is_streaming,
            self.tick_count,
        );

        if self.command_palette.visible {
            self.command_palette.render(frame, chunks[1]);
        }

        self.status_bar.render(
            frame,
            chunks[2],
            self.is_streaming,
            self.tick_count,
            self.session_id.as_deref(),
            &self.current_mode,
        );

        if let Some(approval) = &self.pending_approval {
            ApprovalOverlay::render(frame, approval);
        }

        if let Some(card) = &self.interview_card {
            card.render(frame, full_area);
        }

        if self.session_manager.visible {
            self.session_manager.render(frame, full_area, self.tick_count);
        }
    }

    /// Handle a terminal event.
    pub async fn handle_event(&mut self, event: crossterm::event::Event) -> anyhow::Result<()> {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let Event::Key(
            key @ KeyEvent {
                code, modifiers, ..
            },
        ) = event
        else {
            return Ok(());
        };

        // Global intercepts: always handled first.
        match (code, modifiers) {
            (KeyCode::Char('q'), KeyModifiers::CONTROL) => {
                self.state = AppState::Quitting;
                return Ok(());
            }
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                if self.focus == Focus::InterviewCard {
                    self.cancel_interview();
                } else if self.is_streaming {
                    self.send_to_agent(TuiToAgent::CancelStream);
                } else {
                    self.state = AppState::Quitting;
                }
                return Ok(());
            }
            (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
                if matches!(self.focus, Focus::Input | Focus::ThreadView) {
                    self.open_session_manager().await;
                }
                return Ok(());
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                let half = (self.thread_view_height / 2).max(1);
                self.thread_view.scroll_up(half as usize);
                return Ok(());
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                let half = (self.thread_view_height / 2).max(1);
                self.thread_view.scroll_down(half as usize, self.thread_view_height);
                return Ok(());
            }
            _ => {}
        }

        // Route to overlay handlers (they consume all keys).
        if self.focus == Focus::InterviewCard {
            return self.handle_interview_key(code, modifiers);
        }
        if self.focus == Focus::SessionManager {
            return self.handle_session_manager_key(code, modifiers).await;
        }

        // Command palette intercepts (when visible in Input focus).
        if self.focus == Focus::Input && self.command_palette.visible {
            match (code, modifiers) {
                (KeyCode::Up, _) => {
                    self.command_palette.select_prev();
                    return Ok(());
                }
                (KeyCode::Down, _) => {
                    self.command_palette.select_next();
                    return Ok(());
                }
                (KeyCode::Tab, KeyModifiers::NONE) => {
                    if let Some(cmd) = self.command_palette.selected_command() {
                        self.dispatch_console.textarea.select_all();
                        self.dispatch_console.textarea.cut();
                        self.dispatch_console.textarea.insert_str(&cmd);
                        let new_text = self.dispatch_console.textarea.lines().join("\n");
                        self.command_palette.update(&new_text);
                    }
                    return Ok(());
                }
                (KeyCode::Esc, _) => {
                    self.command_palette.hide();
                    return Ok(());
                }
                (KeyCode::Enter, KeyModifiers::NONE) => {
                    // Use highlighted command if palette has a unique match, else submit text.
                    if let Some(cmd) = self.command_palette.selected_command() {
                        if self.command_palette.visible {
                            self.command_palette.hide();
                            self.execute_slash_command(&cmd).await;
                            return Ok(());
                        }
                    }
                    // Fall through to normal Enter handling below.
                }
                _ => {}
            }
        }

        // If ghost text is visible, Tab accepts it before focus switch.
        if code == KeyCode::Tab
            && modifiers == KeyModifiers::NONE
            && self.focus == Focus::Input
            && self.dispatch_console.ghost.is_some()
        {
            self.dispatch_console.accept_ghost();
            return Ok(());
        }

        // Non-overlay global intercepts.
        match (code, modifiers) {
            (KeyCode::Tab, KeyModifiers::NONE) => {
                if self.focus != Focus::ApprovalOverlay {
                    self.focus = match self.focus {
                        Focus::Input => Focus::ThreadView,
                        _ => Focus::Input,
                    };
                }
                return Ok(());
            }
            (KeyCode::BackTab, _) => {
                if !self.is_streaming && self.focus != Focus::ApprovalOverlay {
                    let next = match self.current_mode {
                        AgentMode::Quick => AgentMode::Smart,
                        AgentMode::Smart => AgentMode::Quick,
                    };
                    self.send_to_agent(TuiToAgent::SetMode(next));
                }
                return Ok(());
            }
            (KeyCode::Esc, _) => {
                if self.focus == Focus::ApprovalOverlay {
                    self.pending_approval = None;
                    self.focus = Focus::Input;
                }
                return Ok(());
            }
            (KeyCode::Enter, KeyModifiers::NONE) if self.focus == Focus::Input => {
                if let Some(text) = self.dispatch_console.take_message() {
                    self.command_palette.hide();
                    self.execute_slash_command_or_send(text).await;
                }
                return Ok(());
            }
            (KeyCode::Char('j'), KeyModifiers::CONTROL) if self.focus == Focus::Input => {
                self.dispatch_console.textarea.insert_newline();
                return Ok(());
            }
            (KeyCode::Enter, KeyModifiers::CONTROL) if self.focus == Focus::Input => {
                self.dispatch_console.textarea.insert_newline();
                return Ok(());
            }
            _ => {}
        }

        // Per-focus dispatch.
        match self.focus {
            Focus::Input => {
                self.dispatch_console.handle_key(key);
                // Update command palette after every keystroke.
                let input_text = self.dispatch_console.textarea.lines().join("\n");
                self.command_palette.update(&input_text);
            }
            Focus::ThreadView => match code {
                KeyCode::Up => self.thread_view.scroll_up(3),
                KeyCode::Down => self.thread_view.scroll_down(3, self.thread_view_height),
                KeyCode::PageUp => self.thread_view.scroll_up(self.thread_view_height as usize),
                KeyCode::PageDown => {
                    self.thread_view
                        .scroll_down(self.thread_view_height as usize, self.thread_view_height);
                }
                KeyCode::Char('g') => self.thread_view.scroll_to_bottom(self.thread_view_height),
                _ => {}
            },
            Focus::ApprovalOverlay => {
                if let Some(approval) = &self.pending_approval {
                    let action = match code {
                        KeyCode::Char('y') => Some((approval.id.clone(), true, false)),
                        KeyCode::Char('n') => Some((approval.id.clone(), false, false)),
                        KeyCode::Char('a') => Some((approval.id.clone(), true, true)),
                        _ => None,
                    };
                    if let Some((id, approved, remember)) = action {
                        self.pending_approval = None;
                        self.focus = Focus::Input;
                        self.send_to_agent(TuiToAgent::ApprovalResponse {
                            id,
                            approved,
                            remember,
                        });
                    }
                }
            }
            Focus::InterviewCard | Focus::SessionManager => {}
        }

        Ok(())
    }

    // ── Interview helpers ─────────────────────────────────────────────────────

    fn handle_interview_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) -> anyhow::Result<()> {
        use crossterm::event::{KeyCode, KeyModifiers};

        if self.interview_card.is_none() {
            return Ok(());
        }

        // Handle keys that mutate self beyond the card borrow first.
        match code {
            KeyCode::Esc => {
                self.cancel_interview();
                return Ok(());
            }
            KeyCode::Enter => {
                let is_last = self
                    .interview_card
                    .as_ref()
                    .map(|c| c.is_last_question())
                    .unwrap_or(false);
                if is_last {
                    let answers = self
                        .interview_card
                        .as_ref()
                        .map(|c| c.collect_answers())
                        .unwrap_or_default();
                    let resp_tx = self.interview_resp_tx.take();
                    self.interview_card = None;
                    self.focus = Focus::Input;
                    if let Some(tx) = resp_tx {
                        let _ = tx.try_send(answers);
                    }
                } else if let Some(card) = self.interview_card.as_mut() {
                    card.next_question();
                }
                return Ok(());
            }
            _ => {}
        }

        // Navigation and input — borrow card briefly.
        if let Some(card) = self.interview_card.as_mut() {
            match code {
                KeyCode::Left => card.prev_question(),
                KeyCode::Right => card.next_question(),
                KeyCode::Up => card.select_prev(),
                KeyCode::Down => card.select_next(),
                KeyCode::Char(' ') => card.toggle_multi(),
                KeyCode::Backspace => card.handle_backspace(),
                KeyCode::Char(c)
                    if modifiers == KeyModifiers::NONE
                        || modifiers == KeyModifiers::SHIFT =>
                {
                    card.handle_char(c);
                }
                _ => {}
            }
        }

        Ok(())
    }

    fn cancel_interview(&mut self) {
        self.interview_card = None;
        self.interview_resp_tx = None; // drop tx → presenter gets channel-closed error
        self.focus = Focus::Input;
    }

    // ── Session manager helpers ───────────────────────────────────────────────

    async fn open_session_manager(&mut self) {
        let working_dir = if self.session_manager.global_mode {
            None
        } else {
            Some(self.workspace_dir.clone())
        };
        let sessions = tokio::task::spawn_blocking(move || {
            let db = Database::open_default().ok()?;
            let store = SessionStore::new(db);
            store.list_sessions(working_dir.as_deref()).ok()
        })
        .await
        .unwrap_or(None)
        .unwrap_or_default();

        self.session_manager.open(sessions);
        self.session_manager.visible = true;
        self.focus = Focus::SessionManager;
    }

    async fn handle_session_manager_key(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
    ) -> anyhow::Result<()> {
        let action = self.session_manager.handle_key(code, modifiers);

        match action {
            SessionAction::None => {}
            SessionAction::Open(session) => {
                // Drop current agent — bridge_loop ends when cmd_tx is dropped.
                self.event_rx = None;
                self.cmd_tx = None;
                self.is_streaming = false;

                // Clear current thread and scroll state.
                let title = session.title.clone().unwrap_or_else(|| session.id.clone());
                self.thread = Thread::new(&title);
                self.thread_view = crate::ui::thread_view::ThreadView::new();

                // Load historical messages for display.
                let sid = session.id.clone();
                let messages = tokio::task::spawn_blocking(move || {
                    let db = Database::open_default().ok()?;
                    let store = SessionStore::new(db);
                    store.load_messages(&sid).ok()
                })
                .await
                .unwrap_or(None)
                .unwrap_or_default();

                self.restore_history(messages);

                // Spawn a new agent bridge for the selected session.
                let workspace = std::path::PathBuf::from(&self.workspace_dir);
                match spawn_agent_bridge(workspace, Some(session.id.clone())) {
                    Ok((event_rx, cmd_tx)) => {
                        self.set_bridge(event_rx, cmd_tx);
                    }
                    Err(err) => {
                        tracing::warn!("failed to spawn agent for session {}: {err}", session.id);
                    }
                }

                // Close session manager.
                self.session_manager.visible = false;
                self.focus = Focus::Input;
            }
            SessionAction::LoadPreview(session_id) => {
                self.session_manager.set_preview_loading(&session_id);
                let sid = session_id.clone();
                let messages = tokio::task::spawn_blocking(move || {
                    let db = Database::open_default().ok()?;
                    let store = SessionStore::new(db);
                    store.load_messages(&sid).ok()
                })
                .await
                .unwrap_or(None)
                .unwrap_or_default();

                let total = messages.len();
                let head: Vec<_> = messages.iter().take(10).cloned().collect();
                let tail: Vec<_> = if total > 20 {
                    let skip = total.saturating_sub(10);
                    messages[skip..].to_vec()
                } else {
                    vec![]
                };
                self.session_manager.set_preview(session_id, head, tail, total);
            }
            SessionAction::RefreshList { global } => {
                let working_dir = if global {
                    None
                } else {
                    Some(self.workspace_dir.clone())
                };
                let sessions = tokio::task::spawn_blocking(move || {
                    let db = Database::open_default().ok()?;
                    let store = SessionStore::new(db);
                    store.list_sessions(working_dir.as_deref()).ok()
                })
                .await
                .unwrap_or(None)
                .unwrap_or_default();
                self.session_manager.set_sessions(sessions);
            }
            SessionAction::Delete(ids) => {
                for id in ids {
                    let sid = id.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        let db = Database::open_default().ok()?;
                        let store = SessionStore::new(db);
                        store.delete_session(&sid).ok()
                    })
                    .await;
                }
                self.refresh_session_list().await;
            }
            SessionAction::Rename(session_id, new_title) => {
                let sid = session_id.clone();
                let title = new_title.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let db = Database::open_default().ok()?;
                    let store = SessionStore::new(db);
                    store.update_title(&sid, &title).ok()
                })
                .await;
                self.refresh_session_list().await;
            }
            SessionAction::CreateNew => {
                self.session_manager.visible = false;
                self.focus = Focus::Input;
            }
        }

        if !self.session_manager.visible {
            self.focus = Focus::Input;
        }

        Ok(())
    }

    async fn refresh_session_list(&mut self) {
        let working_dir = if self.session_manager.global_mode {
            None
        } else {
            Some(self.workspace_dir.clone())
        };
        let sessions = tokio::task::spawn_blocking(move || {
            let db = Database::open_default().ok()?;
            let store = SessionStore::new(db);
            store.list_sessions(working_dir.as_deref()).ok()
        })
        .await
        .unwrap_or(None)
        .unwrap_or_default();
        self.session_manager.set_sessions(sessions);
    }

    // ── Slash command execution ───────────────────────────────────────────────

    async fn execute_slash_command_or_send(&mut self, text: String) {
        let trimmed = text.trim();
        if trimmed.starts_with('/') {
            self.execute_slash_command(trimmed).await;
        } else {
            self.thread.add_user_message(text.clone());
            self.thread_view.on_new_content(self.thread_view_height);
            self.send_to_agent(TuiToAgent::SendMessage(text));
        }
    }

    async fn execute_slash_command(&mut self, cmd: &str) {
        match cmd.trim() {
            "/exit" | "/quit" => {
                self.state = AppState::Quitting;
            }
            "/sessions" => {
                self.open_session_manager().await;
            }
            "/clear" => {
                self.thread = Thread::new("New Thread");
                self.thread_view = ThreadView::new();
            }
            "/cancel" => {
                if self.is_streaming {
                    self.send_to_agent(TuiToAgent::CancelStream);
                }
            }
            other => {
                // Send unrecognised slash commands as plain messages.
                let text = other.to_string();
                self.thread.add_user_message(text.clone());
                self.thread_view.on_new_content(self.thread_view_height);
                self.send_to_agent(TuiToAgent::SendMessage(text));
            }
        }
    }
}

fn strip_preamble(content: &str) -> &str {
    if let Some(pos) = content.find("\n\n---\n\n") {
        &content[pos + "\n\n---\n\n".len()..]
    } else {
        content
    }
}

pub fn chat_message_to_thread_message(message: ChatMessage) -> Option<ThreadMessage> {
    match message.role {
        ChatRole::User => {
            let content = strip_preamble(&message.content).to_string();
            if content.is_empty() || message.content.starts_with("[Mode Switch]") {
                return None;
            }
            Some(ThreadMessage::User {
                content,
                timestamp: chrono::Utc::now(),
            })
        }
        ChatRole::Assistant => Some(ThreadMessage::Assistant {
            content: message.content,
            thinking: message.reasoning_content.unwrap_or_default(),
            tool_calls: Vec::new(),
            timestamp: chrono::Utc::now(),
            is_streaming: false,
        }),
        ChatRole::System | ChatRole::Tool => None,
    }
}

#[cfg(test)]
mod tests {
    use super::chat_message_to_thread_message;
    use crate::thread::ThreadMessage;
    use sunny_mind::{ChatMessage, ChatRole};

    #[test]
    fn test_chat_message_to_thread_message_maps_user_and_assistant() {
        let user = chat_message_to_thread_message(ChatMessage {
            role: ChatRole::User,
            content: "hello".to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
        let assistant = chat_message_to_thread_message(ChatMessage {
            role: ChatRole::Assistant,
            content: "hi".to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: Some("thinking".to_string()),
        });

        assert!(matches!(user, Some(ThreadMessage::User { .. })));
        assert!(matches!(
            assistant,
            Some(ThreadMessage::Assistant {
                ref content,
                ref thinking,
                is_streaming: false,
                ..
            }) if content == "hi" && thinking == "thinking"
        ));
    }

    #[test]
    fn test_chat_message_to_thread_message_skips_system_and_tool() {
        let system = chat_message_to_thread_message(ChatMessage {
            role: ChatRole::System,
            content: "system".to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });
        let tool = chat_message_to_thread_message(ChatMessage {
            role: ChatRole::Tool,
            content: "tool".to_string(),
            tool_calls: None,
            tool_call_id: Some("call-1".to_string()),
            reasoning_content: None,
        });

        assert!(system.is_none());
        assert!(tool.is_none());
    }
}
