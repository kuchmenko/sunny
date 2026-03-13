use std::path::{Path, PathBuf};

use ignore::gitignore::GitignoreBuilder;
use std::sync::OnceLock;
use tracing::{debug, info, warn};
use walkdir::WalkDir;

use super::error::ToolError;
use crate::events::{
    EVENT_TOOL_EXEC_END, EVENT_TOOL_EXEC_ERROR, EVENT_TOOL_EXEC_START, OUTCOME_ERROR,
    OUTCOME_SUCCESS,
};

static DEFAULT_MAX_FILES: OnceLock<usize> = OnceLock::new();
static DEFAULT_MAX_DEPTH: OnceLock<usize> = OnceLock::new();

pub(crate) fn default_max_files() -> usize {
    *DEFAULT_MAX_FILES.get_or_init(|| usize_from_env("SUNNY_DEFAULT_MAX_FILES", 10_000))
}

pub(crate) fn default_max_depth() -> usize {
    *DEFAULT_MAX_DEPTH.get_or_init(|| usize_from_env("SUNNY_DEFAULT_MAX_DEPTH", 50))
}

fn usize_from_env(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}
const DEFAULT_IGNORE_DIRS: &[&str] = &[".git", "target", "node_modules", ".sisyphus/evidence"];

/// Recursively scans directories collecting file metadata.
///
/// Respects configurable limits on file count and directory depth,
/// and skips common non-source directories (`.git`, `target`, etc.).
pub struct FileScanner {
    pub max_files: usize,
    pub max_depth: usize,
    pub ignore_dirs: Vec<String>,
}

/// Metadata for a single scanned file.
#[derive(Debug)]
pub struct ScannedFile {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub extension: Option<String>,
}

/// Result of a filesystem scan.
#[derive(Debug)]
pub struct ScanResult {
    pub files: Vec<ScannedFile>,
    pub truncated: bool,
    pub total_size_bytes: u64,
}

impl Default for FileScanner {
    fn default() -> Self {
        Self {
            max_files: default_max_files(),
            max_depth: default_max_depth(),
            ignore_dirs: DEFAULT_IGNORE_DIRS
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }
}

impl FileScanner {
    /// Scan a directory recursively, returning metadata for all files found.
    ///
    /// Returns `ToolError::PathNotFound` if the given path does not exist.
    /// Sets `ScanResult::truncated = true` if `max_files` limit is reached.
    pub fn scan(&self, path: &Path) -> Result<ScanResult, ToolError> {
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "fs_scan", path = %path.display());

        if !path.exists() {
            info!(
                name: EVENT_TOOL_EXEC_ERROR,
                tool_name = "fs_scan",
                outcome = OUTCOME_ERROR,
                error_kind = "PathNotFound"
            );
            return Err(ToolError::PathNotFound {
                path: path.display().to_string(),
            });
        }

        let ignore_dirs = &self.ignore_dirs;
        let mut files = Vec::new();
        let mut total_size_bytes: u64 = 0;
        let mut truncated = false;

        let gitignore = {
            let mut builder = GitignoreBuilder::new(path);
            for entry in WalkDir::new(path)
                .max_depth(self.max_depth)
                .into_iter()
                .filter_map(Result::ok)
            {
                if !entry.file_type().is_file() || entry.file_name() != ".gitignore" {
                    continue;
                }

                if let Some(error) = builder.add(entry.path()) {
                    warn!(
                        gitignore_path = %entry.path().display(),
                        error = %error,
                        "failed to load .gitignore, continuing scan"
                    );
                }
            }
            builder
                .build()
                .map_err(|source| ToolError::ExecutionFailed {
                    source: Box::new(source),
                })?
        };

        debug!(
            path = %path.display(),
            max_files = self.max_files,
            max_depth = self.max_depth,
            "starting filesystem scan"
        );

        let walker = WalkDir::new(path)
            .max_depth(self.max_depth)
            .into_iter()
            .filter_entry(|entry| {
                let entry_path = entry.path();

                if gitignore
                    .matched_path_or_any_parents(entry_path, entry.file_type().is_dir())
                    .is_ignore()
                {
                    return false;
                }

                if entry.file_type().is_dir() {
                    !ignore_dirs
                        .iter()
                        .any(|pattern| entry_path.ends_with(Path::new(pattern.as_str())))
                } else {
                    true
                }
            });

        for entry in walker.filter_map(Result::ok) {
            if !entry.file_type().is_file() {
                continue;
            }

            let entry_path = entry.path();

            if files.len() >= self.max_files {
                truncated = true;
                break;
            }

            let size = match entry.metadata() {
                Ok(m) => m.len(),
                Err(_) => continue,
            };

            total_size_bytes = total_size_bytes.saturating_add(size);

            let extension = entry_path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|s| s.to_string());

            files.push(ScannedFile {
                path: entry_path.to_path_buf(),
                size_bytes: size,
                extension,
            });
        }

        debug!(
            file_count = files.len(),
            total_size = total_size_bytes,
            truncated,
            "filesystem scan complete"
        );

        let file_count = files.len();
        info!(
            name: EVENT_TOOL_EXEC_END,
            tool_name = "fs_scan",
            outcome = OUTCOME_SUCCESS,
            file_count,
            total_size_bytes,
            truncated
        );

        Ok(ScanResult {
            files,
            truncated,
            total_size_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_scan_simple_directory() {
        let dir = tempdir().expect("create temp dir");
        fs::write(dir.path().join("file1.txt"), "hello").expect("write file1");
        fs::write(dir.path().join("file2.rs"), "fn main() {}").expect("write file2");
        fs::write(dir.path().join("file3.md"), "# README").expect("write file3");

        let scanner = FileScanner::default();
        let result = scanner.scan(dir.path()).expect("scan succeeds");

        assert_eq!(result.files.len(), 3);
        assert!(!result.truncated);
        assert!(result.total_size_bytes > 0);

        let mut extensions: Vec<_> = result
            .files
            .iter()
            .filter_map(|f| f.extension.as_deref())
            .collect();
        extensions.sort();
        assert_eq!(extensions, vec!["md", "rs", "txt"]);
    }

    #[test]
    fn test_scan_respects_max_files_limit() {
        let dir = tempdir().expect("create temp dir");
        for i in 0..20 {
            fs::write(dir.path().join(format!("file{i}.txt")), "content").expect("write file");
        }

        let scanner = FileScanner {
            max_files: 5,
            ..Default::default()
        };
        let result = scanner.scan(dir.path()).expect("scan succeeds");

        assert_eq!(result.files.len(), 5);
        assert!(result.truncated);
    }

    #[test]
    fn test_scan_ignores_dotgit_target_nodemodules() {
        let dir = tempdir().expect("create temp dir");
        fs::write(dir.path().join("keep.txt"), "kept").expect("write keep");

        for ignored in &[".git", "target", "node_modules"] {
            let sub = dir.path().join(ignored);
            fs::create_dir_all(&sub).expect("create ignored dir");
            fs::write(sub.join("hidden.txt"), "hidden").expect("write hidden");
        }

        let scanner = FileScanner::default();
        let result = scanner.scan(dir.path()).expect("scan succeeds");

        assert_eq!(result.files.len(), 1);
        assert_eq!(
            result.files[0].path.file_name().expect("has filename"),
            "keep.txt"
        );
    }

    #[test]
    fn test_scan_respects_gitignore_rules() {
        let dir = tempdir().expect("create temp dir");
        fs::write(dir.path().join("keep.txt"), "kept").expect("write keep");
        fs::write(dir.path().join("ignored.log"), "ignored").expect("write ignored file");

        let ignored_dir = dir.path().join("ignored_dir");
        fs::create_dir_all(&ignored_dir).expect("create ignored dir");
        fs::write(ignored_dir.join("nested.txt"), "ignored nested").expect("write ignored nested");

        fs::write(dir.path().join(".gitignore"), "*.log\nignored_dir/\n")
            .expect("write .gitignore");

        let scanner = FileScanner::default();
        let result = scanner.scan(dir.path()).expect("scan succeeds");

        let names: Vec<String> = result
            .files
            .iter()
            .filter_map(|f| {
                f.path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(str::to_string)
            })
            .collect();

        assert!(names.contains(&"keep.txt".to_string()));
        assert!(!names.contains(&"ignored.log".to_string()));
        assert!(!names.contains(&"nested.txt".to_string()));
    }

    #[test]
    fn test_scan_ignores_sisyphus_evidence() {
        let dir = tempdir().expect("create temp dir");
        fs::write(dir.path().join("keep.txt"), "kept").expect("write keep");

        // .sisyphus/evidence/ should be skipped
        let evidence = dir.path().join(".sisyphus").join("evidence");
        fs::create_dir_all(&evidence).expect("create evidence dir");
        fs::write(evidence.join("log.txt"), "evidence data").expect("write evidence");

        // Files directly in .sisyphus/ should NOT be skipped
        fs::write(dir.path().join(".sisyphus").join("plan.md"), "plan content")
            .expect("write plan");

        let scanner = FileScanner::default();
        let result = scanner.scan(dir.path()).expect("scan succeeds");

        let names: Vec<&str> = result
            .files
            .iter()
            .filter_map(|f| f.path.file_name()?.to_str())
            .collect();
        assert_eq!(result.files.len(), 2);
        assert!(names.contains(&"keep.txt"));
        assert!(names.contains(&"plan.md"));
    }

    #[test]
    fn test_scan_nonexistent_path() {
        let scanner = FileScanner::default();
        let result = scanner.scan(Path::new("/nonexistent/path"));

        assert!(result.is_err());
        match result.unwrap_err() {
            ToolError::PathNotFound { path } => {
                assert_eq!(path, "/nonexistent/path");
            }
            other => panic!("expected PathNotFound, got: {other}"),
        }
    }

    #[test]
    fn test_scan_empty_directory() {
        let dir = tempdir().expect("create temp dir");

        let scanner = FileScanner::default();
        let result = scanner.scan(dir.path()).expect("scan succeeds");

        assert!(result.files.is_empty());
        assert!(!result.truncated);
        assert_eq!(result.total_size_bytes, 0);
    }

    #[test]
    fn test_scan_respects_max_depth() {
        let dir = tempdir().expect("create temp dir");

        let mut current = dir.path().to_path_buf();
        for i in 1..=10 {
            current = current.join(format!("level{i}"));
            fs::create_dir_all(&current).expect("create nested dir");
            fs::write(current.join(format!("depth{i}.txt")), "content").expect("write nested");
        }
        fs::write(dir.path().join("root.txt"), "root").expect("write root");

        // WalkDir depth: root=0, children=1, grandchildren=2...
        // max_depth=3 → root.txt(1), depth1.txt(2), depth2.txt(3); depth3.txt(4) excluded
        let scanner = FileScanner {
            max_depth: 3,
            ..Default::default()
        };
        let result = scanner.scan(dir.path()).expect("scan succeeds");

        assert_eq!(result.files.len(), 3, "only files up to depth 3");

        for file in &result.files {
            let rel = file
                .path
                .strip_prefix(dir.path())
                .expect("strip temp prefix");
            let depth = rel.components().count();
            assert!(
                depth <= 3,
                "file at relative depth {depth} exceeds max_depth=3: {rel:?}"
            );
        }
    }

    #[test]
    fn test_fs_scan_tracing() {
        let dir = tempdir().expect("create temp dir");
        fs::write(dir.path().join("file1.txt"), "hello").expect("write file1");
        fs::write(dir.path().join("file2.rs"), "fn main() {}").expect("write file2");

        let scanner = FileScanner::default();

        // Test successful scan emits structured events
        let result = scanner.scan(dir.path()).expect("scan succeeds");
        assert_eq!(result.files.len(), 2);
        assert!(!result.truncated);
        assert!(result.total_size_bytes > 0);

        // Test error path emits error event
        let result = scanner.scan(Path::new("/nonexistent/path"));
        assert!(result.is_err());
    }
}
