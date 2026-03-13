use std::path::{Path, PathBuf};

use crate::tool::ToolError;

/// Path sandboxing guard that validates requested paths stay within a root directory.
///
/// Extracted from `ExploreAgent::resolve_tool_path()` for shared use across write tools.
pub struct PathGuard {
    root: PathBuf,
}

impl PathGuard {
    /// Create a new `PathGuard` rooted at `root`.
    ///
    /// The root is canonicalized on construction.
    pub fn new(root: PathBuf) -> Result<Self, ToolError> {
        let canonical_root = std::fs::canonicalize(&root).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => ToolError::PathNotFound {
                path: root.display().to_string(),
            },
            std::io::ErrorKind::PermissionDenied => ToolError::PermissionDenied {
                path: root.display().to_string(),
            },
            _ => ToolError::ExecutionFailed {
                source: Box::new(err),
            },
        })?;
        Ok(Self {
            root: canonical_root,
        })
    }

    /// Resolve and validate a requested path is within the root.
    ///
    /// - Relative paths are joined to the root
    /// - Absolute paths are checked to be within root
    /// - Path traversal attempts (../../etc) are rejected
    /// - `.git` component paths are rejected
    /// - Path must exist
    pub fn resolve(&self, requested: &str) -> Result<PathBuf, ToolError> {
        let candidate = self.to_candidate(requested);

        if !candidate.exists() {
            return Err(ToolError::PathNotFound {
                path: candidate.display().to_string(),
            });
        }

        let canonical = std::fs::canonicalize(&candidate).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => ToolError::PathNotFound {
                path: candidate.display().to_string(),
            },
            std::io::ErrorKind::PermissionDenied => ToolError::PermissionDenied {
                path: candidate.display().to_string(),
            },
            _ => ToolError::ExecutionFailed {
                source: Box::new(err),
            },
        })?;

        self.validate_within_root(&canonical)?;
        Ok(canonical)
    }

    /// Resolve a path for writing. The target file may not exist, but its parent must.
    ///
    /// Used by `FileWriter` to allow writing new files within the sandbox.
    pub fn resolve_for_write(&self, requested: &str) -> Result<PathBuf, ToolError> {
        let candidate = self.to_candidate(requested);

        if candidate.exists() {
            // If it exists, validate normally
            return self.resolve(requested);
        }

        // File doesn't exist — check parent exists
        let parent = candidate.parent().ok_or_else(|| ToolError::PathNotFound {
            path: candidate.display().to_string(),
        })?;

        if !parent.exists() {
            // Parent doesn't exist — validate root membership based on joined path
            // Normalize without canonicalize (file doesn't exist yet)
            let normalized = normalize_path(&candidate);
            if !normalized.starts_with(&self.root) {
                return Err(ToolError::SensitiveFileDenied {
                    path: candidate.display().to_string(),
                });
            }
            return Ok(normalized);
        }

        let canonical_parent = std::fs::canonicalize(parent).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => ToolError::PathNotFound {
                path: parent.display().to_string(),
            },
            std::io::ErrorKind::PermissionDenied => ToolError::PermissionDenied {
                path: parent.display().to_string(),
            },
            _ => ToolError::ExecutionFailed {
                source: Box::new(err),
            },
        })?;

        if !canonical_parent.starts_with(&self.root) {
            return Err(ToolError::SensitiveFileDenied {
                path: candidate.display().to_string(),
            });
        }

        if contains_git_component(&canonical_parent) {
            return Err(ToolError::SensitiveFileDenied {
                path: candidate.display().to_string(),
            });
        }

        Ok(canonical_parent.join(
            candidate
                .file_name()
                .ok_or_else(|| ToolError::PathNotFound {
                    path: candidate.display().to_string(),
                })?,
        ))
    }

    fn to_candidate(&self, requested: &str) -> PathBuf {
        let p = PathBuf::from(requested);
        if p.is_absolute() {
            p
        } else {
            self.root.join(p)
        }
    }

    fn validate_within_root(&self, canonical: &Path) -> Result<(), ToolError> {
        if !canonical.starts_with(&self.root) {
            return Err(ToolError::SensitiveFileDenied {
                path: canonical.display().to_string(),
            });
        }
        if contains_git_component(canonical) {
            return Err(ToolError::SensitiveFileDenied {
                path: canonical.display().to_string(),
            });
        }
        Ok(())
    }
}

fn contains_git_component(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == std::ffi::OsStr::new(".git"))
}

/// Normalize a path without requiring it to exist (no canonicalize).
/// Resolves `..` and `.` components.
fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                result.pop();
            }
            std::path::Component::CurDir => {}
            c => result.push(c),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> tempfile::TempDir {
        tempfile::tempdir().expect("test: create temp dir")
    }

    #[test]
    fn test_path_guard_within_root_allowed() {
        let dir = setup();
        let file = dir.path().join("test_file.rs");
        std::fs::write(&file, "fn main() {}").expect("test: write test file");

        let guard = PathGuard::new(dir.path().to_path_buf()).expect("test: create guard");
        let result = guard.resolve("test_file.rs").expect("should resolve");
        assert_eq!(
            result,
            std::fs::canonicalize(&file).expect("test: canonicalize")
        );
    }

    #[test]
    fn test_path_guard_escape_is_rejected() {
        let dir = setup();
        let guard = PathGuard::new(dir.path().to_path_buf()).expect("test: create guard");
        let result = guard.resolve("../../etc/passwd");
        assert!(result.is_err(), "path traversal should be rejected");
        match result.expect_err("test: expected error") {
            ToolError::PathNotFound { .. } | ToolError::SensitiveFileDenied { .. } => {}
            other => panic!("expected PathNotFound or SensitiveFileDenied, got {other:?}"),
        }
    }

    #[test]
    fn test_path_guard_git_component_rejected() {
        let dir = setup();
        let git_dir = dir.path().join(".git");
        std::fs::create_dir_all(&git_dir).expect("test: create .git dir");
        let git_file = git_dir.join("config");
        std::fs::write(&git_file, "").expect("test: write .git/config");

        let guard = PathGuard::new(dir.path().to_path_buf()).expect("test: create guard");
        let result = guard.resolve(".git/config");
        assert!(result.is_err(), ".git paths should be rejected");
    }

    #[test]
    fn test_path_guard_nonexistent_path_rejected() {
        let dir = setup();
        let guard = PathGuard::new(dir.path().to_path_buf()).expect("test: create guard");
        let result = guard.resolve("nonexistent_file.rs");
        assert!(matches!(
            result.expect_err("test: expected error"),
            ToolError::PathNotFound { .. }
        ));
    }

    #[test]
    fn test_path_guard_write_nonexistent_with_existing_parent() {
        let dir = setup();
        let subdir = dir.path().join("subdir");
        std::fs::create_dir(&subdir).expect("test: create subdir");

        let guard = PathGuard::new(dir.path().to_path_buf()).expect("test: create guard");
        let result = guard.resolve_for_write("subdir/new_file.rs");
        assert!(
            result.is_ok(),
            "should allow write to non-existent file in existing parent"
        );
    }

    #[test]
    fn test_path_guard_write_escape_rejected() {
        let dir = setup();
        let guard = PathGuard::new(dir.path().to_path_buf()).expect("test: create guard");
        let result = guard.resolve_for_write("../../etc/evil.txt");
        assert!(result.is_err(), "write path traversal should be rejected");
    }
}
