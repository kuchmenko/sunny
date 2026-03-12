use crate::agent::{AgentMessage, Capability};
use crate::orchestrator::intent::{Intent, PlanPolicy};
use crate::orchestrator::WorkspaceExtensions;
use crate::orchestrator::{ExecutionPlan, OrchestratorError, PlanId, PlanStep, RequestId, StepId};
use crate::timeouts::{plan_context_timeout_ms, plan_stage_timeout_ms};
use std::collections::HashMap;
use std::str::FromStr;
use thiserror::Error;

/// Execution profile that maps to planning constraints.
///
/// Defines three predefined profiles (Low, Medium, High) that each map to specific
/// `PlanPolicy` constraints. Used for CLI configuration and runtime planning decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionProfile {
    /// Minimal planning: depth=1, steps=4, retries=2
    Low,
    /// Balanced planning (default): depth=2, steps=16, retries=2
    #[default]
    Medium,
    /// Aggressive planning: depth=3, steps=32, retries=2
    High,
}

impl ExecutionProfile {
    /// Convert profile to corresponding `PlanPolicy` constraints.
    pub fn to_policy(&self) -> PlanPolicy {
        match self {
            ExecutionProfile::Low => PlanPolicy {
                max_depth: 1,
                max_steps: 4,
                max_retries: 2,
            },
            ExecutionProfile::Medium => PlanPolicy {
                max_depth: 2,
                max_steps: 16,
                max_retries: 2,
            },
            ExecutionProfile::High => PlanPolicy {
                max_depth: 3,
                max_steps: 32,
                max_retries: 2,
            },
        }
    }
}

impl FromStr for ExecutionProfile {
    type Err = ExecutionProfileParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "low" => Ok(ExecutionProfile::Low),
            "medium" => Ok(ExecutionProfile::Medium),
            "high" => Ok(ExecutionProfile::High),
            _ => Err(ExecutionProfileParseError {
                input: s.to_string(),
            }),
        }
    }
}

impl std::fmt::Display for ExecutionProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutionProfile::Low => write!(f, "low"),
            ExecutionProfile::Medium => write!(f, "medium"),
            ExecutionProfile::High => write!(f, "high"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HeuristicLoopPlanner {
    policy: PlanPolicy,
    llm_enabled: bool,
}

impl HeuristicLoopPlanner {
    pub fn new(policy: PlanPolicy, llm_enabled: bool) -> Self {
        Self {
            policy,
            llm_enabled,
        }
    }

    pub fn policy(&self) -> PlanPolicy {
        self.policy
    }

    pub fn llm_enabled(&self) -> bool {
        self.llm_enabled
    }

    pub fn build_plan(
        &self,
        intent: Intent,
        task: AgentMessage,
        request_id: RequestId,
        hints: Option<crate::orchestrator::intake::PlanHints>,
    ) -> Result<ExecutionPlan, OrchestratorError> {
        let (task_id, content, mut metadata) = match task {
            AgentMessage::Task {
                id,
                content,
                metadata,
            } => (id, content, metadata),
        };

        if content.trim().is_empty() {
            return Err(OrchestratorError::PlanPolicyViolation {
                reason: "planner received empty task content".to_string(),
            });
        }

        metadata
            .entry("_sunny.planner.mode".to_string())
            .or_insert_with(|| {
                if self.llm_enabled {
                    "heuristic_llm_enabled".to_string()
                } else {
                    "heuristic_no_llm".to_string()
                }
            });

        // Inject intake hints metadata when present
        if let Some(ref plan_hints) = hints {
            if let Some(ref capability) = plan_hints.suggested_capability {
                metadata
                    .entry("_sunny.intake.suggested_capability".to_string())
                    .or_insert_with(|| capability.0.clone());
            }
            if let Some(ref complexity) = plan_hints.complexity_hint {
                metadata
                    .entry("_sunny.intake.complexity_hint".to_string())
                    .or_insert_with(|| format!("{:?}", complexity).to_lowercase());
            }
            // Merge metadata_overrides without overwriting existing _sunny.* keys
            for (key, value) in &plan_hints.metadata_overrides {
                if !key.starts_with("_sunny.") {
                    metadata.entry(key.clone()).or_insert_with(|| value.clone());
                }
            }
        }

        let ambiguous_query = is_ambiguous_query(&intent, &content);
        let planning_iterations = planning_iterations(self.policy, hints.as_ref());
        let primary_capability =
            resolve_primary_capability(&intent, hints.as_ref(), ambiguous_query);
        let should_run_evidence =
            should_run_evidence_stage(self.llm_enabled, hints.as_ref(), planning_iterations);
        let should_run_oracle_validation =
            should_run_oracle_validation(self.llm_enabled, hints.as_ref(), ambiguous_query);

        if ambiguous_query {
            metadata
                .entry("_sunny.planner.ambiguous_query".to_string())
                .or_insert_with(|| "true".to_string());
        }
        metadata
            .entry("_sunny.planner.iterations".to_string())
            .or_insert_with(|| planning_iterations.to_string());

        if let Some(ref plan_hints) = hints {
            if let Some(ref suggested) = plan_hints.suggested_capability {
                let from_capability = intent
                    .required_capability
                    .as_ref()
                    .map(|capability| capability.0.as_str())
                    .unwrap_or("query");
                if suggested.0 != primary_capability.0 {
                    tracing::info!(
                        event = "planner.capability.override",
                        from = %from_capability,
                        to = %primary_capability.0,
                        reason = "advisor_capability_rebalanced",
                        "planner rebalanced intake capability suggestion"
                    );
                }
            }
        }

        let mut plan = ExecutionPlan::new(
            PlanId::new().to_string(),
            request_id.to_string(),
            intent,
            self.policy,
        );

        let context_action = metadata
            .get("_sunny.query_root")
            .cloned()
            .or_else(|| metadata.get("_sunny.cwd").cloned())
            .unwrap_or_else(|| content.clone());
        let context_step_id = StepId::new().to_string();
        let mut context_metadata = with_task_id(metadata.clone(), task_id.clone());
        context_metadata.insert("_sunny.stage".to_string(), "context_gather".to_string());
        context_metadata.insert("_sunny.stage.iteration".to_string(), "1".to_string());
        plan.add_step(PlanStep::new_with_metadata(
            context_step_id.clone(),
            context_action,
            Some(Capability("query".to_string())),
            plan_context_timeout_ms(),
            context_metadata,
            Vec::new(),
        ))?;

        let mut dependencies = vec![context_step_id.clone()];
        if should_run_evidence {
            let evidence_step_id = StepId::new().to_string();
            let mut evidence_metadata = with_task_id(metadata.clone(), task_id.clone());
            evidence_metadata.insert("_sunny.stage".to_string(), "evidence_check".to_string());
            evidence_metadata.insert("_sunny.stage.iteration".to_string(), "2".to_string());
            plan.add_step(PlanStep::new_with_metadata(
                evidence_step_id.clone(),
                content.clone(),
                Some(Capability("explore".to_string())),
                plan_stage_timeout_ms(),
                evidence_metadata,
                vec![context_step_id],
            ))?;
            dependencies = vec![evidence_step_id];
        }

        let primary_step_id = StepId::new().to_string();
        let mut primary_metadata = with_task_id(metadata.clone(), task_id.clone());
        primary_metadata.insert("_sunny.stage".to_string(), "plan_finalize".to_string());
        primary_metadata.insert("_sunny.stage.iteration".to_string(), "3".to_string());
        plan.add_step(PlanStep::new_with_metadata(
            primary_step_id.clone(),
            content.clone(),
            Some(primary_capability),
            plan_stage_timeout_ms(),
            primary_metadata,
            dependencies,
        ))?;

        if should_run_oracle_validation {
            let mut oracle_metadata = with_task_id(metadata, task_id);
            oracle_metadata.insert("_sunny.stage".to_string(), "oracle_validation".to_string());
            oracle_metadata.insert("_sunny.stage.iteration".to_string(), "4".to_string());
            oracle_metadata.insert(
                "_sunny.oracle.role".to_string(),
                "advisory_validation".to_string(),
            );
            plan.add_step(PlanStep::new_with_metadata(
                StepId::new().to_string(),
                content,
                Some(Capability("advise".to_string())),
                plan_stage_timeout_ms(),
                oracle_metadata,
                vec![primary_step_id],
            ))?;
        }

        Ok(plan)
    }
}

fn planning_iterations(
    policy: PlanPolicy,
    hints: Option<&crate::orchestrator::intake::PlanHints>,
) -> u32 {
    let base = match hints.and_then(|value| value.complexity_hint) {
        Some(crate::orchestrator::intake::ComplexityHint::Low) => 2,
        Some(crate::orchestrator::intake::ComplexityHint::Medium) => 3,
        Some(crate::orchestrator::intake::ComplexityHint::High) => 4,
        None => 3,
    };

    let bounded_by_depth = policy.max_depth.saturating_add(1);
    base.min(bounded_by_depth).min(policy.max_steps.max(1))
}

fn should_run_evidence_stage(
    llm_enabled: bool,
    hints: Option<&crate::orchestrator::intake::PlanHints>,
    planning_iterations: u32,
) -> bool {
    if !llm_enabled {
        return false;
    }

    if planning_iterations < 3 {
        return false;
    }

    match hints.and_then(|value| value.complexity_hint) {
        Some(crate::orchestrator::intake::ComplexityHint::Low) => false,
        Some(crate::orchestrator::intake::ComplexityHint::Medium)
        | Some(crate::orchestrator::intake::ComplexityHint::High)
        | None => true,
    }
}

fn should_run_oracle_validation(
    llm_enabled: bool,
    hints: Option<&crate::orchestrator::intake::PlanHints>,
    ambiguous_query: bool,
) -> bool {
    if !llm_enabled {
        return false;
    }

    if !ambiguous_query {
        return false;
    }

    matches!(
        hints.and_then(|value| value.complexity_hint),
        Some(crate::orchestrator::intake::ComplexityHint::High)
            | Some(crate::orchestrator::intake::ComplexityHint::Medium)
    )
}

fn resolve_primary_capability(
    intent: &Intent,
    hints: Option<&crate::orchestrator::intake::PlanHints>,
    ambiguous_query: bool,
) -> Capability {
    let intent_capability = intent
        .required_capability
        .clone()
        .unwrap_or_else(|| Capability("query".to_string()));

    let Some(suggested) = hints.and_then(|value| value.suggested_capability.clone()) else {
        return intent_capability;
    };

    if suggested.0 == "advise" && ambiguous_query {
        return Capability("query".to_string());
    }

    suggested
}

fn is_ambiguous_query(intent: &Intent, content: &str) -> bool {
    if !matches!(intent.kind, crate::orchestrator::intent::IntentKind::Query) {
        return false;
    }

    let lowered = content.to_ascii_lowercase();
    let broad_terms = [
        "next steps",
        "what should",
        "ideas",
        "strategy",
        "roadmap",
        "how to improve",
    ];

    let has_broad_term = broad_terms.iter().any(|term| lowered.contains(term));
    let common_extensions = WorkspaceExtensions::common_extensions();
    let has_known_extension = lowered.split_whitespace().any(|token| {
        let candidate = token.trim_matches(|ch: char| {
            ch == ','
                || ch == ';'
                || ch == ':'
                || ch == ')'
                || ch == '('
                || ch == '"'
                || ch == '\''
                || ch == '`'
        });
        candidate
            .rsplit_once('.')
            .map(|(_, ext)| common_extensions.contains_extension(ext))
            .unwrap_or(false)
    });
    let has_explicit_path_hint = lowered.contains("/") || has_known_extension;

    has_broad_term && !has_explicit_path_hint
}

fn with_task_id(mut metadata: HashMap<String, String>, task_id: String) -> HashMap<String, String> {
    metadata
        .entry("_sunny.task_id".to_string())
        .or_insert(task_id);
    metadata
}

/// Error type for `ExecutionProfile` parsing failures.
#[derive(Error, Debug)]
#[error("invalid execution profile: '{input}' (expected 'low', 'medium', or 'high')")]
pub struct ExecutionProfileParseError {
    input: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_low_profile_policy() {
        let profile = ExecutionProfile::Low;
        let policy = profile.to_policy();
        assert_eq!(policy.max_depth, 1);
        assert_eq!(policy.max_steps, 4);
        assert_eq!(policy.max_retries, 2);
    }

    #[test]
    fn test_medium_profile_policy() {
        let profile = ExecutionProfile::Medium;
        let policy = profile.to_policy();
        assert_eq!(policy.max_depth, 2);
        assert_eq!(policy.max_steps, 16);
        assert_eq!(policy.max_retries, 2);
    }

    #[test]
    fn test_high_profile_policy() {
        let profile = ExecutionProfile::High;
        let policy = profile.to_policy();
        assert_eq!(policy.max_depth, 3);
        assert_eq!(policy.max_steps, 32);
        assert_eq!(policy.max_retries, 2);
    }

    #[test]
    fn test_default_profile_is_medium() {
        let default = ExecutionProfile::default();
        assert_eq!(default, ExecutionProfile::Medium);
        assert_eq!(default.to_policy().max_depth, 2);
    }

    #[test]
    fn test_profile_from_str() {
        assert_eq!(
            "low".parse::<ExecutionProfile>().unwrap(),
            ExecutionProfile::Low
        );
        assert_eq!(
            "medium".parse::<ExecutionProfile>().unwrap(),
            ExecutionProfile::Medium
        );
        assert_eq!(
            "high".parse::<ExecutionProfile>().unwrap(),
            ExecutionProfile::High
        );
    }

    #[test]
    fn test_profile_from_str_case_insensitive() {
        assert_eq!(
            "LOW".parse::<ExecutionProfile>().unwrap(),
            ExecutionProfile::Low
        );
        assert_eq!(
            "MeDiUm".parse::<ExecutionProfile>().unwrap(),
            ExecutionProfile::Medium
        );
        assert_eq!(
            "HIGH".parse::<ExecutionProfile>().unwrap(),
            ExecutionProfile::High
        );
    }

    #[test]
    fn test_profile_from_str_invalid() {
        let result = "invalid".parse::<ExecutionProfile>();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("invalid execution profile"));
    }

    #[test]
    fn test_profile_display() {
        assert_eq!(ExecutionProfile::Low.to_string(), "low");
        assert_eq!(ExecutionProfile::Medium.to_string(), "medium");
        assert_eq!(ExecutionProfile::High.to_string(), "high");
    }

    #[test]
    fn test_profile_equality() {
        assert_eq!(ExecutionProfile::Low, ExecutionProfile::Low);
        assert_ne!(ExecutionProfile::Low, ExecutionProfile::Medium);
    }

    #[test]
    fn test_profile_copy() {
        let profile = ExecutionProfile::Medium;
        let _copy = profile; // Copy trait allows this
        let _another = profile; // Can use again
        assert_eq!(profile, ExecutionProfile::Medium);
    }

    #[test]
    fn test_build_plan_with_none_hints_unchanged() {
        use crate::agent::AgentMessage;
        use crate::orchestrator::intent::{Intent, IntentKind};
        use crate::orchestrator::RequestId;
        use std::collections::HashMap;

        let planner = HeuristicLoopPlanner::new(PlanPolicy::default(), false);
        let intent = Intent {
            kind: IntentKind::Query,
            raw_input: "test".to_string(),
            required_capability: None,
        };
        let task = AgentMessage::Task {
            id: "task-1".to_string(),
            content: "test content".to_string(),
            metadata: HashMap::new(),
        };
        let request_id = RequestId::new();

        let plan = planner.build_plan(intent, task, request_id, None).unwrap();
        assert_eq!(plan.steps.len(), 2);

        // Verify no intake metadata when hints is None
        let step = &plan.steps[0];
        assert!(!step
            .metadata
            .contains_key("_sunny.intake.suggested_capability"));
        assert!(!step.metadata.contains_key("_sunny.intake.complexity_hint"));
    }

    #[test]
    fn test_build_plan_with_hints_injects_metadata() {
        use crate::agent::AgentMessage;
        use crate::agent::Capability;
        use crate::orchestrator::intake::{ComplexityHint, PlanHints};
        use crate::orchestrator::intent::{Intent, IntentKind};
        use crate::orchestrator::RequestId;
        use std::collections::HashMap;

        let planner = HeuristicLoopPlanner::new(PlanPolicy::default(), false);
        let intent = Intent {
            kind: IntentKind::Query,
            raw_input: "test".to_string(),
            required_capability: None,
        };
        let task = AgentMessage::Task {
            id: "task-1".to_string(),
            content: "test content".to_string(),
            metadata: HashMap::new(),
        };
        let request_id = RequestId::new();

        let hints = PlanHints {
            suggested_capability: Some(Capability("analyze".to_string())),
            complexity_hint: Some(ComplexityHint::High),
            context_tags: vec!["tag1".to_string()],
            metadata_overrides: {
                let mut map = HashMap::new();
                map.insert("custom_key".to_string(), "custom_value".to_string());
                map.insert(
                    "_sunny.should_not_be_added".to_string(),
                    "ignored".to_string(),
                );
                map
            },
        };

        let plan = planner
            .build_plan(intent, task, request_id, Some(hints))
            .unwrap();
        assert_eq!(plan.steps.len(), 2);

        let step = &plan.steps[0];
        // Verify intake metadata was injected
        assert_eq!(
            step.metadata.get("_sunny.intake.suggested_capability"),
            Some(&"analyze".to_string())
        );
        assert_eq!(
            step.metadata.get("_sunny.intake.complexity_hint"),
            Some(&"high".to_string())
        );
        // Verify custom metadata was merged
        assert_eq!(
            step.metadata.get("custom_key"),
            Some(&"custom_value".to_string())
        );
        // Verify _sunny.* keys from overrides are NOT added
        assert!(!step.metadata.contains_key("_sunny.should_not_be_added"));
    }
}

#[cfg(test)]
fn finalize_step(plan: &ExecutionPlan) -> &PlanStep {
    plan.steps
        .iter()
        .find(|step| step.metadata.get("_sunny.stage").map(String::as_str) == Some("plan_finalize"))
        .expect("plan_finalize step must exist")
}

#[test]
fn test_build_plan_uses_suggested_capability_over_intent() {
    use crate::agent::{AgentMessage, Capability};
    use crate::orchestrator::intake::PlanHints;
    use crate::orchestrator::intent::{Intent, IntentKind};
    use crate::orchestrator::RequestId;
    use std::collections::HashMap;

    let planner = HeuristicLoopPlanner::new(PlanPolicy::default(), false);
    let intent = Intent {
        kind: IntentKind::Query,
        raw_input: "test".to_string(),
        required_capability: Some(Capability("query".to_string())),
    };
    let task = AgentMessage::Task {
        id: "task-1".to_string(),
        content: "top 5 ideas for next steps".to_string(),
        metadata: HashMap::new(),
    };
    let request_id = RequestId::new();

    let hints = PlanHints {
        suggested_capability: Some(Capability("advise".to_string())),
        complexity_hint: None,
        context_tags: vec![],
        metadata_overrides: HashMap::new(),
    };

    let plan = planner
        .build_plan(intent, task, request_id, Some(hints))
        .unwrap();

    let step = finalize_step(&plan);
    // Should use intake suggestion, not intent capability
    assert_eq!(
        step.required_capability,
        Some(Capability("query".to_string()))
    );

    assert!(plan
        .steps
        .iter()
        .all(|step| step.required_capability != Some(Capability("advise".to_string()))));
}

#[test]
fn test_build_plan_falls_back_to_intent_when_no_suggestion() {
    use crate::agent::{AgentMessage, Capability};
    use crate::orchestrator::intake::PlanHints;
    use crate::orchestrator::intent::{Intent, IntentKind};
    use crate::orchestrator::RequestId;
    use std::collections::HashMap;

    let planner = HeuristicLoopPlanner::new(PlanPolicy::default(), false);
    let intent = Intent {
        kind: IntentKind::Query,
        raw_input: "test".to_string(),
        required_capability: Some(Capability("analyze".to_string())),
    };
    let task = AgentMessage::Task {
        id: "task-1".to_string(),
        content: "test content".to_string(),
        metadata: HashMap::new(),
    };
    let request_id = RequestId::new();

    let hints = PlanHints {
        suggested_capability: None,
        complexity_hint: None,
        context_tags: vec![],
        metadata_overrides: HashMap::new(),
    };

    let plan = planner
        .build_plan(intent, task, request_id, Some(hints))
        .unwrap();

    let step = finalize_step(&plan);
    // Should fall back to intent's capability
    assert_eq!(
        step.required_capability,
        Some(Capability("analyze".to_string()))
    );
}

#[test]
fn test_build_plan_defaults_to_query_when_no_capability() {
    use crate::agent::{AgentMessage, Capability};
    use crate::orchestrator::intake::PlanHints;
    use crate::orchestrator::intent::{Intent, IntentKind};
    use crate::orchestrator::RequestId;
    use std::collections::HashMap;

    let planner = HeuristicLoopPlanner::new(PlanPolicy::default(), false);
    let intent = Intent {
        kind: IntentKind::Query,
        raw_input: "test".to_string(),
        required_capability: None,
    };
    let task = AgentMessage::Task {
        id: "task-1".to_string(),
        content: "test content".to_string(),
        metadata: HashMap::new(),
    };
    let request_id = RequestId::new();

    let hints = PlanHints {
        suggested_capability: None,
        complexity_hint: None,
        context_tags: vec![],
        metadata_overrides: HashMap::new(),
    };

    let plan = planner
        .build_plan(intent, task, request_id, Some(hints))
        .unwrap();

    let step = finalize_step(&plan);
    // Should default to "query" when neither intent nor hints have capability
    assert_eq!(
        step.required_capability,
        Some(Capability("query".to_string()))
    );
}
