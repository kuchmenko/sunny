//! Read-only Git tools for repository inspection.
//!
//! Provides [`GitLog`], [`GitDiff`], and [`GitStatus`] — thin wrappers around
//! `git log`, `git diff`, and `git status` with hardcoded flag allowlists
//! to prevent mutating operations and arbitrary argument injection.

use std::path::Path;
use std::process::Command;

use tracing::info;

use sunny_core::tool::ToolError;

/// Maximum output size in bytes; longer output is truncated.
const MAX_OUTPUT_BYTES: usize = 100_000;

/// Default number of log entries when `-n` is not specified.
const DEFAULT_LOG_LIMIT: usize = 50;

/// Allowed flags for `git log`.
const GIT_LOG_ALLOWED: &[&str] = &[
    "--oneline",
    "-n",
    "--max-count",
    "--decorate",
    "--graph",
    "--all",
    "--no-merges",
    "--format",
    "--since",
    "--author",
];

/// Allowed flags for `git diff`.
const GIT_DIFF_ALLOWED: &[&str] = &[
    "--staged",
    "--cached",
    "--stat",
    "--name-only",
    "--name-status",
    "--numstat",
];

/// Allowed flags for `git status`.
const GIT_STATUS_ALLOWED: &[&str] = &[
    "--porcelain",
    "--short",
    "--branch",
    "-b",
    "--untracked-files",
];

/// Read-only `git log` wrapper.
#[derive(Debug, Default)]
pub struct GitLog;

/// Read-only `git diff` wrapper.
#[derive(Debug, Default)]
pub struct GitDiff;

/// Read-only `git status` wrapper.
#[derive(Debug, Default)]
pub struct GitStatus;

impl GitLog {
    /// Run `git log --oneline -n {limit}` with optional extra flags.
    ///
    /// Every flag in `args` is validated against the allowlist.
    /// Unknown flags produce [`ToolError::PermissionDenied`].
    pub fn execute(&self, args: &str, root: &Path) -> Result<String, ToolError> {
        let normalized_args = normalize_git_log_args(args);
        let extra = validate_flags(&normalized_args, GIT_LOG_ALLOWED)?;

        let default_limit = DEFAULT_LOG_LIMIT.to_string();
        let mut cmd_args = vec!["log", "--oneline"];

        if !extra.iter().any(|a| a == "-n") {
            cmd_args.push("-n");
            cmd_args.push(&default_limit);
        }

        let extra_refs: Vec<&str> = extra.iter().map(String::as_str).collect();
        cmd_args.extend(extra_refs);

        run_git(&cmd_args, root, "git_log")
    }
}

impl GitDiff {
    /// Run `git diff` with optional extra flags.
    ///
    /// Every flag in `args` is validated against the allowlist.
    /// Unknown flags produce [`ToolError::PermissionDenied`].
    pub fn execute(&self, args: &str, root: &Path) -> Result<String, ToolError> {
        let extra = validate_flags(args, GIT_DIFF_ALLOWED)?;

        let mut cmd_args = vec!["diff"];
        let extra_refs: Vec<&str> = extra.iter().map(String::as_str).collect();
        cmd_args.extend(extra_refs);

        run_git(&cmd_args, root, "git_diff")
    }
}

impl GitStatus {
    /// Run `git status --porcelain` with optional extra flags.
    ///
    /// Every flag in `args` is validated against the allowlist.
    /// Unknown flags produce [`ToolError::PermissionDenied`].
    pub fn execute(&self, args: &str, root: &Path) -> Result<String, ToolError> {
        let extra = validate_flags(args, GIT_STATUS_ALLOWED)?;

        let mut cmd_args = vec!["status", "--porcelain"];
        let extra_refs: Vec<&str> = extra.iter().map(String::as_str).collect();
        cmd_args.extend(extra_refs);

        run_git(&cmd_args, root, "git_status")
    }
}

/// Validate that all flags in `args` are in the allowlist.
///
/// Non-flag tokens (values for preceding flags) pass through.
/// Returns the parsed tokens on success.
fn validate_flags(args: &str, allowed: &[&str]) -> Result<Vec<String>, ToolError> {
    if args.trim().is_empty() {
        return Ok(Vec::new());
    }

    let tokens: Vec<&str> = args.split_whitespace().collect();
    for token in &tokens {
        if token.starts_with('-') {
            // Handle --flag=value by checking only the flag portion
            let flag = token.split('=').next().unwrap_or(token);
            if !allowed.contains(&flag) {
                return Err(ToolError::PermissionDenied {
                    path: format!("disallowed git flag: {token}"),
                });
            }
        }
    }

    Ok(tokens.into_iter().map(String::from).collect())
}

fn normalize_git_log_args(args: &str) -> String {
    args.split_whitespace()
        .map(|token| {
            if token.starts_with('-')
                && token.len() > 1
                && token[1..].chars().all(|ch| ch.is_ascii_digit())
            {
                format!("-n {}", &token[1..])
            } else {
                token.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Run a git command and return truncated stdout.
fn run_git(args: &[&str], root: &Path, tool_name: &str) -> Result<String, ToolError> {
    info!(tool = tool_name, ?args, root = %root.display(), "executing git command");

    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| ToolError::ExecutionFailed {
            source: Box::new(e),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ToolError::ExecutionFailed {
            source: Box::new(std::io::Error::other(stderr.to_string())),
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    Ok(truncate_output(stdout))
}

/// Truncate output at [`MAX_OUTPUT_BYTES`], respecting UTF-8 boundaries.
fn truncate_output(mut output: String) -> String {
    if output.len() > MAX_OUTPUT_BYTES {
        let mut end = MAX_OUTPUT_BYTES;
        while end > 0 && !output.is_char_boundary(end) {
            end -= 1;
        }
        output.truncate(end);
        output.push_str("\n... [output truncated at 100KB]");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    /// Create a temporary directory with an initialized git repository.
    fn init_test_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .expect("test: git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .output()
            .expect("test: git config email");
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(dir.path())
            .output()
            .expect("test: git config name");
        dir
    }

    #[test]
    fn test_git_log_in_repo() {
        let dir = init_test_repo();
        let root = dir.path();

        fs::write(root.join("hello.txt"), "hello world").expect("test: write file");
        Command::new("git")
            .args(["add", "hello.txt"])
            .current_dir(root)
            .output()
            .expect("test: git add");
        Command::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(root)
            .output()
            .expect("test: git commit");

        let log = GitLog;
        let result = log.execute("", root).expect("should get git log");
        assert!(
            result.contains("initial commit"),
            "log should contain commit message, got: {result}"
        );
    }

    #[test]
    fn test_git_status_in_repo() {
        let dir = init_test_repo();
        let root = dir.path();

        // Need an initial commit so HEAD exists
        fs::write(root.join("init.txt"), "init").expect("test: write init file");
        Command::new("git")
            .args(["add", "init.txt"])
            .current_dir(root)
            .output()
            .expect("test: git add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(root)
            .output()
            .expect("test: git commit");

        fs::write(root.join("untracked.txt"), "new file").expect("test: write untracked file");

        let status = GitStatus;
        let result = status.execute("", root).expect("should get git status");
        assert!(
            result.contains("untracked.txt"),
            "status should show untracked file, got: {result}"
        );
    }

    #[test]
    fn test_git_diff_in_repo() {
        let dir = init_test_repo();
        let root = dir.path();

        fs::write(root.join("file.txt"), "original content").expect("test: write file");
        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(root)
            .output()
            .expect("test: git add");
        Command::new("git")
            .args(["commit", "-m", "add file"])
            .current_dir(root)
            .output()
            .expect("test: git commit");

        // Modify tracked file
        fs::write(root.join("file.txt"), "modified content").expect("test: modify file");

        let diff = GitDiff;
        let result = diff.execute("", root).expect("should get git diff");
        assert!(
            result.contains("modified content"),
            "diff should show changes, got: {result}"
        );
    }

    #[test]
    fn test_git_diff_accepts_name_status_flag() {
        let dir = init_test_repo();
        let root = dir.path();

        fs::write(root.join("file.txt"), "one").expect("test: write file");
        Command::new("git")
            .args(["add", "file.txt"])
            .current_dir(root)
            .output()
            .expect("test: git add");
        Command::new("git")
            .args(["commit", "-m", "add file"])
            .current_dir(root)
            .output()
            .expect("test: git commit");

        fs::write(root.join("file.txt"), "two").expect("test: modify file");

        let diff = GitDiff;
        let result = diff
            .execute("--name-status", root)
            .expect("should accept --name-status");
        assert!(result.contains("file.txt"));
    }

    #[test]
    fn test_git_status_accepts_branch_flag() {
        let dir = init_test_repo();
        let root = dir.path();

        let status = GitStatus;
        let result = status.execute("-b", root).expect("should accept -b");
        assert!(
            result.contains("##"),
            "expected branch header, got: {result}"
        );
    }

    #[test]
    fn test_not_a_git_repo_error() {
        let dir = tempfile::tempdir().expect("test: create temp dir");

        let log = GitLog;
        let err = log.execute("", dir.path()).unwrap_err();
        assert!(
            matches!(err, ToolError::ExecutionFailed { .. }),
            "expected ExecutionFailed for non-git dir, got: {err:?}"
        );
    }

    #[test]
    fn test_normalize_git_log_args_converts_numeric_shorthand() {
        assert_eq!(normalize_git_log_args("-15 --oneline"), "-n 15 --oneline");
        assert_eq!(normalize_git_log_args("--author=foo"), "--author=foo");
        assert_eq!(normalize_git_log_args(""), "");
    }

    #[test]
    fn test_git_log_accepts_max_count_long_flag() {
        let dir = init_test_repo();
        let root = dir.path();

        fs::write(root.join("hello.txt"), "hello world").expect("test: write file");
        Command::new("git")
            .args(["add", "hello.txt"])
            .current_dir(root)
            .output()
            .expect("test: git add");
        Command::new("git")
            .args(["commit", "-m", "initial commit"])
            .current_dir(root)
            .output()
            .expect("test: git commit");

        let log = GitLog;
        let result = log
            .execute("--max-count=1", root)
            .expect("should accept --max-count");
        assert!(result.contains("initial commit"));
    }

    #[test]
    fn test_large_output_truncated() {
        let dir = init_test_repo();
        let root = dir.path();

        // Initial commit required
        fs::write(root.join("init.txt"), "init").expect("test: write init file");
        Command::new("git")
            .args(["add", "init.txt"])
            .current_dir(root)
            .output()
            .expect("test: git add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(root)
            .output()
            .expect("test: git commit");

        // Create a large file (200KB) and stage it
        let large_content = "x".repeat(200_000);
        fs::write(root.join("large.txt"), &large_content).expect("test: write large file");
        Command::new("git")
            .args(["add", "large.txt"])
            .current_dir(root)
            .output()
            .expect("test: git add large file");

        let diff = GitDiff;
        let result = diff
            .execute("--staged", root)
            .expect("should get staged diff");

        assert!(
            result.len() <= MAX_OUTPUT_BYTES + 50,
            "output should be truncated near 100KB, got {} bytes",
            result.len()
        );
        assert!(
            result.contains("[output truncated at 100KB]"),
            "should contain truncation marker"
        );
    }
}
