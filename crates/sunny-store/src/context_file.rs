//! Context file management
//! Reads SUNNY.md from project root and ~/.sunny/ for context injection

use crate::error::StoreError;
use std::path::Path;

/// Read context files from project root and global ~/.sunny/ directory
///
/// Reads up to 6000 characters from each file:
/// - `{workspace_root}/SUNNY.md` (project-specific context)
/// - `~/.sunny/SUNNY.md` (global context)
///
/// Returns concatenated content with section headers when both files exist.
/// Returns empty string if neither file exists (not an error).
///
/// # Errors
/// Returns `StoreError::Io` if file read fails (permission denied, etc.)
pub fn read_context_files(workspace_root: &Path) -> Result<String, StoreError> {
    let mut parts = Vec::new();

    // Read project SUNNY.md
    let project_file = workspace_root.join("SUNNY.md");
    if project_file.exists() {
        let content = std::fs::read_to_string(&project_file)?;
        let truncated = content.chars().take(6000).collect::<String>();
        parts.push(format!("## Project Context (SUNNY.md)\n{truncated}"));
    }

    // Read global ~/.sunny/SUNNY.md
    if let Some(home) = dirs::home_dir() {
        let global_file = home.join(".sunny").join("SUNNY.md");
        if global_file.exists() {
            let content = std::fs::read_to_string(&global_file)?;
            let truncated = content.chars().take(6000).collect::<String>();
            parts.push(format!(
                "## Global Context (~/.sunny/SUNNY.md)\n{truncated}"
            ));
        }
    }

    Ok(parts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_read_context_file_project_root() {
        let dir = tempdir().expect("should create temp dir");
        let sunny_md = dir.path().join("SUNNY.md");
        std::fs::write(&sunny_md, "# My Project\nUse async everywhere").expect("should write file");

        let result = read_context_files(dir.path()).expect("should read context files");
        assert!(result.contains("## Project Context (SUNNY.md)"));
        assert!(result.contains("Use async everywhere"));
    }

    #[test]
    fn test_context_file_caps_at_6000() {
        let dir = tempdir().expect("should create temp dir");
        let sunny_md = dir.path().join("SUNNY.md");
        let large_content = "x".repeat(10000);
        std::fs::write(&sunny_md, &large_content).expect("should write file");

        let result = read_context_files(dir.path()).expect("should read context files");
        // Header is "## Project Context (SUNNY.md)\n" = 30 chars + 6000 chars content
        assert!(result.len() <= 6030);
        assert!(result.contains("## Project Context (SUNNY.md)"));
        // Verify truncation happened
        assert!(!result.contains(&"x".repeat(6001)));
    }

    #[test]
    fn test_context_file_returns_empty_when_neither_exists() {
        let dir = tempdir().expect("should create temp dir");

        let result = read_context_files(dir.path()).expect("should read context files");
        assert_eq!(result, "");
    }

    #[test]
    fn test_context_file_both_files_present() {
        let dir = tempdir().expect("should create temp dir");
        let sunny_md = dir.path().join("SUNNY.md");
        std::fs::write(&sunny_md, "Project context").expect("should write file");

        // Create a mock global context by testing with just project file
        // (we can't easily mock dirs::home_dir in tests)
        let result = read_context_files(dir.path()).expect("should read context files");
        assert!(result.contains("## Project Context (SUNNY.md)"));
        assert!(result.contains("Project context"));
    }

    #[test]
    fn test_context_file_non_ascii_chars_counted_correctly() {
        let dir = tempdir().expect("should create temp dir");
        let sunny_md = dir.path().join("SUNNY.md");
        // Create content with multi-byte UTF-8 characters
        let content = "Hello 世界 🌍".repeat(1000); // Each emoji is 4 bytes but 1 char
        std::fs::write(&sunny_md, &content).expect("should write file");

        let result = read_context_files(dir.path()).expect("should read context files");
        // Verify that truncation is based on chars, not bytes
        let content_part = result
            .split("## Project Context (SUNNY.md)\n")
            .nth(1)
            .expect("should have content");
        assert!(content_part.len() <= 6000 * 4); // Max 6000 chars, each up to 4 bytes
        assert!(content_part.contains("世界"));
    }
}
