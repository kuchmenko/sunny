//! Command palette — slash command suggestions popup.
//!
//! Triggered when input starts with `/`. Shows filtered commands
//! above the input area. Tab to complete, arrows to navigate.
//! When input matches a command exactly followed by a space, switches to
//! subcommand mode (e.g. `/context ` → shows "debug").

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::ui::theme;

// ── Command definitions ────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct SubcommandDef {
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone)]
pub struct CommandDef {
    pub name: &'static str,
    pub description: &'static str,
    pub subcommands: &'static [SubcommandDef],
}

pub const COMMANDS: &[CommandDef] = &[
    CommandDef {
        name: "/help",
        description: "Show commands and shortcuts",
        subcommands: &[],
    },
    CommandDef {
        name: "/clear",
        description: "Clear the conversation",
        subcommands: &[],
    },
    CommandDef {
        name: "/cancel",
        description: "Cancel current operation",
        subcommands: &[],
    },
    CommandDef {
        name: "/model",
        description: "Show model and session info",
        subcommands: &[],
    },
    CommandDef {
        name: "/context",
        description: "Copy conversation to clipboard",
        subcommands: &[SubcommandDef {
            name: "debug",
            description: "Include full debug info (XML)",
        }],
    },
    CommandDef {
        name: "/sessions",
        description: "Browse and switch sessions",
        subcommands: &[],
    },
    CommandDef {
        name: "/exit",
        description: "End shift",
        subcommands: &[],
    },
    CommandDef {
        name: "/quit",
        description: "End shift",
        subcommands: &[],
    },
];

// ── Palette state ──────────────────────────────────────────────────────────

pub struct CommandPalette {
    pub visible: bool,
    pub selected: usize,
    /// Indices into COMMANDS (top-level mode) or into parent's subcommands (sub mode).
    filtered: Vec<usize>,
    /// Set when showing subcommands of a specific command.
    parent: Option<usize>,
}

impl CommandPalette {
    pub fn new() -> Self {
        Self {
            visible: false,
            selected: 0,
            filtered: Vec::new(),
            parent: None,
        }
    }

    /// Update palette state based on current input text.
    ///
    /// - `/` or `/partial` → filter top-level commands.
    /// - `/command ` or `/command sub` → switch to subcommand mode if the
    ///   command has subcommands.
    pub fn update(&mut self, input_text: &str) {
        if !input_text.starts_with('/') {
            self.hide();
            return;
        }

        let rest = &input_text[1..];

        if let Some(space_pos) = rest.find(' ') {
            // Input contains a space → check for subcommand mode
            let cmd_name = format!("/{}", &rest[..space_pos]);
            let sub_filter = rest[space_pos + 1..].to_lowercase();

            if let Some((cmd_idx, cmd)) =
                COMMANDS.iter().enumerate().find(|(_, c)| c.name == cmd_name)
            {
                if !cmd.subcommands.is_empty() {
                    self.parent = Some(cmd_idx);
                    self.filtered = cmd
                        .subcommands
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| sub_filter.is_empty() || s.name.starts_with(&sub_filter))
                        .map(|(i, _)| i)
                        .collect();
                    self.visible = !self.filtered.is_empty();
                    if self.selected >= self.filtered.len() {
                        self.selected = 0;
                    }
                    return;
                }
            }
            // Command has no subcommands (or doesn't exist)
            self.hide();
        } else {
            // No space yet — filter top-level commands
            self.parent = None;
            let filter = input_text.to_lowercase();
            self.filtered = COMMANDS
                .iter()
                .enumerate()
                .filter(|(_, cmd)| cmd.name.starts_with(&filter))
                .map(|(i, _)| i)
                .collect();
            self.visible = !self.filtered.is_empty();
            if self.selected >= self.filtered.len() {
                self.selected = 0;
            }
        }
    }

    pub fn hide(&mut self) {
        self.visible = false;
        self.selected = 0;
        self.filtered.clear();
        self.parent = None;
    }

    pub fn select_next(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = (self.selected + 1) % self.filtered.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.filtered.is_empty() {
            self.selected = self
                .selected
                .checked_sub(1)
                .unwrap_or(self.filtered.len() - 1);
        }
    }

    /// Returns the full command string to execute, including subcommand if in sub mode.
    /// e.g. `"/help"` or `"/context debug"`.
    pub fn selected_command(&self) -> Option<String> {
        if let Some(parent_idx) = self.parent {
            let cmd = &COMMANDS[parent_idx];
            let sub_idx = *self.filtered.get(self.selected)?;
            Some(format!("{} {}", cmd.name, cmd.subcommands[sub_idx].name))
        } else {
            self.filtered.get(self.selected).map(|&i| COMMANDS[i].name.to_owned())
        }
    }

    /// Render the palette as a floating popup above the input area.
    pub fn render(&self, frame: &mut Frame, input_area: Rect) {
        if !self.visible || self.filtered.is_empty() {
            return;
        }

        let item_count = self.filtered.len() as u16;
        let height = (item_count + 2).min(input_area.y); // +2 for borders
        if height < 3 {
            return;
        }

        let width = input_area.width.min(60).saturating_sub(2);
        let area = Rect::new(
            input_area.x + 1,
            input_area.y.saturating_sub(height),
            width,
            height,
        );

        frame.render_widget(Clear, area);

        let items: Vec<Line> = self
            .filtered
            .iter()
            .enumerate()
            .map(|(display_idx, &item_idx)| {
                let is_selected = display_idx == self.selected;

                let (name_text, desc_text) = if let Some(parent_idx) = self.parent {
                    let cmd = &COMMANDS[parent_idx];
                    let sub = &cmd.subcommands[item_idx];
                    (format!("{} {}", cmd.name, sub.name), sub.description)
                } else {
                    let cmd = &COMMANDS[item_idx];
                    (cmd.name.to_owned(), cmd.description)
                };

                let name_style = if is_selected {
                    Style::default().fg(theme::SUNNY_GOLD).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme::CREAM)
                };
                let desc_style = Style::default().fg(theme::STEEL_GRAY);
                let marker = if is_selected { "▸ " } else { "  " };

                if let Some(parent_idx) = self.parent {
                    // In subcommand mode: dim the parent prefix, highlight the subcommand part
                    let cmd = &COMMANDS[parent_idx];
                    let prefix_style = if is_selected {
                        Style::default().fg(theme::STEEL_GRAY).add_modifier(Modifier::BOLD)
                    } else {
                        theme::hint()
                    };
                    let sub = &COMMANDS[parent_idx].subcommands[item_idx];
                    Line::from(vec![
                        Span::styled(marker, name_style),
                        Span::styled(cmd.name, prefix_style),
                        Span::styled(" ", name_style),
                        Span::styled(sub.name, name_style),
                        Span::styled(format!("  {}", desc_text), desc_style),
                    ])
                } else {
                    Line::from(vec![
                        Span::styled(marker, name_style),
                        Span::styled(name_text, name_style),
                        Span::styled(format!("  {}", desc_text), desc_style),
                    ])
                }
            })
            .collect();

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme::STEEL_GRAY))
            .title_bottom(Line::from(Span::styled(
                " tab complete · esc dismiss ",
                theme::hint(),
            )));

        let paragraph = Paragraph::new(items).block(block);
        frame.render_widget(paragraph, area);
    }
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_palette_hidden_by_default() {
        let palette = CommandPalette::new();
        assert!(!palette.visible);
    }

    #[test]
    fn test_palette_shows_on_slash() {
        let mut palette = CommandPalette::new();
        palette.update("/");
        assert!(palette.visible);
        assert_eq!(palette.filtered.len(), COMMANDS.len());
    }

    #[test]
    fn test_palette_filters_on_partial() {
        let mut palette = CommandPalette::new();
        palette.update("/he");
        assert!(palette.visible);
        assert_eq!(palette.filtered.len(), 1);
        assert_eq!(palette.selected_command().as_deref(), Some("/help"));
    }

    #[test]
    fn test_palette_hides_on_space_with_no_subcommands() {
        let mut palette = CommandPalette::new();
        palette.update("/help ");
        assert!(!palette.visible);
    }

    #[test]
    fn test_palette_shows_subcommands_after_space() {
        let mut palette = CommandPalette::new();
        palette.update("/context ");
        assert!(palette.visible);
        assert!(palette.parent.is_some());
        assert_eq!(palette.filtered.len(), 1);
        assert_eq!(palette.selected_command().as_deref(), Some("/context debug"));
    }

    #[test]
    fn test_palette_filters_subcommands_on_partial() {
        let mut palette = CommandPalette::new();
        palette.update("/context de");
        assert!(palette.visible);
        assert_eq!(palette.selected_command().as_deref(), Some("/context debug"));
    }

    #[test]
    fn test_palette_hides_on_unknown_subcommand() {
        let mut palette = CommandPalette::new();
        palette.update("/context zzz");
        assert!(!palette.visible);
    }

    #[test]
    fn test_palette_hides_on_plain_text() {
        let mut palette = CommandPalette::new();
        palette.update("hello");
        assert!(!palette.visible);
    }

    #[test]
    fn test_palette_select_wraps() {
        let mut palette = CommandPalette::new();
        palette.update("/");
        let count = palette.filtered.len();

        for _ in 0..count {
            palette.select_next();
        }
        assert_eq!(palette.selected, 0); // wrapped back

        palette.select_prev();
        assert_eq!(palette.selected, count - 1); // wrapped to end
    }

    #[test]
    fn test_palette_no_match_hides() {
        let mut palette = CommandPalette::new();
        palette.update("/zzz");
        assert!(!palette.visible);
    }

    #[test]
    fn test_palette_hide_resets_state() {
        let mut palette = CommandPalette::new();
        palette.update("/he");
        palette.select_next();
        palette.hide();
        assert!(!palette.visible);
        assert_eq!(palette.selected, 0);
        assert!(palette.filtered.is_empty());
        assert!(palette.parent.is_none());
    }
}
