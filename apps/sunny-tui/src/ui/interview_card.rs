//! Interview card overlay — native ratatui widget for answering interview questions.
//!
//! Renders as a centered card above the thread view.
//! All questions are presented in a navigable card (Left/Right to move between questions).
//! User can go back and change previous answers before submitting.

use std::collections::HashSet;

use ratatui::{
    layout::{Alignment, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use sunny_core::tool::{InterviewAnswer, InterviewQuestion, QuestionType};

use super::theme;

/// Per-question answer state (may be partially filled).
#[derive(Debug, Clone)]
pub struct QuestionState {
    /// Currently highlighted option index (SingleChoice/MultiChoice).
    pub selected: usize,
    /// Toggled option indices (MultiChoice).
    pub multi_selected: HashSet<usize>,
    /// Free text input buffer (FreeText).
    pub free_text: String,
    /// Confirm answer value (Confirm).
    pub confirm_value: Option<bool>,
}

impl QuestionState {
    fn new() -> Self {
        Self {
            selected: 0,
            multi_selected: HashSet::new(),
            free_text: String::new(),
            confirm_value: None,
        }
    }
}

/// Stateful interview card holding all questions and their answer states.
pub struct InterviewCard {
    pub request_id: String,
    pub questions: Vec<InterviewQuestion>,
    pub states: Vec<QuestionState>,
    pub current_index: usize,
}

impl InterviewCard {
    pub fn new(id: String, questions: Vec<InterviewQuestion>) -> Self {
        let states = questions.iter().map(|_| QuestionState::new()).collect();
        Self {
            request_id: id,
            questions,
            states,
            current_index: 0,
        }
    }

    pub fn current_question(&self) -> &InterviewQuestion {
        &self.questions[self.current_index]
    }

    pub fn current_state(&self) -> &QuestionState {
        &self.states[self.current_index]
    }

    #[allow(dead_code)]
    pub fn current_state_mut(&mut self) -> &mut QuestionState {
        &mut self.states[self.current_index]
    }

    pub fn next_question(&mut self) {
        if self.current_index < self.questions.len().saturating_sub(1) {
            self.current_index += 1;
        }
    }

    pub fn prev_question(&mut self) {
        self.current_index = self.current_index.saturating_sub(1);
    }

    pub fn select_next(&mut self) {
        let q = &self.questions[self.current_index];
        let state = &mut self.states[self.current_index];
        if !q.options.is_empty() {
            state.selected = (state.selected + 1) % q.options.len();
        }
    }

    pub fn select_prev(&mut self) {
        let q = &self.questions[self.current_index];
        let state = &mut self.states[self.current_index];
        if !q.options.is_empty() {
            state.selected = if state.selected == 0 {
                q.options.len() - 1
            } else {
                state.selected - 1
            };
        }
    }

    pub fn toggle_multi(&mut self) {
        let state = &mut self.states[self.current_index];
        let idx = state.selected;
        if state.multi_selected.contains(&idx) {
            state.multi_selected.remove(&idx);
        } else {
            state.multi_selected.insert(idx);
        }
    }

    pub fn handle_char(&mut self, c: char) {
        let q = &self.questions[self.current_index];
        let state = &mut self.states[self.current_index];
        match q.question_type {
            QuestionType::FreeText => state.free_text.push(c),
            QuestionType::Confirm => match c {
                'y' | 'Y' => state.confirm_value = Some(true),
                'n' | 'N' => state.confirm_value = Some(false),
                _ => {}
            },
            _ => {}
        }
    }

    pub fn handle_backspace(&mut self) {
        let state = &mut self.states[self.current_index];
        state.free_text.pop();
    }

    pub fn is_last_question(&self) -> bool {
        self.current_index == self.questions.len().saturating_sub(1)
    }

    /// Build answers from all question states.
    pub fn collect_answers(&self) -> Vec<InterviewAnswer> {
        self.questions
            .iter()
            .zip(self.states.iter())
            .map(|(q, state)| match q.question_type {
                QuestionType::SingleChoice => {
                    let label = q
                        .options
                        .get(state.selected)
                        .map(|o| o.label.clone())
                        .unwrap_or_default();
                    InterviewAnswer {
                        question_id: q.id.clone(),
                        value: label.clone(),
                        selected_options: vec![label],
                    }
                }
                QuestionType::MultiChoice => {
                    let selected: Vec<String> = state
                        .multi_selected
                        .iter()
                        .filter_map(|&idx| q.options.get(idx).map(|o| o.label.clone()))
                        .collect();
                    InterviewAnswer {
                        question_id: q.id.clone(),
                        value: selected.join(", "),
                        selected_options: selected,
                    }
                }
                QuestionType::FreeText => InterviewAnswer {
                    question_id: q.id.clone(),
                    value: state.free_text.clone(),
                    selected_options: Vec::new(),
                },
                QuestionType::Confirm => InterviewAnswer {
                    question_id: q.id.clone(),
                    value: state.confirm_value.unwrap_or(false).to_string(),
                    selected_options: Vec::new(),
                },
            })
            .collect()
    }

    /// Render the card as a centered overlay.
    pub fn render(&self, frame: &mut Frame, area: Rect) {
        let q = self.current_question();
        let state = self.current_state();

        // Build content lines
        let mut lines: Vec<Line> = Vec::new();

        // Question text (bold cream)
        lines.push(Line::from(Span::styled(
            &q.text,
            Style::default()
                .fg(theme::CREAM)
                .add_modifier(Modifier::BOLD),
        )));

        // Description (from question.description or question.header)
        let desc = q
            .description
            .as_deref()
            .or(q.header.as_deref())
            .unwrap_or("");
        if !desc.is_empty() {
            lines.push(Line::from(Span::styled(desc, theme::thinking())));
        }

        lines.push(Line::default());

        // Render question-type-specific content
        match q.question_type {
            QuestionType::SingleChoice => {
                for (i, opt) in q.options.iter().enumerate() {
                    let is_selected = i == state.selected;
                    let marker = if is_selected { "\u{25b8} " } else { "  " };
                    let style = if is_selected {
                        Style::default()
                            .fg(theme::SUNNY_GOLD)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme::CREAM)
                    };
                    lines.push(Line::from(Span::styled(
                        format!("{marker}{}", opt.label),
                        style,
                    )));
                    if let Some(desc) = &opt.description {
                        lines.push(Line::from(Span::styled(
                            format!("    {desc}"),
                            theme::muted(),
                        )));
                    }
                }
            }
            QuestionType::MultiChoice => {
                for (i, opt) in q.options.iter().enumerate() {
                    let is_highlighted = i == state.selected;
                    let is_toggled = state.multi_selected.contains(&i);
                    let marker = if is_toggled { "\u{25c9} " } else { "\u{25cb} " };
                    let style = if is_highlighted {
                        Style::default()
                            .fg(theme::SUNNY_GOLD)
                            .add_modifier(Modifier::BOLD)
                    } else if is_toggled {
                        Style::default().fg(theme::CREAM)
                    } else {
                        theme::muted()
                    };
                    lines.push(Line::from(Span::styled(
                        format!("{marker}{}", opt.label),
                        style,
                    )));
                    if let Some(desc) = &opt.description {
                        lines.push(Line::from(Span::styled(
                            format!("    {desc}"),
                            theme::muted(),
                        )));
                    }
                }
            }
            QuestionType::FreeText => {
                let display = format!("{}\u{258c}", state.free_text);
                lines.push(Line::from(Span::styled(
                    display,
                    Style::default().fg(theme::CREAM),
                )));
            }
            QuestionType::Confirm => {
                let yes_style = match state.confirm_value {
                    Some(true) => Style::default()
                        .fg(theme::SUNNY_GOLD)
                        .add_modifier(Modifier::BOLD),
                    _ => theme::muted(),
                };
                let no_style = match state.confirm_value {
                    Some(false) => Style::default()
                        .fg(theme::SUNNY_GOLD)
                        .add_modifier(Modifier::BOLD),
                    _ => theme::muted(),
                };
                lines.push(Line::from(vec![
                    Span::styled("[Y] Yes", yes_style),
                    Span::raw("  "),
                    Span::styled("[N] No", no_style),
                ]));
            }
        }

        // Compute card dimensions
        let content_height = lines.len() as u16 + 4; // borders + footer padding
        let max_height = (area.height * 8 / 10).max(6);
        let card_height = content_height.min(max_height);
        let card_width = (area.width * 7 / 10).min(70).max(30);

        let card_x = area.x + (area.width.saturating_sub(card_width)) / 2;
        let card_y = area.y + (area.height.saturating_sub(card_height)) / 2;
        let card_area = Rect::new(card_x, card_y, card_width, card_height);

        frame.render_widget(Clear, card_area);

        let progress = format!(
            " interview \u{00b7} {}/{} ",
            self.current_index + 1,
            self.questions.len()
        );

        let hints = match q.question_type {
            QuestionType::SingleChoice | QuestionType::MultiChoice => {
                let nav = if self.questions.len() > 1 {
                    "\u{2190}/\u{2192} prev/next \u{00b7} "
                } else {
                    ""
                };
                let extra = if q.question_type == QuestionType::MultiChoice {
                    "space toggle \u{00b7} "
                } else {
                    ""
                };
                format!("{nav}\u{2191}\u{2193} select \u{00b7} {extra}enter confirm \u{00b7} esc cancel")
            }
            QuestionType::FreeText => {
                let nav = if self.questions.len() > 1 {
                    "\u{2190}/\u{2192} prev/next \u{00b7} "
                } else {
                    ""
                };
                format!("{nav}type answer \u{00b7} enter confirm \u{00b7} esc cancel")
            }
            QuestionType::Confirm => {
                let nav = if self.questions.len() > 1 {
                    "\u{2190}/\u{2192} prev/next \u{00b7} "
                } else {
                    ""
                };
                format!("{nav}y/n answer \u{00b7} enter confirm \u{00b7} esc cancel")
            }
        };

        let block = Block::default()
            .title(Span::styled(
                progress,
                Style::default()
                    .fg(theme::SUNNY_GOLD)
                    .add_modifier(Modifier::BOLD),
            ))
            .title_alignment(Alignment::Left)
            .title_bottom(Line::from(Span::styled(&hints, theme::hint())))
            .borders(Borders::ALL)
            .border_type(ratatui::widgets::BorderType::Rounded)
            .border_style(Style::default().fg(theme::SUNNY_GOLD));

        let paragraph = Paragraph::new(Text::from(lines))
            .block(block)
            .wrap(Wrap { trim: false });

        frame.render_widget(paragraph, card_area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sunny_core::tool::InterviewOption;

    fn make_single_choice_question(id: &str, text: &str, options: &[&str]) -> InterviewQuestion {
        InterviewQuestion {
            id: id.to_string(),
            text: text.to_string(),
            description: None,
            question_type: QuestionType::SingleChoice,
            options: options
                .iter()
                .map(|label| InterviewOption {
                    label: label.to_string(),
                    description: None,
                })
                .collect(),
            header: None,
        }
    }

    fn make_multi_choice_question(id: &str, text: &str, options: &[&str]) -> InterviewQuestion {
        InterviewQuestion {
            id: id.to_string(),
            text: text.to_string(),
            description: None,
            question_type: QuestionType::MultiChoice,
            options: options
                .iter()
                .map(|label| InterviewOption {
                    label: label.to_string(),
                    description: None,
                })
                .collect(),
            header: None,
        }
    }

    fn make_free_text_question(id: &str, text: &str) -> InterviewQuestion {
        InterviewQuestion {
            id: id.to_string(),
            text: text.to_string(),
            description: None,
            question_type: QuestionType::FreeText,
            options: Vec::new(),
            header: None,
        }
    }

    fn make_confirm_question(id: &str, text: &str) -> InterviewQuestion {
        InterviewQuestion {
            id: id.to_string(),
            text: text.to_string(),
            description: None,
            question_type: QuestionType::Confirm,
            options: Vec::new(),
            header: None,
        }
    }

    #[test]
    fn test_card_new_starts_at_first_question() {
        let card = InterviewCard::new(
            "r1".into(),
            vec![
                make_single_choice_question("q1", "Pick", &["A", "B"]),
                make_single_choice_question("q2", "Pick", &["C", "D"]),
            ],
        );
        assert_eq!(card.current_index, 0);
        assert_eq!(card.questions.len(), 2);
        assert_eq!(card.states.len(), 2);
    }

    #[test]
    fn test_card_next_prev_navigation() {
        let mut card = InterviewCard::new(
            "r1".into(),
            vec![
                make_single_choice_question("q1", "Q1", &["A"]),
                make_single_choice_question("q2", "Q2", &["B"]),
                make_single_choice_question("q3", "Q3", &["C"]),
            ],
        );
        assert_eq!(card.current_index, 0);
        card.next_question();
        assert_eq!(card.current_index, 1);
        card.next_question();
        assert_eq!(card.current_index, 2);
        card.prev_question();
        assert_eq!(card.current_index, 1);
    }

    #[test]
    fn test_card_prev_stops_at_first() {
        let mut card = InterviewCard::new(
            "r1".into(),
            vec![make_single_choice_question("q1", "Q1", &["A"])],
        );
        card.prev_question();
        assert_eq!(card.current_index, 0);
    }

    #[test]
    fn test_card_select_next_wraps() {
        let mut card = InterviewCard::new(
            "r1".into(),
            vec![make_single_choice_question("q1", "Q1", &["A", "B", "C"])],
        );
        assert_eq!(card.current_state().selected, 0);
        card.select_next();
        assert_eq!(card.current_state().selected, 1);
        card.select_next();
        assert_eq!(card.current_state().selected, 2);
        card.select_next();
        assert_eq!(card.current_state().selected, 0);
    }

    #[test]
    fn test_card_toggle_multi() {
        let mut card = InterviewCard::new(
            "r1".into(),
            vec![make_multi_choice_question("q1", "Pick", &["A", "B", "C"])],
        );
        card.toggle_multi(); // toggle index 0
        assert!(card.current_state().multi_selected.contains(&0));
        card.select_next();
        card.toggle_multi(); // toggle index 1
        assert!(card.current_state().multi_selected.contains(&1));
        card.select_prev();
        card.toggle_multi(); // untoggle index 0
        assert!(!card.current_state().multi_selected.contains(&0));
    }

    #[test]
    fn test_card_collect_answer_single_choice() {
        let mut card = InterviewCard::new(
            "r1".into(),
            vec![make_single_choice_question("q1", "Pick", &["A", "B"])],
        );
        card.select_next(); // select "B"
        let answers = card.collect_answers();
        assert_eq!(answers[0].question_id, "q1");
        assert_eq!(answers[0].value, "B");
        assert_eq!(answers[0].selected_options, vec!["B"]);
    }

    #[test]
    fn test_card_collect_answer_multi_choice() {
        let mut card = InterviewCard::new(
            "r1".into(),
            vec![make_multi_choice_question(
                "q1",
                "Pick",
                &["A", "B", "C"],
            )],
        );
        card.toggle_multi(); // toggle "A" (index 0)
        card.select_next();
        card.select_next();
        card.toggle_multi(); // toggle "C" (index 2)
        let answers = card.collect_answers();
        assert_eq!(answers[0].question_id, "q1");
        assert!(answers[0].selected_options.contains(&"A".to_string()));
        assert!(answers[0].selected_options.contains(&"C".to_string()));
        assert!(!answers[0].selected_options.contains(&"B".to_string()));
    }

    #[test]
    fn test_card_collect_answer_free_text() {
        let mut card = InterviewCard::new(
            "r1".into(),
            vec![make_free_text_question("q1", "Name?")],
        );
        card.handle_char('h');
        card.handle_char('i');
        let answers = card.collect_answers();
        assert_eq!(answers[0].value, "hi");
        assert!(answers[0].selected_options.is_empty());
    }

    #[test]
    fn test_card_collect_answer_confirm() {
        let mut card = InterviewCard::new(
            "r1".into(),
            vec![make_confirm_question("q1", "Sure?")],
        );
        card.handle_char('y');
        let answers = card.collect_answers();
        assert_eq!(answers[0].value, "true");
    }

    #[test]
    fn test_card_is_last_question() {
        let mut card = InterviewCard::new(
            "r1".into(),
            vec![
                make_single_choice_question("q1", "Q1", &["A"]),
                make_single_choice_question("q2", "Q2", &["B"]),
            ],
        );
        assert!(!card.is_last_question());
        card.next_question();
        assert!(card.is_last_question());
    }

    #[test]
    fn test_card_collect_answers_all_questions() {
        let mut card = InterviewCard::new(
            "r1".into(),
            vec![
                make_single_choice_question("q1", "Q1", &["A", "B"]),
                make_free_text_question("q2", "Q2"),
                make_confirm_question("q3", "Q3"),
            ],
        );
        // Answer q1
        card.select_next(); // "B"
        // Answer q2
        card.next_question();
        card.handle_char('x');
        // Answer q3
        card.next_question();
        card.handle_char('n');

        let answers = card.collect_answers();
        assert_eq!(answers.len(), 3);
        assert_eq!(answers[0].value, "B");
        assert_eq!(answers[1].value, "x");
        assert_eq!(answers[2].value, "false");
    }
}
