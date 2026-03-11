use std::error::Error;

#[derive(thiserror::Error, Debug)]
pub enum AgentError {
    #[error("agent not found: {id}")]
    NotFound { id: String },

    #[error("execution failed: {source}")]
    ExecutionFailed {
        source: Box<dyn Error + Send + Sync>,
    },

    #[error("operation timed out")]
    Timeout,
}
