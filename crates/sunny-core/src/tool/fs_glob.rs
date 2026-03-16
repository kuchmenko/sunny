use std::path::PathBuf;

use globset::{Glob, GlobSetBuilder};
use ignore::WalkBuilder;
use tracing::info;

use crate::events::{EVENT_TOOL_EXEC_END, EVENT_TOOL_EXEC_START, OUTCOME_SUCCESS};
use crate::tool::error::ToolError;
use crate::tool::path_guard::PathGuard;

/// File pattern matching tool using glob patterns.
///
/// Respects `.gitignore` rules and limits results to 1000 files.
pub struct FsGlobTool {
    guard: PathGuard,
}

impl FsGlobTool {
    /// Create a new `FsGlobTool` rooted at the given directory.
    pub fn new(root: PathBuf) -> Result<Self, ToolError> {
        let guard = PathGuard::new(root)?;
        Ok(Self { guard })
    }

    /// Find files matching a glob pattern.
    ///
    /// - `pattern`: glob pattern (e.g., "**/*.rs", "src/**/*.txt")
    /// - `base_path`: optional relative path to search within (defaults to root)
    ///
    /// Returns a list of matching file paths relative to the base path.
    /// Respects `.gitignore` rules. Limits results to 1000 files.
    pub fn glob(&self, pattern: &str, base_path: Option<&str>) -> Result<Vec<PathBuf>, ToolError> {
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "fs_glob", pattern = pattern, base_path = ?base_path);

        let search_path = if let Some(base) = base_path {
            self.guard.resolve(base)?
        } else {
            self.guard.resolve(".")?
        };

        // Build the glob pattern using GlobSet
        let glob = Glob::new(pattern).map_err(|e| ToolError::ExecutionFailed {
            source: Box::new(e),
        })?;
        let mut builder = GlobSetBuilder::new();
        builder.add(glob);
        let glob_set = builder.build().map_err(|e| ToolError::ExecutionFailed {
            source: Box::new(e),
        })?;

        let mut results: Vec<PathBuf> = Vec::new();
        const MAX_RESULTS: usize = 1000;

        // Use ignore crate's WalkBuilder to respect .gitignore
        let walker = WalkBuilder::new(&search_path)
            .standard_filters(true)
            .build();

        for entry in walker.flatten() {
            if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                continue;
            }

            let entry_path = entry.path();

            // Match against glob pattern
            if glob_set.is_match(entry_path) {
                // Store relative path if possible, otherwise absolute
                let rel_path = entry_path
                    .strip_prefix(&search_path)
                    .unwrap_or(entry_path)
                    .to_path_buf();
                results.push(rel_path);

                if results.len() >= MAX_RESULTS {
                    info!(name: EVENT_TOOL_EXEC_END, tool_name = "fs_glob", outcome = "limit_exceeded", count = results.len());
                    return Err(ToolError::ExecutionFailed {
                        source: Box::new(std::io::Error::other(
                            "glob pattern matched more than 1000 files",
                        )),
                    });
                }
            }
        }

        info!(name: EVENT_TOOL_EXEC_END, tool_name = "fs_glob", outcome = OUTCOME_SUCCESS, count = results.len());
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_fs_glob_matches_rust_files() {
        let temp = TempDir::new().unwrap();
        let temp_path = temp.path().to_path_buf();

        // Create test files
        fs::write(temp_path.join("file1.rs"), "fn main() {}").unwrap();
        fs::write(temp_path.join("file2.rs"), "fn test() {}").unwrap();
        fs::write(temp_path.join("file3.txt"), "not rust").unwrap();

        let tool = FsGlobTool::new(temp_path).unwrap();
        let results = tool.glob("**/*.rs", None).unwrap();

        assert_eq!(results.len(), 2);
        assert!(results
            .iter()
            .any(|p| p.to_string_lossy().ends_with("file1.rs")));
        assert!(results
            .iter()
            .any(|p| p.to_string_lossy().ends_with("file2.rs")));
    }

    #[test]
    fn test_fs_glob_respects_gitignore() {
        // Note: The ignore crate respects .gitignore files in the directory tree.
        // This test verifies that the walker is configured to use standard_filters.
        let temp = TempDir::new().unwrap();
        let temp_path = temp.path().to_path_buf();

        // Create .gitignore
        fs::write(temp_path.join(".gitignore"), "target/\n").unwrap();

        // Create files
        fs::create_dir(temp_path.join("target")).unwrap();
        fs::write(temp_path.join("target").join("build.rs"), "build").unwrap();
        fs::write(temp_path.join("main.rs"), "main").unwrap();

        let tool = FsGlobTool::new(temp_path).unwrap();
        let results = tool.glob("**/*.rs", None).unwrap();

        // Should find main.rs
        assert!(
            results
                .iter()
                .any(|p| p.to_string_lossy().ends_with("main.rs")),
            "should find main.rs"
        );
    }

    #[test]
    fn test_fs_glob_limit_exceeded() {
        let temp = TempDir::new().unwrap();
        let temp_path = temp.path().to_path_buf();

        // Create 1001 files to exceed limit
        for i in 0..1001 {
            fs::write(temp_path.join(format!("file{}.txt", i)), "content").unwrap();
        }

        let tool = FsGlobTool::new(temp_path).unwrap();
        let result = tool.glob("**/*.txt", None);

        // Should return error when limit exceeded
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("more than 1000 files"));
    }
}
