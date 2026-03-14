//! Error types for sunny-store

#[derive(thiserror::Error, Debug)]
pub enum StoreError {
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("session not found: {id}")]
    NotFound { id: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("migration error: {0}")]
    Migration(String),
    #[error("grammar error: {0}")]
    Grammar(String),
    #[error("invalid data: {0}")]
    InvalidData(String),
}
