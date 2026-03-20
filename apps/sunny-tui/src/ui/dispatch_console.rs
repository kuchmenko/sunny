//! DispatchConsole — the primary input widget for sunny-tui.
//!
//! Replaces the plain `InputWidget` with brand-consistent styling,
//! animated borders, embedded keybinding hints, and a ghost-text
//! completion slot ready to wire to AI completions.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph},
    Frame,
};
use tui_textarea::TextArea;

use crate::ui::theme;

// ── Animation constants ────────────────────────────────────────────────────

pub const FLASH_MAX_TICKS: u8 = 8; // 480ms flash
const MODE_ANIM_MAX: u8 = 8;

/// Smoothstep S-curve: 3t² − 2t³.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

// ── Ghost text ────────────────────────────────────────────────────────────────

pub struct GhostText {
    pub suggestion: String,
    pub visible_chars: usize,
}

// ── Key action ────────────────────────────────────────────────────────────────

pub enum DispatchKeyAction {
    Consumed,
    AcceptGhost,
}

// ── DispatchConsole ───────────────────────────────────────────────────────────

pub struct DispatchConsole {
    pub textarea: TextArea<'static>,
    // ── Animations ────────────────────────────────────────────────────────────
    pub send_flash_ticks: u8,
    pub mode_anim_ticks: u8,
    pub mode_label: String,
    // ── Ghost completion ──────────────────────────────────────────────────────
    pub ghost: Option<GhostText>,
    pub completion_idle_ticks: u8,
    pub completion_requested: bool,
    pub last_input_for_completion: String,
    // ── Internal ──────────────────────────────────────────────────────────────
    input_changed_this_tick: bool,
}

impl DispatchConsole {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_placeholder_text("Dispatch a message...");
        textarea.set_style(Style::default().fg(theme::CREAM).bg(theme::CHARCOAL));
        textarea.set_cursor_style(Style::default().fg(theme::CHARCOAL).bg(theme::SUNNY_GOLD));
        textarea.set_cursor_line_style(Style::default());
        // No block on textarea — outer block handled by render()
        textarea.set_block(Block::default());
        Self {
            textarea,
            send_flash_ticks: 0,
            mode_anim_ticks: 0,
            mode_label: "quick".to_string(),
            ghost: None,
            completion_idle_ticks: 0,
            completion_requested: false,
            last_input_for_completion: String::new(),
            input_changed_this_tick: false,
        }
    }

    /// Called by App when `AgentToTui::ModeChanged` arrives.
    pub fn set_mode(&mut self, label: &str) {
        if label != self.mode_label {
            self.mode_label = label.to_string();
            self.mode_anim_ticks = 8;
        }
    }

    /// Called from the main event loop tick arm.
    pub fn on_tick(&mut self) {
        if self.send_flash_ticks > 0 {
            self.send_flash_ticks -= 1;
        }
        if self.mode_anim_ticks > 0 {
            self.mode_anim_ticks -= 1;
        }

        // Ghost text fade-in
        if let Some(ghost) = &mut self.ghost {
            if ghost.visible_chars < ghost.suggestion.len() {
                ghost.visible_chars = (ghost.visible_chars + 3).min(ghost.suggestion.len());
            }
        }

        // Completion debounce: fire after 10 idle ticks (~600ms)
        if self.input_changed_this_tick {
            self.completion_idle_ticks = 0;
            self.input_changed_this_tick = false;
        } else if self.completion_idle_ticks < 10 {
            self.completion_idle_ticks += 1;
            if self.completion_idle_ticks == 10 {
                let current = self.textarea.lines().join("\n");
                if !current.trim().is_empty() && !current.starts_with('/') {
                    self.last_input_for_completion = current;
                    self.completion_requested = true;
                }
            }
        }
    }

    /// Route a key event through the console. Returns action for caller.
    pub fn handle_key(&mut self, key: KeyEvent) -> DispatchKeyAction {
        // Tab with ghost text → caller should accept it
        if key.code == KeyCode::Tab && self.ghost.is_some() {
            return DispatchKeyAction::AcceptGhost;
        }
        // Esc with ghost → dismiss ghost only
        if key.code == KeyCode::Esc && self.ghost.is_some() {
            self.ghost = None;
            return DispatchKeyAction::Consumed;
        }
        // All other keys go to textarea
        self.textarea.input(key);
        self.input_changed_this_tick = true;
        self.ghost = None;
        DispatchKeyAction::Consumed
    }

    /// Extract the current message, clear the textarea, and trigger send flash.
    pub fn take_message(&mut self) -> Option<String> {
        let text = self.textarea.lines().join("\n");
        let trimmed = text.trim().to_owned();
        if trimmed.is_empty() {
            return None;
        }
        self.textarea.select_all();
        self.textarea.cut();
        self.ghost = None;
        self.completion_idle_ticks = 0;
        self.send_flash_ticks = FLASH_MAX_TICKS;
        Some(trimmed)
    }

    /// Set ghost completion text (fade-in starts from 0).
    pub fn set_ghost_text(&mut self, text: String) {
        self.ghost = Some(GhostText {
            suggestion: text,
            visible_chars: 0,
        });
    }

    /// Accept the current ghost suggestion into the textarea.
    pub fn accept_ghost(&mut self) {
        if let Some(ghost) = self.ghost.take() {
            self.textarea.insert_str(&ghost.suggestion);
        }
    }

    /// Render the full console into `area` (must be 6 rows tall).
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        focused: bool,
        is_streaming: bool,
        _tick_count: usize,
    ) {
        // Step 1 — compute border color (action-only animations)
        // Flash: start at CREAM, quadratic ease-out back to GOLD
        let border_color = if self.send_flash_ticks > 0 {
            let t = self.send_flash_ticks as f32 / FLASH_MAX_TICKS as f32;
            theme::lerp(theme::SUNNY_GOLD, theme::CREAM, t * t)
        } else if focused {
            theme::SUNNY_GOLD
        } else {
            theme::STEEL_GRAY
        };

        // BOLD only during peak flash (top third of flash duration)
        let border_style = if self.send_flash_ticks > (FLASH_MAX_TICKS * 2 / 3) {
            Style::default().fg(border_color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(border_color)
        };

        // Step 2 — compute mode pill (char-reveal + color lerp animation)
        let mode_target = if self.mode_label == "smart" { theme::CREAM } else { theme::SUNNY_GOLD };
        let (mode_pill, pill_color) = if self.mode_anim_ticks > 0 {
            let progress = (MODE_ANIM_MAX - self.mode_anim_ticks) as f32 / MODE_ANIM_MAX as f32;
            let reveal = (MODE_ANIM_MAX - self.mode_anim_ticks) as usize;
            let chars: String = self.mode_label.chars().take(reveal).collect();
            (
                format!("[ {} ]", chars),
                theme::lerp(theme::STEEL_GRAY, mode_target, smoothstep(progress)),
            )
        } else {
            (format!("[ {} ]", self.mode_label), mode_target)
        };
        let mode_style = Style::default().fg(pill_color).add_modifier(Modifier::BOLD);

        // Step 3 — outer block with dual titles
        let block = Block::default()
            .title(Line::from(Span::styled("─ dispatch ", border_style)))
            .title(
                Line::from(vec![Span::styled(format!(" {} ", mode_pill), mode_style)])
                    .alignment(Alignment::Right),
            )
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(border_style);

        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Step 4 — split inner into [textarea(2), separator(1), hints(1)]
        let [textarea_area, sep_area, hints_area] = Layout::vertical([
            Constraint::Length(2),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);

        // Step 5 — ▶ prompt icon + textarea
        let prompt_area = Rect::new(textarea_area.x, textarea_area.y, 2, textarea_area.height);
        let ta_area = Rect::new(
            textarea_area.x + 2,
            textarea_area.y,
            textarea_area.width.saturating_sub(2),
            textarea_area.height,
        );
        // ▶ prompt shares border_color for visual sync
        frame.render_widget(
            Paragraph::new(Span::styled("▶", Style::default().fg(border_color))),
            prompt_area,
        );
        frame.render_widget(&self.textarea, ta_area);

        // Step 6 — ghost text overlay (dim suggestion after cursor)
        let has_ghost = self.ghost.is_some();
        if let Some(ghost) = &self.ghost {
            let (row, col) = self.textarea.cursor();
            let visible: String = ghost.suggestion.chars().take(ghost.visible_chars).collect();
            let ghost_x = ta_area.x + col as u16;
            let ghost_y = ta_area.y + row as u16;
            if ghost_x < ta_area.x + ta_area.width && ghost_y < ta_area.y + ta_area.height {
                let ghost_width = ta_area.width.saturating_sub(col as u16);
                let clamped: String = visible.chars().take(ghost_width as usize).collect();
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        clamped,
                        Style::default().fg(theme::STEEL_GRAY),
                    )),
                    Rect::new(ghost_x, ghost_y, ghost_width, 1),
                );
            }
        }

        // Step 7 — dashed separator
        let sep_chars = "─ ".repeat((sep_area.width as usize) / 2 + 1);
        let sep_str: String = sep_chars.chars().take(sep_area.width as usize).collect();
        frame.render_widget(
            Paragraph::new(Span::styled(sep_str, theme::hint())),
            sep_area,
        );

        // Step 8 — contextual hint bar
        let hint_line = if has_ghost {
            Line::from(vec![
                Span::styled("tab", Style::default().fg(theme::SUNNY_GOLD)),
                Span::styled(" accept · ", theme::hint()),
                Span::styled("esc", Style::default().fg(theme::SUNNY_GOLD)),
                Span::styled(" clear · ", theme::hint()),
                Span::styled("enter", Style::default().fg(theme::SUNNY_GOLD)),
                Span::styled(" dispatch", theme::hint()),
            ])
        } else if is_streaming {
            Line::from(vec![
                Span::styled("ctrl+c", Style::default().fg(theme::SUNNY_GOLD)),
                Span::styled(" cancel · ", theme::hint()),
                Span::styled("ctrl+s", Style::default().fg(theme::SUNNY_GOLD)),
                Span::styled(" sessions", theme::hint()),
            ])
        } else {
            Line::from(vec![
                Span::styled("enter", Style::default().fg(theme::SUNNY_GOLD)),
                Span::styled(" dispatch · ", theme::hint()),
                Span::styled("ctrl+j", Style::default().fg(theme::SUNNY_GOLD)),
                Span::styled(" newline · ", theme::hint()),
                Span::styled("shift+tab", Style::default().fg(theme::SUNNY_GOLD)),
                Span::styled(" mode · ", theme::hint()),
                Span::styled("/", Style::default().fg(theme::SUNNY_GOLD)),
                Span::styled(" commands", theme::hint()),
            ])
        };
        frame.render_widget(Paragraph::new(hint_line), hints_area);
    }
}

impl Default for DispatchConsole {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dispatch_console_new_empty() {
        let console = DispatchConsole::new();
        let lines = console.textarea.lines();
        assert!(lines.len() <= 1);
        assert!(lines.iter().all(|l| l.is_empty()));
    }

    #[test]
    fn test_take_message_empty_returns_none() {
        let mut console = DispatchConsole::new();
        assert!(console.take_message().is_none());
    }

    #[test]
    fn test_take_message_sets_flash_ticks() {
        let mut console = DispatchConsole::new();
        console.textarea.insert_str("hello");
        let msg = console.take_message();
        assert_eq!(msg, Some("hello".to_string()));
        assert_eq!(console.send_flash_ticks, FLASH_MAX_TICKS);
    }

    #[test]
    fn test_set_mode_animates_on_change() {
        let mut console = DispatchConsole::new();
        assert_eq!(console.mode_anim_ticks, 0);
        console.set_mode("smart");
        assert_eq!(console.mode_anim_ticks, 8);
        assert_eq!(console.mode_label, "smart");
    }

    #[test]
    fn test_set_mode_no_anim_if_same() {
        let mut console = DispatchConsole::new();
        console.set_mode("quick"); // same as initial
        assert_eq!(console.mode_anim_ticks, 0);
    }

    #[test]
    fn test_on_tick_decrements_flash() {
        let mut console = DispatchConsole::new();
        console.send_flash_ticks = 3;
        console.on_tick();
        assert_eq!(console.send_flash_ticks, 2);
    }

    #[test]
    fn test_ghost_text_accept() {
        let mut console = DispatchConsole::new();
        console.set_ghost_text("world".to_string());
        assert!(console.ghost.is_some());
        console.accept_ghost();
        assert!(console.ghost.is_none());
        let text = console.textarea.lines().join("\n");
        assert_eq!(text, "world");
    }
}
