use crate::agent::{AgentMessage, Capability};
use crate::orchestrator::intent::{Intent, PlanPolicy};
use crate::orchestrator::{ExecutionPlan, OrchestratorError, PlanId, PlanStep, RequestId, StepId};
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

        // Prefer intake suggested_capability when available, otherwise fall back to classifier intent
        let required_capability = hints
            .as_ref()
            .and_then(|h| h.suggested_capability.clone())
            .or_else(|| intent.required_capability.clone())
            .unwrap_or_else(|| Capability("query".to_string()));

        // Log capability override when intake suggestion differs from classifier
        if let Some(ref plan_hints) = hints {
            if let Some(ref suggested) = plan_hints.suggested_capability {
                let intent_cap = intent
                    .required_capability
                    .as_ref()
                    .map(|c| c.0.as_str())
                    .unwrap_or("query");
                if suggested.0 != intent_cap {
                    tracing::info!(
                        event = "planner.capability.override",
                        from = %intent_cap,
                        to = %suggested.0,
                        "intake advisory overriding classifier capability"
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

        plan.add_step(PlanStep::new_with_metadata(
            StepId::new().to_string(),
            content,
            Some(required_capability),
            30_000,
            with_task_id(metadata, task_id),
        ))?;

        Ok(plan)
    }
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
        assert_eq!(plan.steps.len(), 1);

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
        assert_eq!(plan.steps.len(), 1);

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
