//! TUI interview presenter — sends questions to the TUI via mpsc, awaits answers via oneshot.
//!
//! Follows the same channel pattern as `GuiApprovalGate` in the GUI app.

use sunny_boys::InterviewPresenter;
use sunny_core::tool::{InterviewAnswer, InterviewQuestion, ToolError};
use tokio::sync::{mpsc, oneshot};

/// A request sent from the presenter to the bridge loop.
pub struct InterviewRequest {
    pub questions: Vec<InterviewQuestion>,
    pub response_tx: oneshot::Sender<Result<Vec<InterviewAnswer>, ToolError>>,
}

/// Interview presenter that relays questions through an mpsc channel
/// and blocks on a oneshot for the TUI's answers.
pub struct TuiInterviewPresenter {
    tx: mpsc::Sender<InterviewRequest>,
}

impl TuiInterviewPresenter {
    pub fn new() -> (Self, mpsc::Receiver<InterviewRequest>) {
        let (tx, rx) = mpsc::channel(4);
        (Self { tx }, rx)
    }
}

#[async_trait::async_trait]
impl InterviewPresenter for TuiInterviewPresenter {
    async fn present(
        &self,
        questions: Vec<InterviewQuestion>,
    ) -> Result<Vec<InterviewAnswer>, ToolError> {
        let (resp_tx, resp_rx) = oneshot::channel();
        self.tx
            .send(InterviewRequest {
                questions,
                response_tx: resp_tx,
            })
            .await
            .map_err(|_| ToolError::ExecutionFailed {
                source: Box::new(std::io::Error::other(
                    "interview presenter channel closed",
                )),
            })?;
        resp_rx.await.map_err(|_| ToolError::ExecutionFailed {
            source: Box::new(std::io::Error::other(
                "interview response channel dropped",
            )),
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sunny_core::tool::{InterviewOption, QuestionType};

    #[tokio::test]
    async fn test_tui_presenter_sends_and_resolves() {
        let (presenter, mut rx) = TuiInterviewPresenter::new();

        let questions = vec![InterviewQuestion {
            id: "q1".to_string(),
            text: "Pick one".to_string(),
            description: None,
            question_type: QuestionType::SingleChoice,
            options: vec![InterviewOption {
                label: "A".to_string(),
                description: None,
            }],
            header: None,
        }];

        let handle = tokio::spawn(async move { presenter.present(questions).await });

        let req = rx.recv().await.expect("should receive request");
        assert_eq!(req.questions.len(), 1);
        assert_eq!(req.questions[0].id, "q1");

        let answers = vec![InterviewAnswer {
            question_id: "q1".to_string(),
            value: "A".to_string(),
            selected_options: vec!["A".to_string()],
        }];
        req.response_tx.send(Ok(answers)).unwrap();

        let result = handle.await.unwrap().expect("should get answers");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].value, "A");
    }
}
