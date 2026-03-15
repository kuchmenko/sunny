#[derive(thiserror::Error, Debug)]
pub enum TaskError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("store error: {0}")]
    Store(#[from] sunny_store::StoreError),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("workspace not found: no .git directory above {path}")]
    WorkspaceNotFound { path: String },
    #[error("task not found: {id}")]
    NotFound { id: String },
    #[error("invalid task status: {status}")]
    InvalidStatus { status: String },
    #[error("dependency cycle detected")]
    DependencyCycle,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
