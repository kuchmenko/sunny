use crate::agent::{AgentMessage, Capability};
use crate::orchestrator::intent::Intent;
use crate::orchestrator::RequestId;
use crate::orchestrator::WorkspaceExtensions;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const ALLOWED_CAPABILITIES: &[&str] = &["query", "analyze", "action", "explore", "advise"];

#[derive(Debug, Clone)]
pub struct PlanningIntakeInput {
    pub intent: Intent,
    pub task: AgentMessage,
    pub request_id: RequestId,
    pub llm_enabled: bool,
    pub workspace_context: WorkspaceContext,
    pub workspace_extensions: WorkspaceExtensions,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceContext {
    pub cwd: Option<String>,
    pub query_root: Option<String>,
    pub is_git_repo: bool,
    pub has_cargo_toml: bool,
    pub has_package_json: bool,
    pub top_entries: Vec<String>,
}

impl WorkspaceContext {
    pub fn summarize(&self) -> String {
        let cwd = self.cwd.as_deref().unwrap_or("unknown");
        let query_root = self.query_root.as_deref().unwrap_or("unknown");
        let entries = if self.top_entries.is_empty() {
            "none".to_string()
        } else {
            self.top_entries.join(", ")
        };

        format!(
            "cwd={cwd}; query_root={query_root}; git_repo={}; cargo_toml={}; package_json={}; top_entries=[{entries}]",
            self.is_git_repo, self.has_cargo_toml, self.has_package_json
        )
    }
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
    async fn advise(
        &self,
        user_input: &str,
        workspace_context: &WorkspaceContext,
    ) -> Result<RawIntakeAdvice, IntakeAdvisorError>;
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
        let workspace_context = merge_workspace_context(
            input.workspace_context.clone(),
            derive_workspace_context(&input.task),
        );

        let verdict = if !input.llm_enabled {
            default_verdict()
        } else if let Some(advisor) = self.advisor.as_ref() {
            match advisor
                .advise(&input.intent.raw_input, &workspace_context)
                .await
            {
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
            verdict = verdict_label,
            workspace_context = %workspace_context.summarize()
        );

        verdict
    }
}

fn merge_workspace_context(
    explicit: WorkspaceContext,
    derived: WorkspaceContext,
) -> WorkspaceContext {
    WorkspaceContext {
        cwd: explicit.cwd.or(derived.cwd),
        query_root: explicit.query_root.or(derived.query_root),
        is_git_repo: explicit.is_git_repo || derived.is_git_repo,
        has_cargo_toml: explicit.has_cargo_toml || derived.has_cargo_toml,
        has_package_json: explicit.has_package_json || derived.has_package_json,
        top_entries: if explicit.top_entries.is_empty() {
            derived.top_entries
        } else {
            explicit.top_entries
        },
    }
}

fn derive_workspace_context(task: &AgentMessage) -> WorkspaceContext {
    let AgentMessage::Task {
        content, metadata, ..
    } = task;

    let cwd = metadata
        .get("_sunny.cwd")
        .cloned()
        .or_else(|| metadata.get("_sunny.workspace.cwd").cloned());
    let query_root = metadata
        .get("_sunny.query_root")
        .cloned()
        .or_else(|| metadata.get("_sunny.cwd").cloned())
        .or_else(|| {
            if content.trim().is_empty() {
                None
            } else {
                Some(content.clone())
            }
        });

    let scan_root = query_root
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| cwd.as_deref().map(PathBuf::from));

    let mut context = WorkspaceContext {
        cwd,
        query_root,
        ..WorkspaceContext::default()
    };

    if let Some(root) = scan_root {
        let normalized = normalize_scan_root(&root);
        context.is_git_repo = normalized.join(".git").exists();
        context.has_cargo_toml = normalized.join("Cargo.toml").exists();
        context.has_package_json = normalized.join("package.json").exists();
        context.top_entries = list_top_entries(&normalized);
    }

    context
}

fn normalize_scan_root(root: &Path) -> PathBuf {
    if root.is_dir() {
        root.to_path_buf()
    } else {
        root.parent().unwrap_or(root).to_path_buf()
    }
}

fn list_top_entries(root: &Path) -> Vec<String> {
    let mut names = match fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect::<Vec<_>>(),
        Err(_) => return Vec::new(),
    };

    names.sort();
    names.truncate(8);
    names
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
        async fn advise(
            &self,
            user_input: &str,
            _workspace_context: &WorkspaceContext,
        ) -> Result<RawIntakeAdvice, IntakeAdvisorError> {
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
            workspace_context: WorkspaceContext::default(),
            workspace_extensions: WorkspaceExtensions::common_extensions(),
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

    #[test]
    fn test_workspace_context_summary() {
        let context = WorkspaceContext {
            cwd: Some("/repo".to_string()),
            query_root: Some("/repo/src".to_string()),
            is_git_repo: true,
            has_cargo_toml: true,
            has_package_json: false,
            top_entries: vec!["Cargo.toml".to_string(), "src".to_string()],
        };
        let summary = context.summarize();
        assert!(summary.contains("cwd=/repo"));
        assert!(summary.contains("query_root=/repo/src"));
        assert!(summary.contains("git_repo=true"));
    }
}
