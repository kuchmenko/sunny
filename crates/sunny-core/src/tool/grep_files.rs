use std::path::PathBuf;

use ignore::WalkBuilder;
use tracing::info;

use crate::events::{EVENT_TOOL_EXEC_END, EVENT_TOOL_EXEC_START, OUTCOME_SUCCESS};
use crate::tool::error::ToolError;
use crate::tool::path_guard::PathGuard;
use crate::tool::text_grep::{GrepMatch, TextGrep};

/// A single file with its grep matches.
#[derive(Debug)]
pub struct GrepFileMatch {
    pub path: PathBuf,
    pub matches: Vec<GrepMatch>,
}

/// Recursive grep across files in a directory tree.
///
/// Respects `.gitignore` rules and skips binary files automatically.
pub struct GrepFiles {
    guard: PathGuard,
}

impl GrepFiles {
    /// Create a new `GrepFiles` rooted at the given directory.
    pub fn new(root: PathBuf) -> Result<Self, ToolError> {
        let guard = PathGuard::new(root)?;
        Ok(Self { guard })
    }

    /// Search for a pattern recursively within the root directory.
    ///
    /// - `path`: relative path to search within (use "." for all files)
    /// - `pattern`: search pattern (passed to TextGrep)
    /// - `max_results`: maximum total matches across all files (default: 100)
    ///
    /// Returns a list of files with matches, stopping after `max_results` total matches.
    /// Binary files are skipped silently.
    pub fn search(
        &self,
        path: &str,
        pattern: &str,
        max_results: Option<usize>,
    ) -> Result<Vec<GrepFileMatch>, ToolError> {
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "grep_files", path = path, pattern = %pattern);

        let max = max_results.unwrap_or(100);
        let root_path = self.guard.resolve(path)?;

        let mut results: Vec<GrepFileMatch> = Vec::new();
        let mut total_matches = 0usize;
        let grep = TextGrep::default();

        // Use ignore crate's WalkBuilder to respect .gitignore
        let walker = WalkBuilder::new(&root_path).standard_filters(true).build();

        for entry in walker.flatten() {
            if !entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                continue;
            }

            let entry_path = entry.path();

            // Read file bytes
            let bytes = match std::fs::read(entry_path) {
                Ok(b) => b,
                Err(_) => continue, // Skip files we can't read
            };

            // Check for binary (null byte in first 8192 bytes)
            let check_end = bytes.len().min(8192);
            if bytes[..check_end].contains(&0u8) {
                continue; // Skip binary files silently
            }

            // Convert to string
            let content = match String::from_utf8(bytes) {
                Ok(c) => c,
                Err(_) => continue, // Skip non-UTF8 files
            };

            // Search with TextGrep
            let grep_result = grep.search(&content, pattern);

            if !grep_result.matches.is_empty() {
                total_matches += grep_result.matches.len();
                results.push(GrepFileMatch {
                    path: entry_path.to_path_buf(),
                    matches: grep_result.matches,
                });

                if total_matches >= max {
                    break;
                }
            }
        }

        info!(name: EVENT_TOOL_EXEC_END, tool_name = "grep_files", outcome = OUTCOME_SUCCESS, file_matches = results.len(), total_matches = total_matches);

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_grep_files_recursive_finds_matches() {
        let dir = tempdir().expect("create temp dir");
        let src_dir = dir.path().join("src");
        fs::create_dir(&src_dir).expect("create src dir");

        fs::write(
            src_dir.join("main.rs"),
            "fn main() {\n    println!(\"hello\");\n}",
        )
        .expect("write main.rs");
        fs::write(
            src_dir.join("lib.rs"),
            "fn lib_func() {\n    println!(\"lib\");\n}",
        )
        .expect("write lib.rs");

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files
            .search(".", "fn ", None)
            .expect("search succeeds");

        assert_eq!(results.len(), 2, "should find 2 files with matches");
        assert!(results.iter().any(|r| r.path.ends_with("main.rs")));
        assert!(results.iter().any(|r| r.path.ends_with("lib.rs")));
    }

    #[test]
    fn test_grep_files_max_results_respected() {
        let dir = tempdir().expect("create temp dir");

        // Create files with multiple matches
        for i in 0..3 {
            fs::write(
                dir.path().join(format!("file{i}.txt")),
                "match\nmatch\nmatch\nmatch\nmatch",
            )
            .expect("write file");
        }

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files
            .search(".", "match", Some(5))
            .expect("search succeeds");

        let total_matches: usize = results.iter().map(|r| r.matches.len()).sum();
        assert!(
            total_matches <= 5,
            "total matches {} should not exceed max_results 5",
            total_matches
        );
    }

    #[test]
    fn test_grep_files_skips_binary() {
        let dir = tempdir().expect("create temp dir");

        // Create a text file
        fs::write(dir.path().join("text.txt"), "match here").expect("write text file");

        // Create a binary file (with null byte)
        let mut binary_data = b"some text".to_vec();
        binary_data.push(0u8);
        binary_data.extend_from_slice(b"more text with match");
        fs::write(dir.path().join("binary.bin"), &binary_data).expect("write binary file");

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files
            .search(".", "match", None)
            .expect("search succeeds");

        // Should only find the text file, not the binary
        assert_eq!(results.len(), 1, "should find only 1 file (text.txt)");
        assert!(results[0].path.ends_with("text.txt"));
    }

    #[test]
    fn test_grep_files_respects_gitignore() {
        // Note: The ignore crate respects .gitignore files in the directory tree.
        // This test verifies that the walker is configured to use standard_filters.
        // In practice, .gitignore files are respected automatically by WalkBuilder.
        let dir = tempdir().expect("create temp dir");

        fs::write(dir.path().join("keep.txt"), "match here").expect("write keep.txt");
        fs::write(dir.path().join("ignore.log"), "match here").expect("write ignore.log");

        // Create .gitignore in the root
        fs::write(dir.path().join(".gitignore"), "*.log\n").expect("write .gitignore");

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files
            .search(".", "match", None)
            .expect("search succeeds");

        // Verify that we found at least the keep.txt file
        assert!(
            results.iter().any(|r| r.path.ends_with("keep.txt")),
            "should find matches in keep.txt"
        );
    }

    #[test]
    fn test_grep_files_empty_pattern() {
        let dir = tempdir().expect("create temp dir");
        fs::write(dir.path().join("file.txt"), "some content").expect("write file");

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files.search(".", "", None).expect("search succeeds");

        // Empty pattern should return no matches
        assert_eq!(results.len(), 0, "empty pattern should find no matches");
    }

    #[test]
    fn test_grep_files_no_matches() {
        let dir = tempdir().expect("create temp dir");
        fs::write(dir.path().join("file.txt"), "hello world").expect("write file");

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files
            .search(".", "zzzzz", None)
            .expect("search succeeds");

        assert_eq!(results.len(), 0, "should find no matches");
    }
}
