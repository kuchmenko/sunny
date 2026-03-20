use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
    Frame,
};
use tui_markdown;

use crate::thread::{Thread, ThreadMessage};
use crate::ui::theme;
use crate::ui::tool_call::render_tool_call;

pub struct ThreadView {
    pub scroll_offset: usize,
    pub at_bottom: bool,
    pub total_lines: usize,
}

fn map_markdown_color(color: ratatui_core::style::Color) -> Color {
    use ratatui_core::style::Color as MarkdownColor;

    match color {
        MarkdownColor::Reset => Color::Reset,
        MarkdownColor::Black => Color::Black,
        MarkdownColor::Red => Color::Red,
        MarkdownColor::Green => Color::Green,
        MarkdownColor::Yellow => Color::Yellow,
        MarkdownColor::Blue => Color::Blue,
        MarkdownColor::Magenta => Color::Magenta,
        MarkdownColor::Cyan => Color::Cyan,
        MarkdownColor::Gray => Color::Gray,
        MarkdownColor::DarkGray => Color::DarkGray,
        MarkdownColor::LightRed => Color::LightRed,
        MarkdownColor::LightGreen => Color::LightGreen,
        MarkdownColor::LightYellow => Color::LightYellow,
        MarkdownColor::LightBlue => Color::LightBlue,
        MarkdownColor::LightMagenta => Color::LightMagenta,
        MarkdownColor::LightCyan => Color::LightCyan,
        MarkdownColor::White => Color::White,
        MarkdownColor::Indexed(i) => Color::Indexed(i),
        MarkdownColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn map_markdown_modifier(modifier: ratatui_core::style::Modifier) -> Modifier {
    use ratatui_core::style::Modifier as MarkdownModifier;

    let mut mapped = Modifier::empty();
    if modifier.contains(MarkdownModifier::BOLD) {
        mapped |= Modifier::BOLD;
    }
    if modifier.contains(MarkdownModifier::DIM) {
        mapped |= Modifier::DIM;
    }
    if modifier.contains(MarkdownModifier::ITALIC) {
        mapped |= Modifier::ITALIC;
    }
    if modifier.contains(MarkdownModifier::UNDERLINED) {
        mapped |= Modifier::UNDERLINED;
    }
    if modifier.contains(MarkdownModifier::SLOW_BLINK) {
        mapped |= Modifier::SLOW_BLINK;
    }
    if modifier.contains(MarkdownModifier::RAPID_BLINK) {
        mapped |= Modifier::RAPID_BLINK;
    }
    if modifier.contains(MarkdownModifier::REVERSED) {
        mapped |= Modifier::REVERSED;
    }
    if modifier.contains(MarkdownModifier::HIDDEN) {
        mapped |= Modifier::HIDDEN;
    }
    if modifier.contains(MarkdownModifier::CROSSED_OUT) {
        mapped |= Modifier::CROSSED_OUT;
    }
    mapped
}

fn map_markdown_style(style: ratatui_core::style::Style) -> Style {
    let mut mapped = Style::default();
    if let Some(fg) = style.fg {
        mapped = mapped.fg(map_markdown_color(fg));
    }
    if let Some(bg) = style.bg {
        mapped = mapped.bg(map_markdown_color(bg));
    }
    mapped = mapped.add_modifier(map_markdown_modifier(style.add_modifier));
    mapped.remove_modifier(map_markdown_modifier(style.sub_modifier))
}

impl ThreadView {
    pub fn new() -> Self {
        Self {
            scroll_offset: 0,
            at_bottom: true,
            total_lines: 0,
        }
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        self.at_bottom = false;
    }

    pub fn scroll_down(&mut self, n: usize, area_height: u16) {
        let max_offset = self.total_lines.saturating_sub(area_height as usize);
        self.scroll_offset = (self.scroll_offset + n).min(max_offset);
        self.at_bottom = self.scroll_offset >= max_offset;
    }

    pub fn scroll_to_bottom(&mut self, area_height: u16) {
        let max_offset = self.total_lines.saturating_sub(area_height as usize);
        self.scroll_offset = max_offset;
        self.at_bottom = true;
    }

    pub fn on_new_content(&mut self, area_height: u16) {
        if self.at_bottom {
            self.scroll_to_bottom(area_height);
        }
    }

    pub fn build_text(&self, thread: &Thread, tick_count: usize) -> Text<'static> {
        let mut all_lines: Vec<Line<'static>> = Vec::new();

        for msg in &thread.messages {
            match msg {
                ThreadMessage::User { content, .. } => {
                    all_lines.push(Line::from(Span::styled("you", theme::user_label())));
                    for content_line in content.lines() {
                        all_lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(
                                content_line.to_owned(),
                                Style::default().fg(theme::TEXT_PRIMARY),
                            ),
                        ]));
                    }
                    all_lines.push(Line::default());
                }
                ThreadMessage::Assistant {
                    content,
                    thinking,
                    tool_calls,
                    is_streaming,
                    ..
                } => {
                    all_lines.push(Line::from(Span::styled("sunny", theme::assistant_label())));

                    if !thinking.is_empty() {
                        for thinking_line in thinking.lines() {
                            all_lines.push(Line::from(vec![
                                Span::raw("  "),
                                Span::styled(
                                    format!("[thinking] {thinking_line}"),
                                    theme::thinking(),
                                ),
                            ]));
                        }
                    }

                    for tc in tool_calls {
                        let tc_text = render_tool_call(tc, tick_count);
                        for line in tc_text.lines {
                            let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
                            spans.extend(line.spans.into_iter());
                            all_lines.push(Line::from(spans));
                        }
                    }

                    if content.is_empty() && *is_streaming {
                        all_lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled("▌", theme::streaming_cursor()),
                        ]));
                    } else {
                        if !content.is_empty() {
                            let md_text = tui_markdown::from_str(content);
                            for line in md_text.lines {
                                let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
                                spans.extend(line.spans.into_iter().map(|s| {
                                    Span::styled(
                                        s.content.into_owned(),
                                        map_markdown_style(s.style),
                                    )
                                }));
                                all_lines.push(Line::from(spans));
                            }
                        }
                        if *is_streaming {
                            all_lines.push(Line::from(vec![
                                Span::raw("  "),
                                Span::styled("▌", theme::streaming_cursor()),
                            ]));
                        }
                    }

                    all_lines.push(Line::default());
                }
                ThreadMessage::System { content, .. } => {
                    all_lines.push(
                        Line::from(Span::styled(
                            format!("-- {content} --"),
                            Style::default()
                                .fg(theme::STEEL_GRAY)
                                .add_modifier(Modifier::DIM),
                        ))
                        .centered(),
                    );
                    all_lines.push(Line::default());
                }
            }
        }


        Text::from(all_lines)
    }


    pub fn render(&mut self, frame: &mut Frame, area: Rect, thread: &Thread, tick_count: usize) {
        let para_area = Rect::new(area.x, area.y, area.width.saturating_sub(1), area.height);
        let text = self.build_text(thread, tick_count);

        let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        self.total_lines = paragraph.line_count(para_area.width);

        self.on_new_content(area.height);

        let paragraph = paragraph.scroll((self.scroll_offset as u16, 0));
        frame.render_widget(paragraph, para_area);

        // Scrollbar on the rightmost 1 column.
        let mut scrollbar_state = ScrollbarState::new(
            self.total_lines.saturating_sub(area.height as usize),
        )
        .position(self.scroll_offset);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .thumb_style(Style::default().fg(theme::STEEL_GRAY))
                .track_style(Style::default().fg(theme::CHARCOAL)),
            area,
            &mut scrollbar_state,
        );


    }
}

impl Default for ThreadView {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_thread_with_user_message() -> Thread {
        let mut t = Thread::new("Test");
        t.add_user_message("hello world");
        t
    }

    #[test]
    fn test_thread_view_new_starts_at_bottom() {
        let view = ThreadView::new();
        assert!(view.at_bottom);
        assert_eq!(view.scroll_offset, 0);
    }

    #[test]
    fn test_thread_view_scroll_up_sets_not_at_bottom() {
        let mut view = ThreadView::new();
        view.total_lines = 50;
        view.scroll_up(5);
        assert!(!view.at_bottom);
        assert_eq!(view.scroll_offset, 0);
    }

    #[test]
    fn test_thread_view_scroll_down_clamps() {
        let mut view = ThreadView::new();
        view.total_lines = 20;
        view.scroll_down(100, 10);
        assert_eq!(view.scroll_offset, 10);
        assert!(view.at_bottom);
    }

    #[test]
    fn test_thread_view_on_new_content_scrolls_if_at_bottom() {
        let mut view = ThreadView::new();
        view.total_lines = 5;
        view.at_bottom = true;
        view.total_lines = 20;
        view.on_new_content(10);
        assert_eq!(view.scroll_offset, 10);
        assert!(view.at_bottom);
    }

    #[test]
    fn test_thread_view_on_new_content_doesnt_scroll_if_user_scrolled() {
        let mut view = ThreadView::new();
        view.scroll_offset = 5;
        view.at_bottom = false;
        view.total_lines = 20;
        view.on_new_content(10);
        assert_eq!(view.scroll_offset, 5);
        assert!(!view.at_bottom);
    }

    #[test]
    fn test_build_text_user_message_has_label() {
        let thread = make_thread_with_user_message();
        let view = ThreadView::new();
        let text = view.build_text(&thread, 0);
        let content = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(content.contains("you"));
        assert!(content.contains("hello world"));
    }

    #[test]
    fn test_build_text_assistant_message_renders() {
        let mut thread = Thread::new("Test");
        thread.start_assistant_message();
        if let Some(crate::thread::ThreadMessage::Assistant {
            content,
            is_streaming,
            ..
        }) = thread.messages.last_mut()
        {
            content.push_str("I can help with that.");
            *is_streaming = false;
        }
        let view = ThreadView::new();
        let text = view.build_text(&thread, 0);
        let content = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(content.contains("I can help with that."));
    }

    #[test]
    fn test_build_text_system_message_renders() {
        let mut thread = Thread::new("Test");
        thread.messages.push(crate::thread::ThreadMessage::System {
            content: "Session started".to_owned(),
            timestamp: chrono::Utc::now(),
        });
        let view = ThreadView::new();
        let text = view.build_text(&thread, 0);
        let content = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(content.contains("Session started"));
    }
}

#[test]
fn test_build_text_markdown_renders_bold() {
    let mut thread = Thread::new("Test");
    thread.start_assistant_message();
    if let Some(crate::thread::ThreadMessage::Assistant {
        content,
        is_streaming,
        ..
    }) = thread.messages.last_mut()
    {
        content.push_str("**bold text** here");
        *is_streaming = false;
    }
    let view = ThreadView::new();
    let text = view.build_text(&thread, 0);
    // tui-markdown should produce styled output (not necessarily same raw text)
    assert!(!text.lines.is_empty());
    // Content should be rendered (spans present)
    let has_content = text
        .lines
        .iter()
        .any(|l| !l.spans.is_empty() && l.spans.iter().any(|s| !s.content.is_empty()));
    assert!(
        has_content,
        "markdown content should produce non-empty spans"
    );
}

#[test]
fn test_build_text_streaming_cursor_present_during_stream() {
    let mut thread = Thread::new("Test");
    thread.start_assistant_message();
    if let Some(crate::thread::ThreadMessage::Assistant {
        content,
        is_streaming,
        ..
    }) = thread.messages.last_mut()
    {
        content.push_str("partial response");
        *is_streaming = true;
    }
    let view = ThreadView::new();
    let text = view.build_text(&thread, 0);
    let all_content = text
        .lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        all_content.contains('▌'),
        "streaming cursor should be present"
    );
}
