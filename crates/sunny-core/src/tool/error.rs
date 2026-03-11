use std::error::Error;

#[derive(thiserror::Error, Debug)]
pub enum ToolError {
    #[error("path not found: {path}")]
    PathNotFound { path: String },

    #[error("permission denied: {path}")]
    PermissionDenied { path: String },

    #[error("file too large: {path} ({size} bytes, limit {limit} bytes)")]
    FileTooLarge { path: String, size: u64, limit: u64 },

    #[error("scan limit exceeded: found {found} files, limit {limit}")]
    ScanLimitExceeded { found: usize, limit: usize },

    #[error("sensitive file denied: {path}")]
    SensitiveFileDenied { path: String },

    #[error("binary file skipped: {path}")]
    BinaryFileSkipped { path: String },

    #[error("directory read unsupported: {path}")]
    DirectoryReadUnsupported { path: String },

    #[error("tool execution failed: {source}")]
    ExecutionFailed {
        source: Box<dyn Error + Send + Sync>,
    },
}
