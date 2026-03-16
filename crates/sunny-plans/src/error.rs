//! Plan domain errors.

#[derive(thiserror::Error, Debug)]
pub enum PlanError {
    #[error("plan {id} not found")]
    NotFound { id: String },
    #[error("invalid status transition: {status}")]
    InvalidStatus { status: String },
    #[error("dependency cycle detected")]
    CycleDetected,
    #[error("validation failed: {reason}")]
    ValidationFailed { reason: String },
    #[error("store error: {0}")]
    StoreError(#[from] rusqlite::Error),
    #[error("plan {id} already exists")]
    AlreadyExists { id: String },
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}
