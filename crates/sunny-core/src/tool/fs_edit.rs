use std::path::PathBuf;

use tracing::info;

use crate::events::{
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

#[derive(Clone, Copy, Debug)]
struct MatchOccurrence {
    start: usize,
    line: usize,
}

impl FileEditor {
    pub fn new(root: PathBuf) -> Result<Self, ToolError> {
        Ok(Self {
            guard: PathGuard::new(root)?,
        })
    }

    pub fn edit(
        &self,
        path: &str,
        old_text: &str,
        new_text: &str,
        line_hint: Option<usize>,
        context_before: Option<&str>,
        context_after: Option<&str>,
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

        let occurrences = collect_occurrences(&content, old_text);

        if occurrences.is_empty() {
            let err = ToolError::ExecutionFailed {
                source: "old_string not found in file".into(),
            };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "fs_edit", outcome = OUTCOME_ERROR, error_kind = "NotFound", error_message = %err);
            return Err(err);
        }

        let selected = select_match(
            &content,
            old_text,
            &occurrences,
            line_hint,
            context_before,
            context_after,
        )?;

        let replacement_end = selected.start + old_text.len();
        let mut new_content =
            String::with_capacity(content.len() - old_text.len() + new_text.len());
        new_content.push_str(&content[..selected.start]);
        new_content.push_str(new_text);
        new_content.push_str(&content[replacement_end..]);

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

fn collect_occurrences(content: &str, old_text: &str) -> Vec<MatchOccurrence> {
    content
        .match_indices(old_text)
        .map(|(start, _)| MatchOccurrence {
            start,
            line: content[..start]
                .bytes()
                .filter(|byte| *byte == b'\n')
                .count()
                + 1,
        })
        .collect()
}

fn select_match(
    content: &str,
    old_text: &str,
    occurrences: &[MatchOccurrence],
    line_hint: Option<usize>,
    context_before: Option<&str>,
    context_after: Option<&str>,
) -> Result<MatchOccurrence, ToolError> {
    if occurrences.len() == 1 {
        return Ok(occurrences[0]);
    }

    let mut candidates: Vec<MatchOccurrence> = occurrences.to_vec();

    if let Some(line) = line_hint {
        let within_window: Vec<MatchOccurrence> = candidates
            .iter()
            .copied()
            .filter(|occurrence| occurrence.line.abs_diff(line) <= 10)
            .collect();
        if !within_window.is_empty() {
            candidates = within_window;
        }
    }

    if context_before.is_some() || context_after.is_some() {
        candidates.retain(|occurrence| {
            let before = &content[..occurrence.start];
            let after = &content[(occurrence.start + old_text.len())..];
            let before_matches = context_before.is_none_or(|expected| before.ends_with(expected));
            let after_matches = context_after.is_none_or(|expected| after.starts_with(expected));
            before_matches && after_matches
        });
    }

    if candidates.is_empty() {
        return Err(ToolError::ExecutionFailed {
            source: "No matches satisfy provided line_hint/context constraints".into(),
        });
    }

    if candidates.len() == 1 {
        return Ok(candidates[0]);
    }

    if let Some(line) = line_hint {
        let nearest_distance = candidates
            .iter()
            .map(|occurrence| occurrence.line.abs_diff(line))
            .min()
            .unwrap_or(usize::MAX);
        let nearest: Vec<MatchOccurrence> = candidates
            .iter()
            .copied()
            .filter(|occurrence| occurrence.line.abs_diff(line) == nearest_distance)
            .collect();
        if nearest.len() == 1 {
            return Ok(nearest[0]);
        }
    }

    let lines = occurrences
        .iter()
        .map(|occurrence| occurrence.line.to_string())
        .collect::<Vec<_>>()
        .join(", ");

    Err(ToolError::ExecutionFailed {
        source: format!(
            "Multiple matches found at lines: {lines}. Provide line_hint to disambiguate."
        )
        .into(),
    })
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
    fn test_edit_simple_single_match() {
        let (dir, filename) = setup_with_file("fn hello() {}\nfn world() {}");
        let editor = FileEditor::new(dir.path().to_path_buf()).expect("test: create editor");
        let result = editor
            .edit(
                &filename,
                "fn hello() {}",
                "fn greeting() {}",
                None,
                None,
                None,
            )
            .expect("edit should succeed");
        assert_eq!(result.replacements, 1);
        let content = std::fs::read_to_string(dir.path().join("test.rs")).expect("test: read back");
        assert_eq!(content, "fn greeting() {}\nfn world() {}");
    }

    #[test]
    fn test_edit_with_line_hint_resolves_ambiguity() {
        let mut lines = Vec::new();
        for line in 1..=90 {
            if line == 10 || line == 42 || line == 80 {
                lines.push("target = old".to_string());
            } else {
                lines.push(format!("line {line}"));
            }
        }
        let (dir, filename) = setup_with_file(&lines.join("\n"));
        let editor = FileEditor::new(dir.path().to_path_buf()).expect("test: create editor");
        editor
            .edit(
                &filename,
                "target = old",
                "target = new",
                Some(42),
                None,
                None,
            )
            .expect("line_hint should select nearest match");

        let content = std::fs::read_to_string(dir.path().join("test.rs")).expect("test: read back");
        let changed_lines: Vec<usize> = content
            .lines()
            .enumerate()
            .filter_map(|(index, line)| (line == "target = new").then_some(index + 1))
            .collect();
        let unchanged_old: Vec<usize> = content
            .lines()
            .enumerate()
            .filter_map(|(index, line)| (line == "target = old").then_some(index + 1))
            .collect();

        assert_eq!(changed_lines, vec![42]);
        assert_eq!(unchanged_old, vec![10, 80]);
    }

    #[test]
    fn test_edit_with_context_validation() {
        let (dir, filename) = setup_with_file(
            "start-one\nfn target() {}\nend-one\n\nstart-two\nfn target() {}\nend-two\n",
        );
        let editor = FileEditor::new(dir.path().to_path_buf()).expect("test: create editor");

        editor
            .edit(
                &filename,
                "fn target() {}",
                "fn selected() {}",
                None,
                Some("start-two\n"),
                Some("\nend-two"),
            )
            .expect("context should disambiguate target block");

        let content = std::fs::read_to_string(dir.path().join("test.rs")).expect("test: read back");
        assert!(content.contains("start-one\nfn target() {}\nend-one"));
        assert!(content.contains("start-two\nfn selected() {}\nend-two"));
    }

    #[test]
    fn test_edit_returns_all_match_locations_on_ambiguity() {
        let (dir, filename) = setup_with_file("alpha\nX\nbeta\nX\ngamma\nX\n");
        let editor = FileEditor::new(dir.path().to_path_buf()).expect("test: create editor");
        let result = editor.edit(&filename, "X", "Y", None, None, None);

        assert!(result.is_err());
        let err_msg = result.expect_err("test: expected error").to_string();
        assert!(err_msg.contains("lines: 2, 4, 6"), "got: {err_msg}");
    }

    #[test]
    fn test_file_editor_zero_matches_errors() {
        let (dir, filename) = setup_with_file("fn hello() {}");
        let editor = FileEditor::new(dir.path().to_path_buf()).expect("test: create editor");
        let result = editor.edit(
            &filename,
            "fn nonexistent() {}",
            "fn bar() {}",
            None,
            None,
            None,
        );
        assert!(result.is_err());
        let err_msg = result.expect_err("test: expected error").to_string();
        assert!(
            err_msg.contains("old_string not found in file"),
            "got: {err_msg}"
        );
    }

    #[test]
    fn test_file_editor_reject_outside_root() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let editor = FileEditor::new(dir.path().to_path_buf()).expect("test: create editor");
        let result = editor.edit("/etc/hosts", "localhost", "evil", None, None, None);
        assert!(result.is_err());
    }
}
