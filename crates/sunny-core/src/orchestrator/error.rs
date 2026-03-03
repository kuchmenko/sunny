use crate::agent::AgentError;

#[derive(thiserror::Error, Debug)]
pub enum OrchestratorError {
    #[error("agent not found: {name}")]
    AgentNotFound { name: String },

    #[error("dispatch failed")]
    DispatchFailed { source: AgentError },

    #[error("agent unresponsive")]
    AgentUnresponsive,

    #[error("orchestrator is shutting down")]
    ShuttingDown,
}

#[derive(thiserror::Error, Debug)]
pub enum RegistryError {
    #[error("duplicate agent name: {name}")]
    DuplicateName { name: String },
}
