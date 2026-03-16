use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Question type for interview questions
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum QuestionType {
    SingleChoice,
    MultiChoice,
    FreeText,
    Confirm,
}

/// Option for choice-based questions
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InterviewOption {
    pub label: String,
    pub description: Option<String>,
}

/// Interview question structure
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InterviewQuestion {
    pub id: String,
    pub text: String,
    pub question_type: QuestionType,
    pub options: Vec<InterviewOption>,
    pub header: Option<String>,
}

/// Answer to an interview question
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InterviewAnswer {
    pub question_id: String,
    pub value: String,
    pub selected_options: Vec<String>,
}

/// Context for accumulating interview answers
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct InterviewContext {
    answers: HashMap<String, InterviewAnswer>,
}

impl InterviewContext {
    /// Create a new empty interview context
    pub fn new() -> Self {
        Self {
            answers: HashMap::new(),
        }
    }

    /// Add an answer to the context, keyed by question_id
    pub fn add_answer(&mut self, answer: InterviewAnswer) {
        self.answers.insert(answer.question_id.clone(), answer);
    }

    /// Serialize context to JSON for agent consumption
    pub fn to_json(&self) -> serde_json::Value {
        let mut answers_obj = serde_json::json!({});

        for (question_id, answer) in &self.answers {
            answers_obj[question_id] = serde_json::json!({
                "value": answer.value,
                "selected_options": answer.selected_options,
            });
        }

        serde_json::json!({
            "answers": answers_obj,
        })
    }
}

impl Default for InterviewContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_interview_context_accumulates_answers() {
        let mut ctx = InterviewContext::new();

        let answer1 = InterviewAnswer {
            question_id: "q1".to_string(),
            value: "yes".to_string(),
            selected_options: vec!["option_a".to_string()],
        };

        let answer2 = InterviewAnswer {
            question_id: "q2".to_string(),
            value: "multiple".to_string(),
            selected_options: vec!["opt1".to_string(), "opt2".to_string()],
        };

        ctx.add_answer(answer1.clone());
        ctx.add_answer(answer2.clone());

        // Verify both answers are stored
        assert_eq!(ctx.answers.len(), 2);
        assert_eq!(ctx.answers.get("q1").unwrap().value, "yes");
        assert_eq!(ctx.answers.get("q2").unwrap().value, "multiple");
        assert_eq!(
            ctx.answers.get("q2").unwrap().selected_options,
            vec!["opt1".to_string(), "opt2".to_string()]
        );
    }

    #[test]
    fn test_interview_context_serializes_to_json() {
        let mut ctx = InterviewContext::new();

        let answer1 = InterviewAnswer {
            question_id: "q1".to_string(),
            value: "answer_value".to_string(),
            selected_options: vec!["choice_a".to_string()],
        };

        let answer2 = InterviewAnswer {
            question_id: "q2".to_string(),
            value: "multi_answer".to_string(),
            selected_options: vec!["choice_1".to_string(), "choice_2".to_string()],
        };

        ctx.add_answer(answer1);
        ctx.add_answer(answer2);

        let json = ctx.to_json();

        // Verify JSON structure
        assert!(json["answers"]["q1"].is_object());
        assert_eq!(json["answers"]["q1"]["value"], "answer_value");
        assert_eq!(json["answers"]["q1"]["selected_options"][0], "choice_a");

        assert!(json["answers"]["q2"].is_object());
        assert_eq!(json["answers"]["q2"]["value"], "multi_answer");
        assert_eq!(json["answers"]["q2"]["selected_options"][0], "choice_1");
        assert_eq!(json["answers"]["q2"]["selected_options"][1], "choice_2");
    }
}
