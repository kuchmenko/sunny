use crate::agent::{AgentMessage, Capability};
use crate::orchestrator::intent::Intent;
use crate::orchestrator::RequestId;
use std::collections::HashMap;
use std::sync::Arc;

const ALLOWED_CAPABILITIES: &[&str] = &["query", "analyze", "action", "explore", "advise"];

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
pub struct RawIntakeAdvice {
    pub suggested_capability: Option<String>,
    pub complexity_hint: Option<String>,
    pub context_tags: Vec<String>,
    pub reasoning: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum IntakeAdvisorError {
    #[error("advisor timeout")]
    Timeout,
    #[error("advisor connection failed: {0}")]
    ConnectionFailed(String),
    #[error("advisor parse failed: {0}")]
    ParseFailed(String),
    #[error("advisor returned invalid capability: {0}")]
    InvalidCapability(String),
}

#[async_trait::async_trait]
pub trait IntakeAdvisor: Send + Sync {
    async fn advise(&self, user_input: &str) -> Result<RawIntakeAdvice, IntakeAdvisorError>;
}

#[derive(Debug, Clone)]
pub enum PlanningIntakeVerdict {
    Proceed(PlanHints),
    Skip { reason: String },
}

impl std::fmt::Debug for dyn IntakeAdvisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<intake-advisor>")
    }
}

#[derive(Debug, Clone)]
pub struct PlanningIntake {
    advisor: Option<Arc<dyn IntakeAdvisor>>,
}

impl PlanningIntake {
    pub fn new(advisor: Option<Arc<dyn IntakeAdvisor>>) -> Self {
        Self { advisor }
    }

    pub async fn evaluate(&self, input: PlanningIntakeInput) -> PlanningIntakeVerdict {
        let default_verdict = || PlanningIntakeVerdict::Proceed(PlanHints::default());

        let verdict = if !input.llm_enabled {
            default_verdict()
        } else if let Some(advisor) = self.advisor.as_ref() {
            match advisor.advise(&input.intent.raw_input).await {
                Ok(raw_advice) => match parse_raw_advice(raw_advice) {
                    Ok(hints) => PlanningIntakeVerdict::Proceed(hints),
                    Err(error) => {
                        tracing::warn!(
                            event = "orchestrator.intake.advisor_error",
                            request_id = %input.request_id,
                            error = %error
                        );
                        default_verdict()
                    }
                },
                Err(error) => {
                    tracing::warn!(
                        event = "orchestrator.intake.advisor_error",
                        request_id = %input.request_id,
                        error = %error
                    );
                    default_verdict()
                }
            }
        } else {
            default_verdict()
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
        Self::new(None)
    }
}

fn parse_raw_advice(raw_advice: RawIntakeAdvice) -> Result<PlanHints, IntakeAdvisorError> {
    let suggested_capability = match raw_advice.suggested_capability {
        Some(capability) => {
            let normalized = capability.trim().to_ascii_lowercase();
            if normalized.is_empty() {
                None
            } else if ALLOWED_CAPABILITIES.contains(&normalized.as_str()) {
                Some(Capability(normalized))
            } else {
                return Err(IntakeAdvisorError::InvalidCapability(capability));
            }
        }
        None => None,
    };

    let complexity_hint = match raw_advice.complexity_hint {
        Some(hint) => {
            let normalized = hint.trim().to_ascii_lowercase();
            match normalized.as_str() {
                "" => None,
                "low" => Some(ComplexityHint::Low),
                "medium" => Some(ComplexityHint::Medium),
                "high" => Some(ComplexityHint::High),
                _ => {
                    return Err(IntakeAdvisorError::ParseFailed(format!(
                        "invalid complexity_hint: {}",
                        hint
                    )));
                }
            }
        }
        None => None,
    };

    let mut metadata_overrides = HashMap::new();
    if let Some(reasoning) = raw_advice.reasoning {
        if !reasoning.trim().is_empty() {
            metadata_overrides.insert("_sunny.intake.reasoning".to_string(), reasoning);
        }
    }

    Ok(PlanHints {
        suggested_capability,
        complexity_hint,
        context_tags: raw_advice.context_tags,
        metadata_overrides,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::intent::IntentKind;
    use std::sync::Mutex;

    #[derive(Debug)]
    struct StubAdvisor {
        advice: Result<RawIntakeAdvice, IntakeAdvisorError>,
        last_input: Mutex<Option<String>>,
    }

    fn clone_advisor_error(error: &IntakeAdvisorError) -> IntakeAdvisorError {
        match error {
            IntakeAdvisorError::Timeout => IntakeAdvisorError::Timeout,
            IntakeAdvisorError::ConnectionFailed(message) => {
                IntakeAdvisorError::ConnectionFailed(message.clone())
            }
            IntakeAdvisorError::ParseFailed(message) => {
                IntakeAdvisorError::ParseFailed(message.clone())
            }
            IntakeAdvisorError::InvalidCapability(capability) => {
                IntakeAdvisorError::InvalidCapability(capability.clone())
            }
        }
    }

    #[async_trait::async_trait]
    impl IntakeAdvisor for StubAdvisor {
        async fn advise(&self, user_input: &str) -> Result<RawIntakeAdvice, IntakeAdvisorError> {
            self.last_input
                .lock()
                .expect("stub advisor lock poisoned")
                .replace(user_input.to_string());
            match &self.advice {
                Ok(advice) => Ok(advice.clone()),
                Err(error) => Err(clone_advisor_error(error)),
            }
        }
    }

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

    #[tokio::test]
    async fn test_intake_proceed_on_valid_input() {
        let advisor = Arc::new(StubAdvisor {
            advice: Ok(RawIntakeAdvice {
                suggested_capability: Some("query".to_string()),
                complexity_hint: Some("high".to_string()),
                context_tags: vec!["prod".to_string()],
                reasoning: Some("requires fast retrieval".to_string()),
            }),
            last_input: Mutex::new(None),
        });
        let intake = PlanningIntake::new(Some(advisor.clone()));
        let mut input = make_input("build a plan");
        input.llm_enabled = true;
        input.intent.raw_input = "route this query".to_string();
        let verdict = intake.evaluate(input).await;

        match verdict {
            PlanningIntakeVerdict::Proceed(hints) => {
                assert_eq!(
                    hints.suggested_capability,
                    Some(Capability("query".to_string()))
                );
                assert_eq!(hints.complexity_hint, Some(ComplexityHint::High));
                assert_eq!(hints.context_tags, vec!["prod".to_string()]);
                assert_eq!(
                    hints.metadata_overrides.get("_sunny.intake.reasoning"),
                    Some(&"requires fast retrieval".to_string())
                );
            }
            PlanningIntakeVerdict::Skip { reason } => {
                panic!("expected Proceed, got Skip: {reason}")
            }
        }

        assert_eq!(
            advisor
                .last_input
                .lock()
                .expect("stub advisor lock poisoned")
                .as_deref(),
            Some("route this query")
        );
    }

    #[tokio::test]
    async fn test_intake_skip_on_blank() {
        let intake = PlanningIntake::new(None);
        let mut input = make_input("   \n\t  ");
        input.llm_enabled = true;
        let verdict = intake.evaluate(input).await;

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

    #[tokio::test]
    async fn test_plan_hints_default() {
        let hints = PlanHints::default();
        assert!(hints.suggested_capability.is_none());
        assert!(hints.complexity_hint.is_none());
        assert!(hints.context_tags.is_empty());
        assert!(hints.metadata_overrides.is_empty());
    }

    #[tokio::test]
    async fn test_verdict_debug_display() {
        let proceed = PlanningIntakeVerdict::Proceed(PlanHints::default());
        let skip = PlanningIntakeVerdict::Skip {
            reason: "blank task content".to_string(),
        };

        let proceed_debug = format!("{:?}", proceed);
        let skip_debug = format!("{:?}", skip);

        assert!(proceed_debug.contains("Proceed"));
        assert!(skip_debug.contains("Skip"));
    }

    #[tokio::test]
    async fn test_intake_with_none_advisor_returns_default_hints() {
        let intake = PlanningIntake::new(None);
        let mut input = make_input("build a plan");
        input.llm_enabled = true;

        let verdict = intake.evaluate(input).await;

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
}
