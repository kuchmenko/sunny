use std::sync::Arc;

use sunny_core::tool::{
    InterviewAnswer, InterviewContext, InterviewQuestion, QuestionType, ToolError,
};
use tokio::sync::Mutex;

/// Trait for presenting interview questions to the user.
///
/// Implementors can deliver questions via CLI prompts (`InterviewRunner`) or
/// through a TUI overlay (`TuiInterviewPresenter`).
#[async_trait::async_trait]
pub trait InterviewPresenter: Send + Sync {
    async fn present(
        &self,
        questions: Vec<InterviewQuestion>,
    ) -> Result<Vec<InterviewAnswer>, ToolError>;
}

pub struct InterviewRunner {
    context: Arc<Mutex<InterviewContext>>,
}

impl InterviewRunner {
    pub fn new() -> Self {
        Self {
            context: Arc::new(Mutex::new(InterviewContext::new())),
        }
    }

    pub fn context(&self) -> Arc<Mutex<InterviewContext>> {
        Arc::clone(&self.context)
    }

    pub async fn run(
        &self,
        questions: Vec<InterviewQuestion>,
    ) -> Result<Vec<InterviewAnswer>, ToolError> {
        let mut answers = Vec::with_capacity(questions.len());

        for question in questions {
            let answer = self.present_one(question).await?;
            let mut ctx = self.context.lock().await;
            ctx.add_answer(answer.clone());
            answers.push(answer);
        }

        Ok(answers)
    }

    async fn present_one(&self, question: InterviewQuestion) -> Result<InterviewAnswer, ToolError> {
        tokio::task::spawn_blocking(move || match question.question_type {
            QuestionType::SingleChoice => prompt_single_choice(question),
            QuestionType::MultiChoice => prompt_multi_choice(question),
            QuestionType::FreeText => prompt_free_text(question),
            QuestionType::Confirm => prompt_confirm(question),
        })
        .await
        .map_err(|error| ToolError::ExecutionFailed {
            source: Box::new(std::io::Error::other(error.to_string())),
        })?
    }
}

impl Default for InterviewRunner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl InterviewPresenter for InterviewRunner {
    async fn present(
        &self,
        questions: Vec<InterviewQuestion>,
    ) -> Result<Vec<InterviewAnswer>, ToolError> {
        self.run(questions).await
    }
}

fn cancelled_answer(question_id: String) -> InterviewAnswer {
    InterviewAnswer {
        question_id,
        value: "cancelled".to_string(),
        selected_options: Vec::new(),
    }
}

fn map_inquire_error(
    question_id: String,
    error: inquire::InquireError,
) -> Result<InterviewAnswer, ToolError> {
    match error {
        // ESC or Ctrl+C — graceful early exit, not an error.
        inquire::InquireError::OperationCanceled | inquire::InquireError::OperationInterrupted => {
            Ok(cancelled_answer(question_id))
        }
        other => Err(ToolError::ExecutionFailed {
            source: Box::new(std::io::Error::other(other.to_string())),
        }),
    }
}

fn prompt_single_choice(question: InterviewQuestion) -> Result<InterviewAnswer, ToolError> {
    let prompt = prompt_message(&question);
    let options = question
        .options
        .iter()
        .map(|option| option.label.clone())
        .collect::<Vec<_>>();
    match inquire::Select::new(&prompt, options).prompt() {
        Ok(selected) => Ok(InterviewAnswer {
            question_id: question.id,
            value: selected.clone(),
            selected_options: vec![selected],
        }),
        Err(e) => map_inquire_error(question.id, e),
    }
}

fn prompt_multi_choice(question: InterviewQuestion) -> Result<InterviewAnswer, ToolError> {
    let prompt = prompt_message(&question);
    let options = question
        .options
        .iter()
        .map(|option| option.label.clone())
        .collect::<Vec<_>>();
    match inquire::MultiSelect::new(&prompt, options).prompt() {
        Ok(selected) => Ok(InterviewAnswer {
            question_id: question.id,
            value: selected.join(", "),
            selected_options: selected,
        }),
        Err(e) => map_inquire_error(question.id, e),
    }
}

fn prompt_free_text(question: InterviewQuestion) -> Result<InterviewAnswer, ToolError> {
    let prompt = prompt_message(&question);
    match inquire::Text::new(&prompt).prompt() {
        Ok(value) => Ok(InterviewAnswer {
            question_id: question.id,
            value,
            selected_options: Vec::new(),
        }),
        Err(e) => map_inquire_error(question.id, e),
    }
}

fn prompt_confirm(question: InterviewQuestion) -> Result<InterviewAnswer, ToolError> {
    let prompt = prompt_message(&question);
    match inquire::Confirm::new(&prompt).prompt() {
        Ok(confirmed) => Ok(InterviewAnswer {
            question_id: question.id,
            value: confirmed.to_string(),
            selected_options: Vec::new(),
        }),
        Err(e) => map_inquire_error(question.id, e),
    }
}

fn prompt_message(question: &InterviewQuestion) -> String {
    match &question.header {
        Some(header) if !header.trim().is_empty() => format!("{header}\n\n{}", question.text),
        _ => question.text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_interview_multi_round_accumulation() {
        let runner = InterviewRunner::new();

        {
            let context = runner.context();
            let mut ctx = context.lock().await;
            ctx.add_answer(InterviewAnswer {
                question_id: "q1".to_string(),
                value: "answer1".to_string(),
                selected_options: Vec::new(),
            });
        }

        {
            let context = runner.context();
            let mut ctx = context.lock().await;
            ctx.add_answer(InterviewAnswer {
                question_id: "q2".to_string(),
                value: "answer2".to_string(),
                selected_options: vec!["option-a".to_string(), "option-b".to_string()],
            });
        }

        let context = runner.context();
        let ctx = context.lock().await;
        let json = ctx.to_json();

        assert_eq!(
            json["answers"].as_object().map(|answers| answers.len()),
            Some(2)
        );
        assert_eq!(json["answers"]["q1"]["value"], "answer1");
        assert_eq!(json["answers"]["q2"]["value"], "answer2");
        assert_eq!(json["answers"]["q2"]["selected_options"][0], "option-a");
        assert_eq!(json["answers"]["q2"]["selected_options"][1], "option-b");
    }
}
