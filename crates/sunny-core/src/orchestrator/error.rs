use super::plan::StepState;
use crate::agent::AgentError;

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    #[error("plan step limit exceeded: executed {executed}, max {max_steps}")]
    StepLimitExceeded { executed: usize, max_steps: u32 },

    #[error("plan depth limit exceeded: depth {depth}, max {max_depth}")]
    DepthLimitExceeded { depth: u32, max_depth: u32 },
}

#[derive(thiserror::Error, Debug)]
pub enum OrchestratorError {
    #[error("agent not found: {name}")]
    AgentNotFound { name: String },

    #[error("dispatch failed: {source}")]
    DispatchFailed { source: AgentError },

    #[error("agent unresponsive")]
    AgentUnresponsive,

    #[error("orchestrator is shutting down")]
    ShuttingDown,

    #[error("invalid step state transition: {from:?} -> {to:?}")]
    InvalidStepTransition { from: StepState, to: StepState },

    #[error("plan policy violation: {reason}")]
    PlanPolicyViolation { reason: String },

    #[error(transparent)]
    Plan {
        #[from]
        source: PlanError,
    },
}

#[derive(thiserror::Error, Debug)]
pub enum RegistryError {
    #[error("duplicate agent name: {name}")]
    DuplicateName { name: String },
}
