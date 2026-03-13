use std::path::PathBuf;

use tracing::info;

use crate::orchestrator::events::{
    EVENT_TOOL_EXEC_END, EVENT_TOOL_EXEC_ERROR, EVENT_TOOL_EXEC_START, OUTCOME_ERROR,
    OUTCOME_SUCCESS,
};
use crate::tool::{PathGuard, ToolError};

/// Result of a successful file edit.
#[derive(Debug)]
pub struct EditResult {
    pub path: PathBuf,
    pub replacements: usize,
}

/// Performs search-and-replace edits on files within a sandboxed root directory.
///
/// The `old_text` must match exactly once in the file — zero or multiple matches
/// are rejected to prevent ambiguous edits.
pub struct FileEditor {
    guard: PathGuard,
}

impl FileEditor {
    pub fn new(root: PathBuf) -> Result<Self, ToolError> {
        Ok(Self {
            guard: PathGuard::new(root)?,
        })
    }

    /// Replace the first (and only) occurrence of `old_text` with `new_text` in `path`.
    ///
    /// Returns `ToolError::ExecutionFailed` if:
    /// - `old_text` is not found (0 matches)
    /// - `old_text` matches more than once (ambiguous)
    pub fn edit(
        &self,
        path: &str,
        old_text: &str,
        new_text: &str,
    ) -> Result<EditResult, ToolError> {
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "fs_edit", path = path);

        let resolved = self.guard.resolve(path).map_err(|err| {
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_edit", outcome = OUTCOME_ERROR, error_kind = "PathGuard", error_message = %err);
            err
        })?;

        let content = std::fs::read_to_string(&resolved).map_err(|e| {
            let err = ToolError::ExecutionFailed {
                source: Box::new(e),
            };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_edit", outcome = OUTCOME_ERROR, error_kind = "ExecutionFailed", error_message = %err);
            err
        })?;

        let match_count = content.matches(old_text).count();

        if match_count == 0 {
            let err = ToolError::ExecutionFailed {
                source: "search text not found".into(),
            };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_edit", outcome = OUTCOME_ERROR, error_kind = "NotFound", error_message = %err);
            return Err(err);
        }

        if match_count > 1 {
            let err = ToolError::ExecutionFailed {
                source: format!(
                    "search text matches {match_count} times, must be unique \
                     — add surrounding context to disambiguate"
                )
                .into(),
            };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_edit", outcome = OUTCOME_ERROR, error_kind = "Ambiguous", error_message = %err);
            return Err(err);
        }

        let new_content = content.replacen(old_text, new_text, 1);

        std::fs::write(&resolved, &new_content).map_err(|e| {
            let err = ToolError::ExecutionFailed {
                source: Box::new(e),
            };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_edit", outcome = OUTCOME_ERROR, error_kind = "ExecutionFailed", error_message = %err);
            err
        })?;

        info!(name: EVENT_TOOL_EXEC_END, tool_name = "fs_edit", outcome = OUTCOME_SUCCESS, replacements = 1);

        Ok(EditResult {
            path: resolved,
            replacements: 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_with_file(content: &str) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let path = dir.path().join("test.rs");
        std::fs::write(&path, content).expect("test: write test file");
        (dir, "test.rs".to_string())
    }

    #[test]
    fn test_file_editor_single_match_replaces() {
        let (dir, filename) = setup_with_file("fn hello() {}\nfn world() {}");
        let editor = FileEditor::new(dir.path().to_path_buf()).expect("test: create editor");
        let result = editor
            .edit(&filename, "fn hello() {}", "fn greeting() {}")
            .expect("edit should succeed");
        assert_eq!(result.replacements, 1);
        let content = std::fs::read_to_string(dir.path().join("test.rs")).expect("test: read back");
        assert_eq!(content, "fn greeting() {}\nfn world() {}");
    }

    #[test]
    fn test_file_editor_zero_matches_errors() {
        let (dir, filename) = setup_with_file("fn hello() {}");
        let editor = FileEditor::new(dir.path().to_path_buf()).expect("test: create editor");
        let result = editor.edit(&filename, "fn nonexistent() {}", "fn bar() {}");
        assert!(result.is_err());
        let err_msg = result.expect_err("test: expected error").to_string();
        assert!(
            err_msg.contains("not found") || err_msg.contains("ExecutionFailed"),
            "got: {err_msg}"
        );
    }

    #[test]
    fn test_file_editor_multiple_matches_errors() {
        let (dir, filename) = setup_with_file("fn foo() {}\nfn foo() {}");
        let editor = FileEditor::new(dir.path().to_path_buf()).expect("test: create editor");
        let result = editor.edit(&filename, "fn foo() {}", "fn bar() {}");
        assert!(result.is_err());
        let err_msg = result.expect_err("test: expected error").to_string();
        assert!(
            err_msg.contains("2") || err_msg.contains("matches"),
            "got: {err_msg}"
        );
    }

    #[test]
    fn test_file_editor_reject_outside_root() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let editor = FileEditor::new(dir.path().to_path_buf()).expect("test: create editor");
        let result = editor.edit("/etc/hosts", "localhost", "evil");
        assert!(result.is_err());
    }
}
