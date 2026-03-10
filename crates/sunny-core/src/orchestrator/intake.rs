use crate::agent::{AgentMessage, Capability};
use crate::orchestrator::intent::Intent;
use crate::orchestrator::RequestId;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct PlanningIntakeInput {
    pub intent: Intent,
    pub task: AgentMessage,
    pub request_id: RequestId,
    pub llm_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComplexityHint {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Default)]
pub struct PlanHints {
    pub suggested_capability: Option<Capability>,
    pub complexity_hint: Option<ComplexityHint>,
    pub context_tags: Vec<String>,
    pub metadata_overrides: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub enum PlanningIntakeVerdict {
    Proceed(PlanHints),
    Skip { reason: String },
}

#[derive(Debug, Clone)]
pub struct PlanningIntake;

impl PlanningIntake {
    pub fn new() -> Self {
        Self
    }

    pub fn evaluate(&self, input: PlanningIntakeInput) -> PlanningIntakeVerdict {
        let PlanningIntakeInput { task, .. } = input;

        let content = match task {
            AgentMessage::Task { content, .. } => content,
        };

        let verdict = if content.trim().is_empty() {
            PlanningIntakeVerdict::Skip {
                reason: "blank task content".to_string(),
            }
        } else {
            PlanningIntakeVerdict::Proceed(PlanHints::default())
        };

        let verdict_label = match &verdict {
            PlanningIntakeVerdict::Proceed(_) => "proceed",
            PlanningIntakeVerdict::Skip { .. } => "skip",
        };

        tracing::info!(
            event = "orchestrator.intake.evaluated",
            verdict = verdict_label
        );

        verdict
    }
}

impl Default for PlanningIntake {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::intent::IntentKind;

    fn make_input(content: &str) -> PlanningIntakeInput {
        PlanningIntakeInput {
            intent: Intent {
                kind: IntentKind::Query,
                raw_input: "query".to_string(),
                required_capability: None,
            },
            task: AgentMessage::Task {
                id: "task-1".to_string(),
                content: content.to_string(),
                metadata: HashMap::new(),
            },
            request_id: RequestId::new(),
            llm_enabled: false,
        }
    }

    #[test]
    fn test_intake_proceed_on_valid_input() {
        let intake = PlanningIntake::new();
        let verdict = intake.evaluate(make_input("build a plan"));

        match verdict {
            PlanningIntakeVerdict::Proceed(hints) => {
                assert!(hints.suggested_capability.is_none());
                assert!(hints.complexity_hint.is_none());
                assert!(hints.context_tags.is_empty());
                assert!(hints.metadata_overrides.is_empty());
            }
            PlanningIntakeVerdict::Skip { reason } => {
                panic!("expected Proceed, got Skip: {reason}")
            }
        }
    }

    #[test]
    fn test_intake_skip_on_blank() {
        let intake = PlanningIntake::new();
        let verdict = intake.evaluate(make_input("   \n\t  "));

        match verdict {
            PlanningIntakeVerdict::Skip { reason } => {
                assert_eq!(reason, "blank task content");
            }
            PlanningIntakeVerdict::Proceed(_) => {
                panic!("expected Skip, got Proceed")
            }
        }
    }

    #[test]
    fn test_plan_hints_default() {
        let hints = PlanHints::default();
        assert!(hints.suggested_capability.is_none());
        assert!(hints.complexity_hint.is_none());
        assert!(hints.context_tags.is_empty());
        assert!(hints.metadata_overrides.is_empty());
    }

    #[test]
    fn test_verdict_debug_display() {
        let proceed = PlanningIntakeVerdict::Proceed(PlanHints::default());
        let skip = PlanningIntakeVerdict::Skip {
            reason: "blank task content".to_string(),
        };

        let proceed_debug = format!("{:?}", proceed);
        let skip_debug = format!("{:?}", skip);

        assert!(proceed_debug.contains("Proceed"));
        assert!(skip_debug.contains("Skip"));
    }
}
