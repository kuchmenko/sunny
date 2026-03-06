use crate::agent::Capability;

/// IntentKind classifies the type of intent being processed.
///
/// This enum represents the three primary intent categories in the planner:
/// - Analyze: Gather information or understand a problem
/// - Query: Request information or state
/// - Action: Perform a state-changing operation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentKind {
    /// Analyze intent: gather information or understand a problem
    Analyze,
    /// Query intent: request information or state
    Query,
    /// Action intent: perform a state-changing operation
    Action,
}

/// Intent represents a user's intent to be processed by the planner.
///
/// An intent combines a classification (kind), the raw user input, and an optional
/// capability requirement for routing to appropriate agents.
#[derive(Debug, Clone)]
pub struct Intent {
    /// The classification of this intent
    pub kind: IntentKind,
    /// The raw user input describing the intent
    pub raw_input: String,
    /// Optional capability requirement for routing
    pub required_capability: Option<Capability>,
}

/// PlanPolicy defines constraints for plan execution.
///
/// These limits prevent runaway planning and ensure bounded execution:
/// - max_depth: maximum nesting depth of plan steps
/// - max_steps: maximum total number of steps in a plan
/// - max_retries: maximum additional attempts (total attempts = max_retries + 1)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlanPolicy {
    /// Maximum nesting depth of plan steps
    pub max_depth: u32,
    /// Maximum total number of steps in a plan
    pub max_steps: u32,
    /// Maximum additional attempts after initial failure (total attempts = max_retries + 1)
    pub max_retries: u32,
}

impl Default for PlanPolicy {
    fn default() -> Self {
        Self {
            max_depth: 2,
            max_steps: 16,
            max_retries: 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intent_kind_variants_exist() {
        let _analyze = IntentKind::Analyze;
        let _query = IntentKind::Query;
        let _action = IntentKind::Action;
    }

    #[test]
    fn test_intent_kind_equality() {
        assert_eq!(IntentKind::Analyze, IntentKind::Analyze);
        assert_ne!(IntentKind::Analyze, IntentKind::Query);
        assert_ne!(IntentKind::Query, IntentKind::Action);
    }

    #[test]
    fn test_intent_creation() {
        let intent = Intent {
            kind: IntentKind::Query,
            raw_input: "What is the status?".to_string(),
            required_capability: None,
        };
        assert_eq!(intent.kind, IntentKind::Query);
        assert_eq!(intent.raw_input, "What is the status?");
        assert!(intent.required_capability.is_none());
    }

    #[test]
    fn test_intent_with_capability() {
        let cap = Capability("database_query".to_string());
        let intent = Intent {
            kind: IntentKind::Query,
            raw_input: "Get user count".to_string(),
            required_capability: Some(cap.clone()),
        };
        assert_eq!(intent.required_capability, Some(cap));
    }

    #[test]
    fn test_plan_policy_default_values() {
        let policy = PlanPolicy::default();
        assert_eq!(policy.max_depth, 2);
        assert_eq!(policy.max_steps, 16);
        assert_eq!(policy.max_retries, 2);
    }

    #[test]
    fn test_plan_policy_custom_values() {
        let policy = PlanPolicy {
            max_depth: 5,
            max_steps: 32,
            max_retries: 3,
        };
        assert_eq!(policy.max_depth, 5);
        assert_eq!(policy.max_steps, 32);
        assert_eq!(policy.max_retries, 3);
    }

    #[test]
    fn test_plan_policy_equality() {
        let policy1 = PlanPolicy::default();
        let policy2 = PlanPolicy {
            max_depth: 2,
            max_steps: 16,
            max_retries: 2,
        };
        assert_eq!(policy1, policy2);
    }
}
