//! Session manager overlay — telescope-style session browser.
//!
//! Full-screen overlay with a filterable list of sessions on the left
//! and a message preview on the right. Opens via Ctrl+S or /sessions.

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use sunny_mind::{ChatMessage, ChatRole};
use sunny_store::SavedSession;

use crate::ui::theme;

// ── Internal focus ──────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
enum PaneFocus {
    Filter,
    List,
    Preview,
}

// ── Public action type ──────────────────────────────────────────────────────

/// Action requested by the session manager for the app to fulfill.
#[derive(Debug)]
pub enum SessionAction {
    None,
    Open(SavedSession),
    LoadPreview(String),          // session_id
    RefreshList { global: bool }, // reload after delete / rename / mode switch
    Delete(Vec<String>),          // Vec<session_id>
    Rename(String, String),       // (session_id, new_title)
    CreateNew,
}

// ── Internal data ───────────────────────────────────────────────────────────

struct PreviewData {
    #[allow(dead_code)]
    session_id: String,
    head: Vec<ChatMessage>,
    tail: Vec<ChatMessage>,
    total: usize,
}

struct RenameModal {
    session_idx: usize,
    title: String,
}

// ── Main widget ─────────────────────────────────────────────────────────────

pub struct SessionManager {
    pub visible: bool,
    sessions: Vec<SavedSession>,
    pub workspace_dir: String,
    pub active_session_id: Option<String>,
    pub global_mode: bool,
    // list pane
    cursor: usize,
    selected: HashSet<usize>, // indices into filtered_indices
    // filter
    filter: String,
    filtered_indices: Vec<usize>, // indices into sessions
    // preview pane
    preview: Option<PreviewData>,
    preview_scroll: usize,
    preview_loading: bool,
    // focus
    pane_focus: PaneFocus,
    // delete confirmation
    confirm_delete_idx: Option<usize>, // index in filtered_indices, needs second `d`
    // sub-modals
    rename_modal: Option<RenameModal>,
    batch_delete_confirm: bool, // D pressed while items selected
}

impl SessionManager {
    pub fn new(workspace_dir: String, active_session_id: Option<String>) -> Self {
        Self {
            visible: false,
            sessions: Vec::new(),
            workspace_dir,
            active_session_id,
            global_mode: false,
            cursor: 0,
            selected: HashSet::new(),
            filter: String::new(),
            filtered_indices: Vec::new(),
            preview: None,
            preview_scroll: 0,
            preview_loading: false,
            pane_focus: PaneFocus::List,
            confirm_delete_idx: None,
            rename_modal: None,
            batch_delete_confirm: false,
        }
    }

    /// Called when the overlay is opened — resets transient state and loads sessions.
    pub fn open(&mut self, sessions: Vec<SavedSession>) {
        self.sessions = sessions;
        self.filter.clear();
        self.cursor = 0;
        self.selected.clear();
        self.preview = None;
        self.preview_scroll = 0;
        self.preview_loading = false;
        self.pane_focus = PaneFocus::List;
        self.confirm_delete_idx = None;
        self.rename_modal = None;
        self.batch_delete_confirm = false;
        self.apply_filter();
    }

    /// Replace session list after an async refresh.
    pub fn set_sessions(&mut self, sessions: Vec<SavedSession>) {
        self.sessions = sessions;
        self.apply_filter();
    }

    pub fn set_preview(
        &mut self,
        session_id: String,
        head: Vec<ChatMessage>,
        tail: Vec<ChatMessage>,
        total: usize,
    ) {
        self.preview = Some(PreviewData { session_id, head, tail, total });
        self.preview_loading = false;
        self.preview_scroll = 0;
    }

    pub fn set_preview_loading(&mut self, session_id: &str) {
        if self.current_session().map(|s| s.id.as_str()) == Some(session_id) {
            self.preview_loading = true;
            self.preview = None;
        }
    }

    // ── Key handling ─────────────────────────────────────────────────────────

    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> SessionAction {
        // Rename modal takes priority over everything
        if self.rename_modal.is_some() {
            return self.handle_rename_key(code, modifiers);
        }

        // Batch delete confirm
        if self.batch_delete_confirm {
            return self.handle_batch_delete_key(code);
        }

        match self.pane_focus {
            PaneFocus::Filter => self.handle_filter_key(code, modifiers),
            PaneFocus::List => self.handle_list_key(code, modifiers),
            PaneFocus::Preview => self.handle_preview_key(code),
        }
    }

    fn handle_rename_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> SessionAction {
        match code {
            KeyCode::Esc => {
                self.rename_modal = None;
                SessionAction::None
            }
            KeyCode::Enter => {
                if let Some(modal) = self.rename_modal.take() {
                    if let Some(&session_idx) = self.filtered_indices.get(modal.session_idx) {
                        if let Some(session) = self.sessions.get(session_idx) {
                            return SessionAction::Rename(session.id.clone(), modal.title);
                        }
                    }
                }
                SessionAction::None
            }
            KeyCode::Backspace => {
                if let Some(modal) = &mut self.rename_modal {
                    modal.title.pop();
                }
                SessionAction::None
            }
            KeyCode::Char(c)
                if modifiers == KeyModifiers::NONE || modifiers == KeyModifiers::SHIFT =>
            {
                if let Some(modal) = &mut self.rename_modal {
                    modal.title.push(c);
                }
                SessionAction::None
            }
            _ => SessionAction::None,
        }
    }

    fn handle_batch_delete_key(&mut self, code: KeyCode) -> SessionAction {
        match code {
            KeyCode::Char('d') => {
                let ids: Vec<String> = self
                    .selected
                    .iter()
                    .filter_map(|&i| self.filtered_indices.get(i))
                    .filter_map(|&idx| self.sessions.get(idx))
                    .map(|s| s.id.clone())
                    .collect();
                self.selected.clear();
                self.batch_delete_confirm = false;
                SessionAction::Delete(ids)
            }
            KeyCode::Esc => {
                self.batch_delete_confirm = false;
                SessionAction::None
            }
            _ => SessionAction::None,
        }
    }

    fn handle_filter_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> SessionAction {
        match code {
            KeyCode::Esc => {
                self.filter.clear();
                self.apply_filter();
                self.pane_focus = PaneFocus::List;
                SessionAction::None
            }
            KeyCode::Enter | KeyCode::Tab => {
                self.pane_focus = PaneFocus::List;
                SessionAction::None
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.apply_filter();
                SessionAction::None
            }
            KeyCode::Char(c)
                if modifiers == KeyModifiers::NONE || modifiers == KeyModifiers::SHIFT =>
            {
                self.filter.push(c);
                self.apply_filter();
                SessionAction::None
            }
            _ => SessionAction::None,
        }
    }

    fn handle_list_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> SessionAction {
        match code {
            KeyCode::Esc => {
                self.visible = false;
                SessionAction::None
            }
            KeyCode::Tab => {
                self.pane_focus = PaneFocus::Preview;
                SessionAction::None
            }
            KeyCode::Char('/' | 'i') if modifiers == KeyModifiers::NONE => {
                self.pane_focus = PaneFocus::Filter;
                SessionAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor_down();
                self.load_preview_action()
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor_up();
                self.load_preview_action()
            }
            KeyCode::Enter => {
                if let Some(session) = self.current_session() {
                    SessionAction::Open(session.clone())
                } else {
                    SessionAction::None
                }
            }
            KeyCode::Char(' ') => {
                let cursor = self.cursor;
                if self.selected.contains(&cursor) {
                    self.selected.remove(&cursor);
                } else {
                    self.selected.insert(cursor);
                }
                self.cursor_down();
                SessionAction::None
            }
            KeyCode::Char('d') => {
                if self.confirm_delete_idx == Some(self.cursor) {
                    if let Some(session) = self.current_session() {
                        let id = session.id.clone();
                        self.confirm_delete_idx = None;
                        return SessionAction::Delete(vec![id]);
                    }
                    self.confirm_delete_idx = None;
                    SessionAction::None
                } else {
                    self.confirm_delete_idx = Some(self.cursor);
                    SessionAction::None
                }
            }
            KeyCode::Char('D') => {
                if !self.selected.is_empty() {
                    self.batch_delete_confirm = true;
                    SessionAction::None
                } else {
                    // Treat as single delete
                    if self.confirm_delete_idx == Some(self.cursor) {
                        if let Some(session) = self.current_session() {
                            let id = session.id.clone();
                            self.confirm_delete_idx = None;
                            return SessionAction::Delete(vec![id]);
                        }
                        self.confirm_delete_idx = None;
                    } else {
                        self.confirm_delete_idx = Some(self.cursor);
                    }
                    SessionAction::None
                }
            }
            KeyCode::Char('r') => {
                if let Some(session) = self.current_session() {
                    let title = session.title.clone().unwrap_or_default();
                    let session_idx = self.cursor;
                    self.rename_modal = Some(RenameModal { session_idx, title });
                }
                SessionAction::None
            }
            KeyCode::Char('n') => SessionAction::CreateNew,
            KeyCode::Char('g') => {
                self.global_mode = !self.global_mode;
                SessionAction::RefreshList { global: self.global_mode }
            }
            _ => SessionAction::None,
        }
    }

    fn handle_preview_key(&mut self, code: KeyCode) -> SessionAction {
        match code {
            KeyCode::Esc | KeyCode::Tab => {
                self.pane_focus = PaneFocus::List;
                SessionAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.preview_scroll = self.preview_scroll.saturating_add(1);
                SessionAction::None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1);
                SessionAction::None
            }
            _ => SessionAction::None,
        }
    }

    fn load_preview_action(&self) -> SessionAction {
        if let Some(s) = self.current_session() {
            SessionAction::LoadPreview(s.id.clone())
        } else {
            SessionAction::None
        }
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    pub fn render(&self, frame: &mut Frame, area: Rect, tick_count: usize) {
        let overlay_area = Self::overlay_rect(area);

        // Dim background
        frame.render_widget(
            Block::default()
                .style(Style::default().bg(Color::Black).add_modifier(Modifier::DIM)),
            area,
        );
        frame.render_widget(Clear, overlay_area);

        // Outer container block
        let session_count = self.filtered_indices.len();
        let scope = self.scope_label();
        let outer_title = format!(" sessions · {} in {} ", session_count, scope);
        let global_style = if self.global_mode { theme::accent() } else { theme::hint() };

        let outer_block = Block::default()
            .title(Span::styled(outer_title, Style::default().fg(theme::CREAM)))
            .title_top(
                Line::from(Span::styled(" [g]lobal ", global_style)).right_aligned(),
            )
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme::border_unfocused());

        let inner = outer_block.inner(overlay_area);
        frame.render_widget(outer_block, overlay_area);

        // Split inner: [content(*), footer(1)]
        let vchunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(1)])
            .split(inner);

        // Footer (or batch-delete confirm)
        if self.batch_delete_confirm {
            let msg = format!(
                " Delete {} selected? Press d to confirm, Esc to cancel ",
                self.selected.len()
            );
            frame.render_widget(
                Paragraph::new(Span::styled(
                    msg,
                    Style::default()
                        .fg(theme::SIGNAL_RED)
                        .add_modifier(Modifier::BOLD),
                )),
                vchunks[1],
            );
        } else {
            self.render_footer(frame, vchunks[1]);
        }

        // Split content: [left(36), right(*)]
        let hchunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(36), Constraint::Min(10)])
            .split(vchunks[0]);

        self.render_list_pane(frame, hchunks[0]);
        self.render_preview_pane(frame, hchunks[1], tick_count);

        // Rename modal rendered on top
        if let Some(modal) = &self.rename_modal {
            self.render_rename_modal(frame, overlay_area, modal);
        }
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        let spans = vec![
            Span::styled("enter", Style::default().fg(theme::SUNNY_GOLD)),
            Span::styled(" open", theme::hint()),
            Span::raw(" · "),
            Span::styled("n", Style::default().fg(theme::SUNNY_GOLD)),
            Span::styled(" new", theme::hint()),
            Span::raw(" · "),
            Span::styled("r", Style::default().fg(theme::SUNNY_GOLD)),
            Span::styled(" rename", theme::hint()),
            Span::raw(" · "),
            Span::styled("d", Style::default().fg(theme::SUNNY_GOLD)),
            Span::styled(" del", theme::hint()),
            Span::raw(" · "),
            Span::styled("space", Style::default().fg(theme::SUNNY_GOLD)),
            Span::styled(" select", theme::hint()),
            Span::raw(" · "),
            Span::styled("D", Style::default().fg(theme::SUNNY_GOLD)),
            Span::styled(" del sel", theme::hint()),
            Span::raw(" · "),
            Span::styled("tab", Style::default().fg(theme::SUNNY_GOLD)),
            Span::styled(" switch pane", theme::hint()),
        ];
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
    }

    fn render_list_pane(&self, frame: &mut Frame, area: Rect) {
        let is_focused = matches!(self.pane_focus, PaneFocus::Filter | PaneFocus::List);
        let border_style =
            if is_focused { theme::border_focused() } else { theme::border_unfocused() };

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let vchunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(1)])
            .split(inner);

        self.render_filter(frame, vchunks[0]);
        self.render_list(frame, vchunks[1]);
    }

    fn render_filter(&self, frame: &mut Frame, area: Rect) {
        let is_focused = self.pane_focus == PaneFocus::Filter;
        let border_style =
            if is_focused { theme::border_focused() } else { theme::border_unfocused() };

        let content = if self.filter.is_empty() && !is_focused {
            Line::from(Span::styled("/ filter...", theme::hint()))
        } else if is_focused {
            Line::from(vec![
                Span::styled("/ ", Style::default().fg(theme::STEEL_GRAY)),
                Span::styled(self.filter.clone(), Style::default().fg(theme::CREAM)),
                Span::styled("█", Style::default().fg(theme::SUNNY_GOLD)),
            ])
        } else {
            Line::from(vec![
                Span::styled("/ ", Style::default().fg(theme::STEEL_GRAY)),
                Span::styled(self.filter.clone(), Style::default().fg(theme::CREAM)),
            ])
        };

        let filter_block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style);

        frame.render_widget(Paragraph::new(content).block(filter_block), area);
    }

    fn render_list(&self, frame: &mut Frame, area: Rect) {
        let total = self.filtered_indices.len();
        if total == 0 {
            let msg = if self.filter.is_empty() { "no sessions" } else { "no matches" };
            frame.render_widget(
                Paragraph::new(Span::styled(msg, theme::hint())),
                area,
            );
            return;
        }

        let visible = area.height as usize;
        // Keep cursor in view, centering it
        let effective_scroll = if total <= visible {
            0
        } else {
            let half = visible / 2;
            self.cursor.saturating_sub(half).min(total - visible)
        };

        let end = (effective_scroll + visible).min(total);

        let items: Vec<Line> = (effective_scroll..end)
            .map(|i| {
                let session = &self.sessions[self.filtered_indices[i]];
                let is_cursor = i == self.cursor;
                let is_selected = self.selected.contains(&i);
                let is_active =
                    self.active_session_id.as_deref() == Some(session.id.as_str());
                let is_confirm = self.confirm_delete_idx == Some(i);

                let prefix = if is_confirm {
                    Span::styled("✗ ", Style::default().fg(theme::SIGNAL_RED))
                } else if is_active {
                    Span::styled("● ", Style::default().fg(theme::SUNNY_GOLD))
                } else if is_selected {
                    Span::styled("✓ ", Style::default().fg(theme::SUCCESS))
                } else {
                    Span::raw("  ")
                };

                let title = session.title.as_deref().unwrap_or("(untitled)");
                let max_title = (area.width as usize).saturating_sub(6);
                let truncated: String = title.chars().take(max_title).collect();
                let title_str = if title.chars().count() > max_title {
                    format!("{}…", truncated)
                } else {
                    truncated
                };

                let title_style = if is_cursor && self.pane_focus == PaneFocus::List {
                    Style::default()
                        .fg(theme::CREAM)
                        .add_modifier(Modifier::BOLD)
                        .add_modifier(Modifier::REVERSED)
                } else if is_confirm {
                    Style::default().fg(theme::SIGNAL_RED)
                } else {
                    Style::default().fg(theme::CREAM)
                };

                let mut spans = vec![prefix, Span::styled(title_str, title_style)];

                if is_active {
                    spans.push(Span::styled(" [a]", theme::hint()));
                }
                if is_confirm {
                    spans.push(Span::styled(
                        "  d again",
                        Style::default().fg(theme::SIGNAL_RED),
                    ));
                }
                if self.global_mode {
                    spans.push(Span::styled(
                        format!(" {}", shorten_path(&session.working_dir)),
                        theme::hint(),
                    ));
                }

                Line::from(spans)
            })
            .collect();

        frame.render_widget(Paragraph::new(items), area);
    }

    fn render_preview_pane(&self, frame: &mut Frame, area: Rect, tick_count: usize) {
        let is_focused = self.pane_focus == PaneFocus::Preview;
        let border_style =
            if is_focused { theme::border_focused() } else { theme::border_unfocused() };

        let session = self.current_session();
        let title_text = session
            .map(|s| s.title.as_deref().unwrap_or("(untitled)").to_string())
            .unwrap_or_default();

        let block = Block::default()
            .title(Span::styled(
                format!(" {} ", title_text),
                Style::default().fg(theme::CREAM).add_modifier(Modifier::BOLD),
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        if session.is_none() {
            frame.render_widget(
                Paragraph::new(Span::styled("no session selected", theme::hint())),
                inner,
            );
            return;
        }

        let session = session.unwrap();

        // Metadata row
        let model = session.model.as_deref().unwrap_or("unknown").to_string();
        let tokens = format_tokens(session.token_count);
        let created = session.created_at.format("%Y-%m-%d").to_string();
        let updated = session.updated_at.format("%Y-%m-%d %H:%M").to_string();
        let meta_line = Line::from(vec![
            Span::styled(model, Style::default().fg(theme::SUNNY_GOLD)),
            Span::styled(" · ", theme::hint()),
            Span::styled(tokens, Style::default().fg(theme::CREAM)),
            Span::styled(" tok · ", theme::hint()),
            Span::styled(created, theme::hint()),
            Span::styled(" → ", theme::hint()),
            Span::styled(updated, theme::hint()),
        ]);

        if self.preview_loading {
            let spinner = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let spin = spinner[tick_count % spinner.len()];
            let lines = vec![
                meta_line,
                Line::raw(""),
                Line::from(Span::styled(
                    format!("{} loading…", spin),
                    theme::hint(),
                )),
            ];
            frame.render_widget(Paragraph::new(lines), inner);
            return;
        }

        if let Some(preview) = &self.preview {
            let mut lines = vec![meta_line, Line::raw("")];

            for msg in &preview.head {
                push_chat_message_lines(&mut lines, msg);
            }

            if preview.total > 20 && !preview.tail.is_empty() {
                let shown = preview.head.len() + preview.tail.len();
                let omitted = preview.total.saturating_sub(shown);
                if omitted > 0 {
                    lines.push(Line::raw(""));
                    lines.push(Line::from(Span::styled(
                        format!("        ··· {} more messages ···", omitted),
                        theme::hint(),
                    )));
                    lines.push(Line::raw(""));
                }
                for msg in &preview.tail {
                    push_chat_message_lines(&mut lines, msg);
                }
            }

            let scroll = self.preview_scroll.min(lines.len().saturating_sub(1)) as u16;
            frame.render_widget(
                Paragraph::new(lines).scroll((scroll, 0)).wrap(Wrap { trim: false }),
                inner,
            );
        } else {
            frame.render_widget(
                Paragraph::new(vec![
                    meta_line,
                    Line::raw(""),
                    Line::from(Span::styled("select a session to preview", theme::hint())),
                ]),
                inner,
            );
        }
    }

    fn render_rename_modal(&self, frame: &mut Frame, parent: Rect, modal: &RenameModal) {
        let width = (parent.width * 6 / 10).clamp(20, 60);
        let height = 5u16;
        let x = parent.x + (parent.width.saturating_sub(width)) / 2;
        let y = parent.y + (parent.height.saturating_sub(height)) / 2;
        let modal_area = Rect::new(x, y, width, height);

        frame.render_widget(Clear, modal_area);

        let content = Line::from(vec![
            Span::styled(modal.title.clone(), Style::default().fg(theme::CREAM)),
            Span::styled("█", Style::default().fg(theme::SUNNY_GOLD)),
        ]);

        let block = Block::default()
            .title(Span::styled(
                " Rename Session ",
                Style::default().fg(theme::CREAM),
            ))
            .title_bottom(Line::from(Span::styled(
                " enter confirm · esc cancel ",
                theme::hint(),
            )))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(theme::border_focused());

        frame.render_widget(Paragraph::new(content).block(block), modal_area);
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn overlay_rect(area: Rect) -> Rect {
        let width = (area.width * 9 / 10).min(120);
        let height = area.height.saturating_sub(4);
        let x = area.x + (area.width.saturating_sub(width)) / 2;
        let y = area.y + (area.height.saturating_sub(height)) / 2;
        Rect::new(x, y, width, height)
    }

    fn apply_filter(&mut self) {
        let q = self.filter.to_lowercase();
        self.filtered_indices = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                let title = s.title.as_deref().unwrap_or("").to_lowercase();
                let id_short = &s.id[..8.min(s.id.len())];
                q.is_empty() || title.contains(&q) || id_short.contains(&q)
            })
            .map(|(i, _)| i)
            .collect();
        self.cursor = self.cursor.min(self.filtered_indices.len().saturating_sub(1));
        self.selected.retain(|&i| i < self.filtered_indices.len());
    }

    pub fn current_session(&self) -> Option<&SavedSession> {
        self.filtered_indices.get(self.cursor).and_then(|&i| self.sessions.get(i))
    }

    fn cursor_down(&mut self) {
        let max = self.filtered_indices.len().saturating_sub(1);
        if self.cursor < max {
            self.cursor += 1;
        }
        self.confirm_delete_idx = None;
    }

    fn cursor_up(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
        }
        self.confirm_delete_idx = None;
    }

    fn scope_label(&self) -> String {
        if self.global_mode {
            "global".to_string()
        } else {
            shorten_path(&self.workspace_dir)
        }
    }
}

// ── Free helpers ─────────────────────────────────────────────────────────────

fn shorten_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.to_string_lossy();
        if path.starts_with(home_str.as_ref()) {
            return format!("~{}", &path[home_str.len()..]);
        }
    }
    path.to_string()
}

fn format_tokens(count: u32) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

fn strip_preamble(content: &str) -> &str {
    if let Some(pos) = content.find("\n\n---\n\n") {
        &content[pos + "\n\n---\n\n".len()..]
    } else {
        content
    }
}

fn push_chat_message_lines(lines: &mut Vec<Line<'static>>, msg: &ChatMessage) {
    match msg.role {
        ChatRole::User => {
            if msg.content.starts_with("[Mode Switch]") {
                return;
            }
            lines.push(Line::from(Span::styled(
                "── you ─────────────────────────",
                Style::default().fg(theme::USER_ACCENT).add_modifier(Modifier::DIM),
            )));
            if !msg.content.is_empty() {
                let content = strip_preamble(&msg.content);
                let preview: String = content.chars().take(200).collect();
                lines.push(Line::from(Span::styled(
                    format!(" > {}", preview),
                    Style::default().fg(theme::CREAM),
                )));
            }
            lines.push(Line::raw(""));
        }
        ChatRole::Assistant => {
            lines.push(Line::from(Span::styled(
                "── sunny ───────────────────────",
                Style::default().fg(theme::SUNNY_GOLD).add_modifier(Modifier::DIM),
            )));
            if !msg.content.is_empty() {
                let preview: String = msg.content.chars().take(300).collect();
                for text_line in preview.lines().take(5) {
                    lines.push(Line::from(Span::styled(
                        format!(" {}", text_line),
                        Style::default().fg(theme::CREAM),
                    )));
                }
            }
            if let Some(tool_calls) = &msg.tool_calls {
                for tc in tool_calls {
                    let display_name = crate::ui::tool_call::tool_display_name(&tc.name);
                    let key_arg =
                        crate::ui::tool_call::extract_key_arg(&tc.name, &tc.arguments);
                    lines.push(Line::from(Span::styled(
                        format!(" [tool: {} {}]", display_name, key_arg),
                        theme::muted(),
                    )));
                }
            }
            lines.push(Line::raw(""));
        }
        _ => {} // Skip system/tool messages in preview
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_session(id: &str, title: Option<&str>) -> SavedSession {
        SavedSession {
            id: id.to_string(),
            title: title.map(String::from),
            model: Some("claude-sonnet-4-6".to_string()),
            working_dir: "/home/user/project".to_string(),
            token_count: 100,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn test_new_starts_hidden() {
        let sm = SessionManager::new("/workspace".to_string(), None);
        assert!(!sm.visible);
        assert!(sm.sessions.is_empty());
        assert_eq!(sm.cursor, 0);
    }

    #[test]
    fn test_set_sessions_updates_filter() {
        let mut sm = SessionManager::new("/workspace".to_string(), None);
        sm.set_sessions(vec![
            make_session("aaa-00000000", Some("hello world")),
            make_session("bbb-00000000", Some("foo bar")),
        ]);
        assert_eq!(sm.filtered_indices.len(), 2);
    }

    #[test]
    fn test_filter_by_title() {
        let mut sm = SessionManager::new("/workspace".to_string(), None);
        sm.set_sessions(vec![
            make_session("aaa-00000000", Some("hello world")),
            make_session("bbb-00000000", Some("foo bar")),
        ]);
        sm.filter = "hello".to_string();
        sm.apply_filter();
        assert_eq!(sm.filtered_indices.len(), 1);
        assert_eq!(sm.sessions[sm.filtered_indices[0]].title.as_deref(), Some("hello world"));
    }

    #[test]
    fn test_cursor_navigation() {
        let mut sm = SessionManager::new("/workspace".to_string(), None);
        sm.set_sessions(vec![
            make_session("a-0000000", Some("s1")),
            make_session("b-0000000", Some("s2")),
            make_session("c-0000000", Some("s3")),
        ]);
        assert_eq!(sm.cursor, 0);
        sm.cursor_down();
        assert_eq!(sm.cursor, 1);
        sm.cursor_down();
        assert_eq!(sm.cursor, 2);
        sm.cursor_down(); // at end, stays
        assert_eq!(sm.cursor, 2);
        sm.cursor_up();
        assert_eq!(sm.cursor, 1);
    }

    #[test]
    fn test_handle_key_esc_in_list_closes() {
        let mut sm = SessionManager::new("/workspace".to_string(), None);
        sm.visible = true;
        sm.set_sessions(vec![make_session("a-0000000", Some("s1"))]);
        let action = sm.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!sm.visible);
        assert!(matches!(action, SessionAction::None));
    }

    #[test]
    fn test_handle_key_g_toggles_global() {
        let mut sm = SessionManager::new("/workspace".to_string(), None);
        sm.visible = true;
        assert!(!sm.global_mode);
        let action = sm.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert!(sm.global_mode);
        assert!(matches!(action, SessionAction::RefreshList { global: true }));
        let action2 = sm.handle_key(KeyCode::Char('g'), KeyModifiers::NONE);
        assert!(!sm.global_mode);
        assert!(matches!(action2, SessionAction::RefreshList { global: false }));
    }

    #[test]
    fn test_handle_key_enter_opens_session() {
        let mut sm = SessionManager::new("/workspace".to_string(), None);
        sm.visible = true;
        sm.set_sessions(vec![make_session("abc-defg-hi", Some("my session"))]);
        let action = sm.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(action, SessionAction::Open(_)));
    }

    #[test]
    fn test_single_delete_double_d() {
        let mut sm = SessionManager::new("/workspace".to_string(), None);
        sm.visible = true;
        sm.set_sessions(vec![make_session("abc-defg-hi", Some("session"))]);
        // First d: set confirm
        let a1 = sm.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(matches!(a1, SessionAction::None));
        assert_eq!(sm.confirm_delete_idx, Some(0));
        // Second d: confirm delete
        let a2 = sm.handle_key(KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(matches!(a2, SessionAction::Delete(_)));
        assert_eq!(sm.confirm_delete_idx, None);
    }

    #[test]
    fn test_rename_modal_workflow() {
        let mut sm = SessionManager::new("/workspace".to_string(), None);
        sm.visible = true;
        sm.set_sessions(vec![make_session("abc-defg-hi", Some("old name"))]);
        // r opens rename modal
        sm.handle_key(KeyCode::Char('r'), KeyModifiers::NONE);
        assert!(sm.rename_modal.is_some());
        assert_eq!(sm.rename_modal.as_ref().unwrap().title, "old name");
        // type new char
        sm.handle_key(KeyCode::Char('!'), KeyModifiers::NONE);
        assert_eq!(sm.rename_modal.as_ref().unwrap().title, "old name!");
        // Esc cancels
        sm.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(sm.rename_modal.is_none());
    }
}
