use crate::agent::Capability;
use crate::orchestrator::error::OrchestratorError;
use crate::orchestrator::intent::{Intent, PlanPolicy};
use std::collections::HashMap;

/// StepState represents the lifecycle state of a plan step.
///
/// State machine transitions follow this pattern:
/// - Planned → Ready → Running → Completed (success path)
/// - Planned → Ready → Running → Failed (failure path)
/// - Any state → Cancelled (cancellation path)
/// - Any state → Skipped (skip path)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    /// Step has been created but not yet ready to execute
    Planned,
    /// Step is ready to execute (dependencies satisfied)
    Ready,
    /// Step is currently executing
    Running,
    /// Step completed successfully
    Completed,
    /// Step failed during execution
    Failed,
    /// Step was skipped (e.g., due to conditional logic)
    Skipped,
    /// Step was cancelled (e.g., due to orchestrator shutdown)
    Cancelled,
}

/// StepOutcome represents the result of a step execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepOutcome {
    /// Step succeeded with output content
    Success { content: String },
    /// Step failed with error message
    Error { message: String },
    /// Step execution timed out
    Timeout,
    /// Step was cancelled
    Cancelled,
}

/// PlanStep represents a single step in an execution plan.
///
/// Each step has:
/// - A unique identifier within the plan
/// - An action to perform (typically an agent capability invocation)
/// - An optional required capability for routing
/// - A timeout in milliseconds
/// - Current state in the step lifecycle
/// - Outcome (populated after execution)
/// - Attempt counter for retry tracking
#[derive(Debug, Clone)]
pub struct PlanStep {
    /// Unique identifier for this step within the plan
    pub step_id: String,
    /// The action to perform (e.g., agent method name or capability invocation)
    pub action: String,
    /// Optional capability requirement for routing to appropriate agent
    pub required_capability: Option<Capability>,
    /// Timeout in milliseconds for step execution
    pub timeout_ms: u64,
    /// Current state of this step
    pub state: StepState,
    /// Outcome of execution (populated after step completes)
    pub outcome: Option<StepOutcome>,
    /// Current attempt number (0-indexed)
    pub attempt: u32,
    /// Step metadata forwarded to agent task message
    pub metadata: HashMap<String, String>,
    pub depends_on: Vec<String>,
}

impl PlanStep {
    /// Creates a new plan step in Planned state.
    pub fn new(
        step_id: String,
        action: String,
        required_capability: Option<Capability>,
        timeout_ms: u64,
    ) -> Self {
        Self::new_with_metadata(
            step_id,
            action,
            required_capability,
            timeout_ms,
            HashMap::new(),
            Vec::new(),
        )
    }

    /// Creates a new plan step in Planned state with metadata.
    pub fn new_with_metadata(
        step_id: String,
        action: String,
        required_capability: Option<Capability>,
        timeout_ms: u64,
        metadata: HashMap<String, String>,
        depends_on: Vec<String>,
    ) -> Self {
        Self {
            step_id,
            action,
            required_capability,
            timeout_ms,
            state: StepState::Planned,
            outcome: None,
            attempt: 0,
            metadata,
            depends_on,
        }
    }

    pub fn is_ready_with(&self, completed_steps: &[String]) -> bool {
        self.depends_on
            .iter()
            .all(|dependency| completed_steps.iter().any(|done| done == dependency))
    }

    /// Transitions this step to a new state with validation.
    ///
    /// Valid transitions:
    /// - Planned → Ready, Cancelled, Skipped
    /// - Ready → Running, Cancelled, Skipped
    /// - Running → Completed, Failed, Cancelled, Timeout
    /// - Completed, Failed, Skipped, Cancelled → no further transitions
    ///
    /// Returns Err if the transition is invalid.
    pub fn transition(&mut self, new_state: StepState) -> Result<(), OrchestratorError> {
        let valid = match (self.state, new_state) {
            // From Planned
            (StepState::Planned, StepState::Ready) => true,
            (StepState::Planned, StepState::Cancelled) => true,
            (StepState::Planned, StepState::Skipped) => true,
            // From Ready
            (StepState::Ready, StepState::Running) => true,
            (StepState::Ready, StepState::Cancelled) => true,
            (StepState::Ready, StepState::Skipped) => true,
            // From Running
            (StepState::Running, StepState::Completed) => true,
            (StepState::Running, StepState::Failed) => true,
            (StepState::Running, StepState::Cancelled) => true,
            // Terminal states cannot transition
            (StepState::Completed, _) => false,
            (StepState::Failed, _) => false,
            (StepState::Skipped, _) => false,
            (StepState::Cancelled, _) => false,
            // All other transitions are invalid
            _ => false,
        };

        if valid {
            self.state = new_state;
            Ok(())
        } else {
            Err(OrchestratorError::InvalidStepTransition {
                from: self.state,
                to: new_state,
            })
        }
    }

    /// Sets the outcome of this step.
    pub fn set_outcome(&mut self, outcome: StepOutcome) {
        self.outcome = Some(outcome);
    }

    /// Increments the attempt counter.
    pub fn increment_attempt(&mut self) {
        self.attempt += 1;
    }
}

/// ExecutionPlan represents a complete plan for executing an intent.
///
/// A plan consists of:
/// - A unique plan identifier
/// - The request ID that triggered this plan
/// - The original intent being executed
/// - An ordered list of steps to execute
/// - Execution policy constraints
#[derive(Debug, Clone)]
pub struct ExecutionPlan {
    /// Unique identifier for this plan
    pub plan_id: String,
    /// Request ID that triggered this plan
    pub request_id: String,
    /// The original intent being executed
    pub intent: Intent,
    /// Ordered list of steps in this plan
    pub steps: Vec<PlanStep>,
    /// Execution policy constraints
    pub policy: PlanPolicy,
}

impl ExecutionPlan {
    /// Creates a new execution plan.
    pub fn new(plan_id: String, request_id: String, intent: Intent, policy: PlanPolicy) -> Self {
        Self {
            plan_id,
            request_id,
            intent,
            steps: Vec::new(),
            policy,
        }
    }

    /// Adds a step to this plan.
    ///
    /// Returns Err if adding the step would violate the plan policy
    /// (e.g., exceeding max_steps).
    pub fn add_step(&mut self, step: PlanStep) -> Result<(), OrchestratorError> {
        if self.steps.len() >= self.policy.max_steps as usize {
            return Err(OrchestratorError::PlanPolicyViolation {
                reason: format!(
                    "max_steps exceeded: {} >= {}",
                    self.steps.len(),
                    self.policy.max_steps
                ),
            });
        }
        self.steps.push(step);
        Ok(())
    }

    /// Returns the number of steps in this plan.
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    /// Returns a reference to a step by step_id, if it exists.
    pub fn get_step(&self, step_id: &str) -> Option<&PlanStep> {
        self.steps.iter().find(|s| s.step_id == step_id)
    }

    /// Returns a mutable reference to a step by step_id, if it exists.
    pub fn get_step_mut(&mut self, step_id: &str) -> Option<&mut PlanStep> {
        self.steps.iter_mut().find(|s| s.step_id == step_id)
    }

    pub fn validate_dependencies(&self) -> Result<(), OrchestratorError> {
        for step in &self.steps {
            for dependency in &step.depends_on {
                if self.get_step(dependency).is_none() {
                    return Err(OrchestratorError::PlanPolicyViolation {
                        reason: format!(
                            "step '{}' depends on missing step '{}'",
                            step.step_id, dependency
                        ),
                    });
                }
                if dependency == &step.step_id {
                    return Err(OrchestratorError::PlanPolicyViolation {
                        reason: format!("step '{}' cannot depend on itself", step.step_id),
                    });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_state_valid_transitions() {
        let mut step = PlanStep::new("step1".to_string(), "test_action".to_string(), None, 5000);

        // Planned → Ready
        assert!(step.transition(StepState::Ready).is_ok());
        assert_eq!(step.state, StepState::Ready);

        // Ready → Running
        assert!(step.transition(StepState::Running).is_ok());
        assert_eq!(step.state, StepState::Running);

        // Running → Completed
        assert!(step.transition(StepState::Completed).is_ok());
        assert_eq!(step.state, StepState::Completed);
    }

    #[test]
    fn test_step_state_invalid_transitions() {
        let mut step = PlanStep::new("step1".to_string(), "test_action".to_string(), None, 5000);

        // Planned → Running (skip Ready)
        assert!(step.transition(StepState::Running).is_err());
        assert_eq!(step.state, StepState::Planned);

        // Planned → Completed (skip Ready and Running)
        assert!(step.transition(StepState::Completed).is_err());
        assert_eq!(step.state, StepState::Planned);
    }

    #[test]
    fn test_step_state_terminal_states_no_transition() {
        let mut step = PlanStep::new("step1".to_string(), "test_action".to_string(), None, 5000);

        // Transition to Completed
        step.state = StepState::Completed;

        // Completed cannot transition to any state
        assert!(step.transition(StepState::Failed).is_err());
        assert!(step.transition(StepState::Cancelled).is_err());
        assert_eq!(step.state, StepState::Completed);
    }

    #[test]
    fn test_step_state_cancellation_from_any_state() {
        let states = vec![StepState::Planned, StepState::Ready, StepState::Running];

        for initial_state in states {
            let mut step =
                PlanStep::new("step1".to_string(), "test_action".to_string(), None, 5000);
            step.state = initial_state;

            // All non-terminal states can transition to Cancelled
            assert!(step.transition(StepState::Cancelled).is_ok());
            assert_eq!(step.state, StepState::Cancelled);
        }
    }

    #[test]
    fn test_step_outcome_setting() {
        let mut step = PlanStep::new("step1".to_string(), "test_action".to_string(), None, 5000);

        assert!(step.outcome.is_none());

        let outcome = StepOutcome::Success {
            content: "test result".to_string(),
        };
        step.set_outcome(outcome.clone());

        assert_eq!(step.outcome, Some(outcome));
    }

    #[test]
    fn test_step_attempt_counter() {
        let mut step = PlanStep::new("step1".to_string(), "test_action".to_string(), None, 5000);

        assert_eq!(step.attempt, 0);

        step.increment_attempt();
        assert_eq!(step.attempt, 1);

        step.increment_attempt();
        assert_eq!(step.attempt, 2);
    }

    #[test]
    fn test_execution_plan_creation() {
        let intent = Intent {
            kind: crate::orchestrator::intent::IntentKind::Query,
            raw_input: "test query".to_string(),
            required_capability: None,
        };

        let plan = ExecutionPlan::new(
            "plan1".to_string(),
            "req1".to_string(),
            intent.clone(),
            PlanPolicy::default(),
        );

        assert_eq!(plan.plan_id, "plan1");
        assert_eq!(plan.request_id, "req1");
        assert_eq!(plan.step_count(), 0);
    }

    #[test]
    fn test_execution_plan_add_step() {
        let intent = Intent {
            kind: crate::orchestrator::intent::IntentKind::Query,
            raw_input: "test query".to_string(),
            required_capability: None,
        };

        let mut plan = ExecutionPlan::new(
            "plan1".to_string(),
            "req1".to_string(),
            intent,
            PlanPolicy::default(),
        );

        let step = PlanStep::new("step1".to_string(), "action1".to_string(), None, 5000);

        assert!(plan.add_step(step).is_ok());
        assert_eq!(plan.step_count(), 1);
    }

    #[test]
    fn test_execution_plan_max_steps_policy() {
        let intent = Intent {
            kind: crate::orchestrator::intent::IntentKind::Query,
            raw_input: "test query".to_string(),
            required_capability: None,
        };

        let policy = PlanPolicy {
            max_depth: 2,
            max_steps: 2,
            max_retries: 2,
        };

        let mut plan = ExecutionPlan::new("plan1".to_string(), "req1".to_string(), intent, policy);

        // Add first step
        let step1 = PlanStep::new("step1".to_string(), "action1".to_string(), None, 5000);
        assert!(plan.add_step(step1).is_ok());

        // Add second step
        let step2 = PlanStep::new("step2".to_string(), "action2".to_string(), None, 5000);
        assert!(plan.add_step(step2).is_ok());

        // Third step should fail due to max_steps policy
        let step3 = PlanStep::new("step3".to_string(), "action3".to_string(), None, 5000);
        assert!(plan.add_step(step3).is_err());
    }

    #[test]
    fn test_execution_plan_get_step() {
        let intent = Intent {
            kind: crate::orchestrator::intent::IntentKind::Query,
            raw_input: "test query".to_string(),
            required_capability: None,
        };

        let mut plan = ExecutionPlan::new(
            "plan1".to_string(),
            "req1".to_string(),
            intent,
            PlanPolicy::default(),
        );

        let step = PlanStep::new("step1".to_string(), "action1".to_string(), None, 5000);
        plan.add_step(step).unwrap();

        let retrieved = plan.get_step("step1");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().step_id, "step1");

        let not_found = plan.get_step("nonexistent");
        assert!(not_found.is_none());
    }

    #[test]
    fn test_execution_plan_get_step_mut() {
        let intent = Intent {
            kind: crate::orchestrator::intent::IntentKind::Query,
            raw_input: "test query".to_string(),
            required_capability: None,
        };

        let mut plan = ExecutionPlan::new(
            "plan1".to_string(),
            "req1".to_string(),
            intent,
            PlanPolicy::default(),
        );

        let step = PlanStep::new("step1".to_string(), "action1".to_string(), None, 5000);
        plan.add_step(step).unwrap();

        let retrieved_mut = plan.get_step_mut("step1");
        assert!(retrieved_mut.is_some());

        let step_mut = retrieved_mut.unwrap();
        assert!(step_mut.transition(StepState::Ready).is_ok());
        assert_eq!(step_mut.state, StepState::Ready);
    }

    #[test]
    fn test_step_state_skip_from_planned() {
        let mut step = PlanStep::new("step1".to_string(), "test_action".to_string(), None, 5000);

        // Planned → Skipped
        assert!(step.transition(StepState::Skipped).is_ok());
        assert_eq!(step.state, StepState::Skipped);

        // Skipped is terminal
        assert!(step.transition(StepState::Ready).is_err());
    }

    #[test]
    fn test_step_state_failure_path() {
        let mut step = PlanStep::new("step1".to_string(), "test_action".to_string(), None, 5000);

        // Planned → Ready → Running → Failed
        assert!(step.transition(StepState::Ready).is_ok());
        assert!(step.transition(StepState::Running).is_ok());
        assert!(step.transition(StepState::Failed).is_ok());
        assert_eq!(step.state, StepState::Failed);

        // Failed is terminal
        assert!(step.transition(StepState::Running).is_err());
    }
}
