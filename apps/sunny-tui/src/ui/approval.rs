use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub id: String,
    pub command: String,
    pub description: String,
}

pub struct ApprovalOverlay;

impl ApprovalOverlay {
    pub fn centered_rect(area: Rect) -> Rect {
        let popup_width = area.width * 6 / 10;
        let popup_height = 10.min(area.height.saturating_sub(4));
        let popup_x = area.x + (area.width.saturating_sub(popup_width)) / 2;
        let popup_y = area.y + (area.height.saturating_sub(popup_height)) / 2;
        Rect::new(popup_x, popup_y, popup_width, popup_height)
    }

    pub fn render(frame: &mut Frame, approval: &PendingApproval) {
        let area = frame.area();
        let popup_area = Self::centered_rect(area);

        frame.render_widget(
            Block::default().style(
                Style::default()
                    .bg(Color::Black)
                    .add_modifier(Modifier::DIM),
            ),
            area,
        );

        frame.render_widget(Clear, popup_area);

        let truncated_command: String = approval.command.chars().take(60).collect();
        let cmd_display = if approval.command.chars().count() > 60 {
            format!("{truncated_command}...")
        } else {
            truncated_command
        };

        let truncated_description: String = approval.description.chars().take(80).collect();
        let desc_display = if approval.description.chars().count() > 80 {
            format!("{truncated_description}...")
        } else {
            truncated_description
        };

        let content = Text::from(vec![
            Line::from(vec![
                Span::styled("Command: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(cmd_display, Style::default().fg(Color::Yellow)),
            ]),
            Line::default(),
            Line::from(vec![
                Span::styled("Reason: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(desc_display),
            ]),
            Line::default(),
            Line::from(vec![Span::styled(
                "[y] Approve  [n] Reject  [a] Always approve this tool",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]),
        ]);

        let paragraph = Paragraph::new(content)
            .block(
                Block::default()
                    .title(" Approval Required ")
                    .title_alignment(Alignment::Center)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red)),
            )
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, popup_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_centered_rect_within_area() {
        let area = Rect::new(0, 0, 100, 40);
        let popup = ApprovalOverlay::centered_rect(area);
        assert!(popup.width <= area.width);
        assert!(popup.height <= area.height);
        assert!(popup.x + popup.width <= area.right());
        assert!(popup.y + popup.height <= area.bottom());
    }

    #[test]
    fn test_pending_approval_constructs() {
        let approval = PendingApproval {
            id: "req-1".to_owned(),
            command: "rm -rf /tmp/test".to_owned(),
            description: "Cleaning up temp files".to_owned(),
        };
        assert_eq!(approval.id, "req-1");
        assert_eq!(approval.command, "rm -rf /tmp/test");
    }

    #[test]
    fn test_pending_approval_clone() {
        let approval = PendingApproval {
            id: "req-1".to_owned(),
            command: "ls".to_owned(),
            description: "List files".to_owned(),
        };
        let cloned = approval.clone();
        assert_eq!(cloned.id, approval.id);
    }
}
