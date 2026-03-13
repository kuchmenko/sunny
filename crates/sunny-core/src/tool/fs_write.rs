use std::path::PathBuf;

use tracing::info;

use crate::orchestrator::events::{
    EVENT_TOOL_EXEC_END, EVENT_TOOL_EXEC_ERROR, EVENT_TOOL_EXEC_START, OUTCOME_ERROR,
    OUTCOME_SUCCESS,
};
use crate::tool::{PathGuard, ToolError};

/// Maximum file size for writes: 1 MiB.
const MAX_WRITE_BYTES: usize = 1_048_576;

/// Result of a successful file write.
#[derive(Debug)]
pub struct WriteResult {
    pub path: PathBuf,
    pub bytes_written: usize,
    pub created: bool,
}

/// Writes UTF-8 content to files within a sandboxed root directory.
pub struct FileWriter {
    guard: PathGuard,
}

impl FileWriter {
    pub fn new(root: PathBuf) -> Result<Self, ToolError> {
        Ok(Self {
            guard: PathGuard::new(root)?,
        })
    }

    /// Write `content` to `path` (relative to root or absolute within root).
    ///
    /// - Creates parent directories as needed
    /// - Rejects content larger than 1 MiB
    /// - Rejects paths outside root via `PathGuard`
    pub fn write(&self, path: &str, content: &str) -> Result<WriteResult, ToolError> {
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "fs_write", path = path);

        if content.len() > MAX_WRITE_BYTES {
            let err = ToolError::ContentTooLarge {
                path: path.to_string(),
                size: content.len(),
                limit: MAX_WRITE_BYTES,
            };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_write", outcome = OUTCOME_ERROR, error_kind = "ContentTooLarge", error_message = %err);
            return Err(err);
        }

        let resolved = self.guard.resolve_for_write(path).map_err(|err| {
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_write", outcome = OUTCOME_ERROR, error_kind = "PathGuard", error_message = %err);
            err
        })?;

        let created = !resolved.exists();

        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                let err = ToolError::ExecutionFailed {
                    source: Box::new(e),
                };
                info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_write", outcome = OUTCOME_ERROR, error_kind = "ExecutionFailed", error_message = %err);
                err
            })?;
        }

        std::fs::write(&resolved, content).map_err(|e| {
            let err = ToolError::ExecutionFailed {
                source: Box::new(e),
            };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_write", outcome = OUTCOME_ERROR, error_kind = "ExecutionFailed", error_message = %err);
            err
        })?;

        let bytes_written = content.len();
        info!(name: EVENT_TOOL_EXEC_END, tool_name = "fs_write", outcome = OUTCOME_SUCCESS, bytes_written = bytes_written, created = created);

        Ok(WriteResult {
            path: resolved,
            bytes_written,
            created,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> tempfile::TempDir {
        tempfile::tempdir().expect("test: create temp dir")
    }

    #[test]
    fn test_file_writer_write_new_file() {
        let dir = setup();
        let writer = FileWriter::new(dir.path().to_path_buf()).expect("test: create writer");
        let result = writer
            .write("hello.rs", "fn main() {}")
            .expect("write file");
        assert!(result.created);
        assert_eq!(result.bytes_written, 12);
        let content =
            std::fs::read_to_string(dir.path().join("hello.rs")).expect("test: read back");
        assert_eq!(content, "fn main() {}");
    }

    #[test]
    fn test_file_writer_overwrite_existing_file() {
        let dir = setup();
        let file = dir.path().join("existing.rs");
        std::fs::write(&file, "old content").expect("test: create existing");
        let writer = FileWriter::new(dir.path().to_path_buf()).expect("test: create writer");
        let result = writer
            .write("existing.rs", "new content")
            .expect("overwrite");
        assert!(!result.created, "should not be created=true for overwrite");
        let content = std::fs::read_to_string(&file).expect("test: read back");
        assert_eq!(content, "new content");
    }

    #[test]
    fn test_file_writer_creates_parent_dirs() {
        let dir = setup();
        let writer = FileWriter::new(dir.path().to_path_buf()).expect("test: create writer");
        let result = writer
            .write("deep/nested/file.rs", "content")
            .expect("write nested");
        assert!(result.created);
        assert!(dir.path().join("deep/nested/file.rs").exists());
    }

    #[test]
    fn test_file_writer_reject_outside_root() {
        let dir = setup();
        let writer = FileWriter::new(dir.path().to_path_buf()).expect("test: create writer");
        let result = writer.write("/etc/passwd", "hacked");
        assert!(result.is_err(), "should reject write outside root");
    }

    #[test]
    fn test_file_writer_reject_oversized_content() {
        let dir = setup();
        let writer = FileWriter::new(dir.path().to_path_buf()).expect("test: create writer");
        let big = "x".repeat(MAX_WRITE_BYTES + 1);
        let result = writer.write("big.txt", &big);
        assert!(matches!(
            result.expect_err("test: expected error"),
            ToolError::ContentTooLarge { .. }
        ));
    }
}
