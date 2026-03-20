//! Status bar widget — model name, token counts, streaming indicator.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::bridge::AgentMode;
use crate::ui::theme;
use crate::ui::tool_call::SPINNER_FRAMES;

/// Token usage counters.
#[derive(Debug, Clone, Default)]
pub struct TokenUsageDisplay {
    pub input: u32,
    pub output: u32,
    pub total: u32,
}

impl TokenUsageDisplay {
    /// Format token count as human-readable: 1200 → "1.2K", 999 → "999"
    pub fn format_tokens(n: u32) -> String {
        if n >= 1000 {
            format!("{:.1}K", n as f64 / 1000.0)
        } else {
            n.to_string()
        }
    }
}

/// Status bar rendered at the bottom of the screen.
pub struct StatusBar {
    pub model_name: String,
    pub usage: TokenUsageDisplay,
}

impl StatusBar {
    pub fn new(model_name: impl Into<String>) -> Self {
        Self {
            model_name: model_name.into(),
            usage: TokenUsageDisplay::default(),
        }
    }

    /// Render the status bar into the given frame area.
    pub fn render(
        &self,
        frame: &mut Frame,
        area: Rect,
        is_streaming: bool,
        tick_count: usize,
        session_id: Option<&str>,
        mode: &AgentMode,
    ) {
        let spinner = if is_streaming {
            let frame_idx = tick_count % SPINNER_FRAMES.len();
            format!("{} streaming ", SPINNER_FRAMES[frame_idx])
        } else {
            String::new()
        };

        let (mode_label, mode_style) = match mode {
            AgentMode::Quick => (
                " quick ",
                Style::default()
                    .fg(theme::SUNNY_GOLD)
                    .add_modifier(Modifier::BOLD),
            ),
            AgentMode::Smart => (
                " smart ",
                Style::default()
                    .fg(theme::CREAM)
                    .add_modifier(Modifier::BOLD),
            ),
        };
        let mode_span = Span::styled(mode_label, mode_style);

        let sep0_span = Span::styled(" │ ", Style::default().fg(theme::STEEL_GRAY));

        let model_span = Span::styled(
            format!("{} ", self.model_name),
            Style::default()
                .fg(theme::STEEL_GRAY)
                .add_modifier(Modifier::DIM),
        );

        let sep_span = Span::styled("│ ", Style::default().fg(theme::STEEL_GRAY));

        let tokens_span = Span::styled(
            format!(
                "in:{} out:{}",
                TokenUsageDisplay::format_tokens(self.usage.input),
                TokenUsageDisplay::format_tokens(self.usage.output),
            ),
            Style::default().fg(theme::STEEL_GRAY),
        );

        let sep2_span = Span::styled(" │ ", Style::default().fg(theme::STEEL_GRAY));

        let streaming_span = Span::styled(
            spinner,
            Style::default()
                .fg(theme::SUNNY_GOLD)
                .add_modifier(Modifier::BOLD),
        );

        let session_span = if let Some(sid) = session_id {
            let short_id: String = sid.chars().take(8).collect();
            Span::styled(
                format!("│ sess:{short_id}"),
                Style::default().fg(theme::STEEL_GRAY),
            )
        } else {
            Span::raw("")
        };

        let line = Line::from(vec![
            mode_span,
            sep0_span,
            model_span,
            sep_span,
            tokens_span,
            sep2_span,
            streaming_span,
            session_span,
        ]);

        let paragraph = Paragraph::new(line)
            .block(Block::default().borders(Borders::NONE))
            .style(Style::default().bg(theme::CHARCOAL));

        frame.render_widget(paragraph, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_tokens_below_1000() {
        assert_eq!(TokenUsageDisplay::format_tokens(999), "999");
        assert_eq!(TokenUsageDisplay::format_tokens(0), "0");
    }

    #[test]
    fn test_format_tokens_1k() {
        assert_eq!(TokenUsageDisplay::format_tokens(1000), "1.0K");
        assert_eq!(TokenUsageDisplay::format_tokens(1500), "1.5K");
        assert_eq!(TokenUsageDisplay::format_tokens(12000), "12.0K");
    }

    #[test]
    fn test_status_bar_new() {
        let bar = StatusBar::new("claude-sonnet-4-6");
        assert_eq!(bar.model_name, "claude-sonnet-4-6");
        assert_eq!(bar.usage.input, 0);
        assert_eq!(bar.usage.output, 0);
        assert_eq!(bar.usage.total, 0);
    }

    #[test]
    fn test_token_usage_display_default() {
        let usage = TokenUsageDisplay::default();
        assert_eq!(usage.input, 0);
        assert_eq!(usage.output, 0);
        assert_eq!(usage.total, 0);
    }
}
