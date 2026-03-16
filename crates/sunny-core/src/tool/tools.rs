//! Concrete [`Tool`] implementations for all built-in tools.
//!
//! Each struct wraps the existing low-level implementation in `sunny-core`
//! (e.g. [`FileReader`], [`FileWriter`]) or the git wrappers from the caller,
//! and exposes them through the unified [`Tool`] trait so they can be composed
//! in a `ToolRegistry` without a monolithic match block.
//!
//! # File-system tools
//! All FS tools accept a `root: PathBuf` that is used to construct a
//! [`PathGuard`] at call time, enforcing sandbox containment.
//!
//! # Shell tool
//! [`ShellExecTool`] spawns processes via [`ShellExecutor`] which is async.
//! Because [`Tool::execute`] is synchronous, the implementation uses
//! `tokio::task::block_in_place` + `Handle::current().block_on(...)` — the
//! same approach previously used in `tools.rs`.
//!
//! # Git tools
//! [`GitLogTool`], [`GitDiffTool`], [`GitStatusTool`] are thin wrappers around
//! the existing read-only git command runners.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;
use sunny_mind::ToolDefinition;

use super::{
    CapabilityChecker, FileEditor, FileReader, FileScanner, FileWriter, GrepFiles, PathGuard,
    ShellExecutor, TextGrep, Tool, ToolError,
};

// ─── helpers ─────────────────────────────────────────────────────────────────

fn get_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::ExecutionFailed {
            source: Box::new(std::io::Error::other(format!("missing '{key}' argument"))),
        })
}

type CodebaseSearchExecutor = dyn Fn(&str, Option<&str>) -> Result<String, ToolError> + Send + Sync;

// ─── FsReadTool ──────────────────────────────────────────────────────────────

/// Read a single file; redirects to `fs_scan` hint when the path is a directory.
pub struct FsReadTool {
    root: PathBuf,
}

impl FsReadTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for FsReadTool {
    fn name(&self) -> &str {
        "fs_read"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fs_read".to_string(),
            description: "Read the contents of a file at the given path. Returns the file content \
                          as a string. For directories, use fs_scan instead."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read, relative to the workspace root"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let path_str = get_str(args, "path")?;
        let guard = PathGuard::new(self.root.clone())?;
        let resolved = guard.resolve(path_str)?;

        if resolved.is_dir() {
            let scanner = FileScanner::default();
            let scan = scanner.scan(&resolved)?;
            let files: Vec<String> = scan
                .files
                .iter()
                .map(|f| f.path.to_string_lossy().to_string())
                .collect();
            return serde_json::to_string(&serde_json::json!({
                "error": "path_is_directory",
                "hint": "Use fs_scan for directories",
                "entries": files
            }))
            .map_err(|e| ToolError::ExecutionFailed {
                source: Box::new(e),
            });
        }

        let reader = FileReader::default();
        let content = reader.read(&resolved)?;
        Ok(content.content)
    }
}

// ─── FsScanTool ──────────────────────────────────────────────────────────────

/// List files and directories under a path.
pub struct FsScanTool {
    root: PathBuf,
}

impl FsScanTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for FsScanTool {
    fn name(&self) -> &str {
        "fs_scan"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fs_scan".to_string(),
            description:
                "List files and directories under the given path. Returns a list of file paths. \
                 Use this to explore the workspace structure."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to scan, relative to the workspace root"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let path_str = get_str(args, "path")?;
        let guard = PathGuard::new(self.root.clone())?;
        let resolved = guard.resolve(path_str)?;
        let scanner = FileScanner::default();
        let scan = scanner.scan(&resolved)?;
        let files: Vec<String> = scan
            .files
            .iter()
            .map(|f| f.path.to_string_lossy().to_string())
            .collect();
        serde_json::to_string(&files).map_err(|e| ToolError::ExecutionFailed {
            source: Box::new(e),
        })
    }
}

// ─── FsWriteTool ─────────────────────────────────────────────────────────────

/// Write or overwrite a file.
pub struct FsWriteTool {
    root: PathBuf,
}

impl FsWriteTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for FsWriteTool {
    fn name(&self) -> &str {
        "fs_write"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fs_write".to_string(),
            description: "Write or create a file at the given path with the provided content. \
                 Overwrites the file if it already exists."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to write the file, relative to the workspace root"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let path_str = get_str(args, "path")?;
        let content = get_str(args, "content")?;
        let writer = FileWriter::new(self.root.clone())?;
        let result = writer.write(path_str, content)?;
        Ok(format!(
            "Written {} bytes to {}",
            result.bytes_written,
            result.path.display()
        ))
    }
}

// ─── FsEditTool ──────────────────────────────────────────────────────────────

/// Search-and-replace within a file (exactly-once match).
pub struct FsEditTool {
    root: PathBuf,
}

impl FsEditTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for FsEditTool {
    fn name(&self) -> &str {
        "fs_edit"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "fs_edit".to_string(),
            description:
                "Search-and-replace text in a file. The old_string must match exactly once in the \
                 file. Use this for targeted edits rather than rewriting the whole file."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to edit, relative to the workspace root"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "The exact text to search for. Must match exactly once."
                    },
                    "new_string": {
                        "type": "string",
                        "description": "The replacement text"
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let path_str = get_str(args, "path")?;
        let old_string = get_str(args, "old_string")?;
        let new_string = get_str(args, "new_string")?;
        let editor = FileEditor::new(self.root.clone())?;
        let result = editor.edit(path_str, old_string, new_string, None, None, None)?;
        Ok(format!("Edited {}", result.path.display()))
    }
}

// ─── ShellExecTool ───────────────────────────────────────────────────────────

/// Execute a shell command inside the workspace root.
///
/// Because [`ShellExecutor::execute`] is async and [`Tool::execute`] is sync,
/// this uses `block_in_place` to run the future on the current thread without
/// blocking the async executor.
pub struct ShellExecTool {
    root: PathBuf,
}

impl ShellExecTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for ShellExecTool {
    fn name(&self) -> &str {
        "shell_exec"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "shell_exec".to_string(),
            description:
                "Execute a shell command in the workspace directory. Use for running builds, \
                 tests, linters, or other tools. Commands are subject to a safety denylist."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute"
                    },
                    "timeout_secs": {
                        "type": "number",
                        "description": "Timeout in seconds (default: 30)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let command = get_str(args, "command")?;
        let timeout_secs = args.get("timeout_secs").and_then(|v| v.as_u64());
        let executor = ShellExecutor::new(self.root.clone());
        // SAFETY: block_in_place is called from within a spawn_blocking thread
        // (as dispatched by StreamingToolLoop). This is safe: spawn_blocking
        // threads are not part of the async executor thread pool, so
        // block_in_place does not starve the runtime.
        let result = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(executor.execute(command, timeout_secs))
        })?;
        let mut output = String::new();
        if !result.stdout.is_empty() {
            output.push_str(&result.stdout);
        }
        if !result.stderr.is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str("stderr: ");
            output.push_str(&result.stderr);
        }
        if result.exit_code != 0 {
            output.push_str(&format!("\nexit code: {}", result.exit_code));
        }
        Ok(output)
    }
}

// ─── TextGrepTool ────────────────────────────────────────────────────────────

/// Regex search within a single file.
pub struct TextGrepTool {
    root: PathBuf,
}

impl TextGrepTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for TextGrepTool {
    fn name(&self) -> &str {
        "text_grep"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "text_grep".to_string(),
            description:
                "Search for a regex pattern in a single file and return matching lines with line \
                 numbers."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to search, relative to the workspace root"
                    },
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for (falls back to literal substring if invalid regex)"
                    }
                },
                "required": ["path", "pattern"]
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let path_str = get_str(args, "path")?;
        let pattern = get_str(args, "pattern")?;
        let guard = PathGuard::new(self.root.clone())?;
        let resolved = guard.resolve(path_str)?;
        let reader = FileReader::default();
        let content = reader.read(&resolved)?;
        let grep = TextGrep::default();
        let result = grep.search(&content.content, pattern);
        let matches: Vec<String> = result
            .matches
            .iter()
            .map(|m| format!("{}:{}", m.line_number, m.line_content))
            .collect();
        serde_json::to_string(&matches).map_err(|e| ToolError::ExecutionFailed {
            source: Box::new(e),
        })
    }
}

// ─── GrepFilesTool ───────────────────────────────────────────────────────────

/// Recursive regex search across a directory tree.
pub struct GrepFilesTool {
    root: PathBuf,
}

impl GrepFilesTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for GrepFilesTool {
    fn name(&self) -> &str {
        "grep_files"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep_files".to_string(),
            description:
                "Recursively search for a regex pattern across all files in a directory. Respects \
                 .gitignore. Returns matching lines with file paths and line numbers."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory to search recursively, relative to the workspace root"
                    },
                    "pattern": {
                        "type": "string",
                        "description": "Regex pattern to search for"
                    },
                    "max_results": {
                        "type": "number",
                        "description": "Maximum number of matching lines to return (default: 100)"
                    }
                },
                "required": ["path", "pattern"]
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let path_str = get_str(args, "path")?;
        let pattern = get_str(args, "pattern")?;
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);
        let grep_files = GrepFiles::new(self.root.clone())?;
        let file_matches = grep_files.search(path_str, pattern, max_results)?;
        let mut lines: Vec<String> = Vec::new();
        for fm in &file_matches {
            for m in &fm.matches {
                lines.push(format!(
                    "{}:{}:{}",
                    fm.path.display(),
                    m.line_number,
                    m.line_content
                ));
            }
        }
        serde_json::to_string(&lines).map_err(|e| ToolError::ExecutionFailed {
            source: Box::new(e),
        })
    }
}

// ─── Git tools ───────────────────────────────────────────────────────────────

/// Read-only `git log` wrapper.
pub struct GitLogTool {
    root: PathBuf,
}

impl GitLogTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for GitLogTool {
    fn name(&self) -> &str {
        "git_log"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "git_log".to_string(),
            description:
                "Run read-only git log to inspect commit history. Supports flags: --oneline, \
                 -n <N>, --max-count=<N>, --format=..., --since=..., --author=..., --graph, \
                 --all, --no-merges."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "string",
                        "description": "Optional git log flags, e.g. '--oneline -n 20'"
                    }
                }
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let git_args = args
            .get("args")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        git_runner::run_git_log(git_args, &self.root)
    }
}

/// Read-only `git diff` wrapper.
pub struct GitDiffTool {
    root: PathBuf,
}

impl GitDiffTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "git_diff".to_string(),
            description:
                "Run read-only git diff to inspect changes. Supports flags: --staged, --cached, \
                 --stat, --name-only, --name-status, --numstat."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "string",
                        "description": "Optional git diff flags, e.g. '--staged --stat'"
                    }
                }
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let git_args = args
            .get("args")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        git_runner::run_git_diff(git_args, &self.root)
    }
}

/// Read-only `git status` wrapper.
pub struct GitStatusTool {
    root: PathBuf,
}

impl GitStatusTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

impl Tool for GitStatusTool {
    fn name(&self) -> &str {
        "git_status"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "git_status".to_string(),
            description: "Run read-only git status to inspect the working tree. Supports flags: \
                 --porcelain, --short, --branch, -b."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "string",
                        "description": "Optional git status flags, e.g. '--short'"
                    }
                }
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let git_args = args
            .get("args")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        git_runner::run_git_status(git_args, &self.root)
    }
}

pub struct GitCommitTool {
    root: PathBuf,
    checker: Option<Arc<dyn CapabilityChecker>>,
}

impl GitCommitTool {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            checker: None,
        }
    }

    pub fn with_capabilities(root: PathBuf, checker: Arc<dyn CapabilityChecker>) -> Self {
        Self {
            root,
            checker: Some(checker),
        }
    }
}

impl Tool for GitCommitTool {
    fn name(&self) -> &str {
        "git_commit"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "git_commit".to_string(),
            description: "Create a git commit, optionally staging the provided files first. Requires git_write capability."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "message": {
                        "type": "string",
                        "description": "Commit message"
                    },
                    "files": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional list of files to stage before committing"
                    }
                },
                "required": ["message"]
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        if let Some(checker) = &self.checker {
            if !checker.is_granted("git_write", None) {
                return Err(ToolError::CommandDenied {
                    command: "git_commit".to_string(),
                    reason: "git_write capability required".to_string(),
                });
            }
        }

        let message = get_str(args, "message")?;
        let files: Vec<String> = args
            .get("files")
            .and_then(|value| value.as_array())
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.as_str().map(ToOwned::to_owned))
                    .collect()
            })
            .unwrap_or_default();

        git_runner::run_git_commit(message, &files, &self.root)
    }
}

pub struct GitBranchTool {
    root: PathBuf,
    checker: Option<Arc<dyn CapabilityChecker>>,
}

impl GitBranchTool {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            checker: None,
        }
    }

    pub fn with_capabilities(root: PathBuf, checker: Arc<dyn CapabilityChecker>) -> Self {
        Self {
            root,
            checker: Some(checker),
        }
    }
}

impl Tool for GitBranchTool {
    fn name(&self) -> &str {
        "git_branch"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "git_branch".to_string(),
            description:
                "Create a git branch, optionally from a base ref. Requires git_write capability."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Name of the branch to create"
                    },
                    "base": {
                        "type": "string",
                        "description": "Optional base ref for checkout -b"
                    }
                },
                "required": ["name"]
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        if let Some(checker) = &self.checker {
            if !checker.is_granted("git_write", None) {
                return Err(ToolError::CommandDenied {
                    command: "git_branch".to_string(),
                    reason: "git_write capability required".to_string(),
                });
            }
        }

        let name = get_str(args, "name")?;
        let base = args.get("base").and_then(|value| value.as_str());
        git_runner::run_git_branch(name, base, &self.root)
    }
}

pub struct GitCheckoutTool {
    root: PathBuf,
    checker: Option<Arc<dyn CapabilityChecker>>,
}

impl GitCheckoutTool {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            checker: None,
        }
    }

    pub fn with_capabilities(root: PathBuf, checker: Arc<dyn CapabilityChecker>) -> Self {
        Self {
            root,
            checker: Some(checker),
        }
    }
}

impl Tool for GitCheckoutTool {
    fn name(&self) -> &str {
        "git_checkout"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "git_checkout".to_string(),
            description: "Check out a git branch, tag, or commit. Requires git_write capability."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Branch, tag, or commit to check out"
                    }
                },
                "required": ["target"]
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        if let Some(checker) = &self.checker {
            if !checker.is_granted("git_write", None) {
                return Err(ToolError::CommandDenied {
                    command: "git_checkout".to_string(),
                    reason: "git_write capability required".to_string(),
                });
            }
        }

        let target = get_str(args, "target")?;
        git_runner::run_git_checkout(target, &self.root)
    }
}

mod git_runner {
    use std::process::Command;

    use tracing::info;

    use super::Path;
    use super::ToolError;

    const MAX_OUTPUT_BYTES: usize = 100_000;
    const DEFAULT_LOG_LIMIT: usize = 50;

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

    const GIT_DIFF_ALLOWED: &[&str] = &[
        "--staged",
        "--cached",
        "--stat",
        "--name-only",
        "--name-status",
        "--numstat",
    ];

    const GIT_STATUS_ALLOWED: &[&str] = &[
        "--porcelain",
        "--short",
        "--branch",
        "-b",
        "--untracked-files",
    ];

    pub fn run_git_log(args: &str, root: &Path) -> Result<String, ToolError> {
        let normalized_args = normalize_git_log_args(args);
        let extra = validate_flags(&normalized_args, GIT_LOG_ALLOWED)?;

        let default_limit = DEFAULT_LOG_LIMIT.to_string();
        let mut cmd_args = vec!["log", "--oneline"];

        if !extra.iter().any(|arg| arg == "-n") {
            cmd_args.push("-n");
            cmd_args.push(&default_limit);
        }

        let extra_refs: Vec<&str> = extra.iter().map(String::as_str).collect();
        cmd_args.extend(extra_refs);

        run_git(&cmd_args, root, "git_log")
    }

    pub fn run_git_diff(args: &str, root: &Path) -> Result<String, ToolError> {
        let extra = validate_flags(args, GIT_DIFF_ALLOWED)?;

        let mut cmd_args = vec!["diff"];
        let extra_refs: Vec<&str> = extra.iter().map(String::as_str).collect();
        cmd_args.extend(extra_refs);

        run_git(&cmd_args, root, "git_diff")
    }

    pub fn run_git_status(args: &str, root: &Path) -> Result<String, ToolError> {
        let extra = validate_flags(args, GIT_STATUS_ALLOWED)?;

        let mut cmd_args = vec!["status", "--porcelain"];
        let extra_refs: Vec<&str> = extra.iter().map(String::as_str).collect();
        cmd_args.extend(extra_refs);

        run_git(&cmd_args, root, "git_status")
    }

    pub fn run_git_commit(
        message: &str,
        files: &[String],
        root: &Path,
    ) -> Result<String, ToolError> {
        if !files.is_empty() {
            let mut add_args = vec!["add"];
            let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();
            add_args.extend(file_refs);
            run_git(&add_args, root, "git_commit")?;
        }

        let output = Command::new("git")
            .args(["commit", "-m", message])
            .current_dir(root)
            .output()
            .map_err(|error| ToolError::ExecutionFailed {
                source: Box::new(error),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ToolError::ExecutionFailed {
                source: Box::new(std::io::Error::other(stderr.to_string())),
            });
        }

        Ok(format!(
            "Committed: {}",
            truncate_output(String::from_utf8_lossy(&output.stdout).to_string())
        ))
    }

    pub fn run_git_branch(
        name: &str,
        base: Option<&str>,
        root: &Path,
    ) -> Result<String, ToolError> {
        match base {
            Some(base) => run_git(&["checkout", "-b", name, base], root, "git_branch"),
            None => run_git(&["branch", name], root, "git_branch"),
        }
    }

    pub fn run_git_checkout(target: &str, root: &Path) -> Result<String, ToolError> {
        run_git(&["checkout", target], root, "git_checkout")
    }

    fn split_args(input: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut current = String::new();
        let mut chars = input.chars().peekable();
        let mut in_single = false;
        let mut in_double = false;

        while let Some(ch) = chars.next() {
            match ch {
                '\'' if !in_double => in_single = !in_single,
                '"' if !in_single => in_double = !in_double,
                '\\' if in_double => {
                    if let Some(&next) = chars.peek() {
                        if next == '"' {
                            chars.next();
                            current.push('"');
                        } else {
                            current.push('\\');
                        }
                    }
                }
                ' ' | '\t' if !in_single && !in_double => {
                    if !current.is_empty() {
                        tokens.push(std::mem::take(&mut current));
                    }
                }
                _ => current.push(ch),
            }
        }

        if !current.is_empty() {
            tokens.push(current);
        }

        tokens
    }

    fn validate_flags(args: &str, allowed: &[&str]) -> Result<Vec<String>, ToolError> {
        if args.trim().is_empty() {
            return Ok(Vec::new());
        }

        let tokens = split_args(args);
        for token in &tokens {
            if token.starts_with('-') {
                let flag = token.split('=').next().unwrap_or(token.as_str());
                if !allowed.contains(&flag) {
                    return Err(ToolError::PermissionDenied {
                        path: format!("disallowed git flag: {token}"),
                    });
                }
            }
        }

        Ok(tokens)
    }

    fn normalize_git_log_args(args: &str) -> String {
        split_args(args)
            .into_iter()
            .map(|token| {
                if token.starts_with('-')
                    && token.len() > 1
                    && token[1..].chars().all(|ch| ch.is_ascii_digit())
                {
                    format!("-n {}", &token[1..])
                } else {
                    token
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn run_git(args: &[&str], root: &Path, tool_name: &str) -> Result<String, ToolError> {
        info!(tool = tool_name, ?args, root = %root.display(), "executing git command");

        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .map_err(|error| ToolError::ExecutionFailed {
                source: Box::new(error),
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(ToolError::ExecutionFailed {
                source: Box::new(std::io::Error::other(stderr.to_string())),
            });
        }

        Ok(truncate_output(
            String::from_utf8_lossy(&output.stdout).to_string(),
        ))
    }

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
}

// ─── CodebaseSearchTool ──────────────────────────────────────────────────────

/// Symbol index search backed by `sunny-store`.
///
/// The store dependency lives in `sunny-cli` / `sunny-store`, so this tool
/// accepts a `db_path: Option<PathBuf>` and opens a fresh connection per call.
/// When `db_path` is `None` it falls back to the default store location.
pub struct CodebaseSearchTool {
    executor: Arc<CodebaseSearchExecutor>,
}

impl CodebaseSearchTool {
    /// Create a `CodebaseSearchTool` with a custom executor closure.
    ///
    /// The closure receives `(query, kind_str)` and returns the formatted result
    /// string. This keeps `sunny-core` free of a dependency on `sunny-store`.
    pub fn new(executor: Arc<CodebaseSearchExecutor>) -> Self {
        Self { executor }
    }

    pub fn with_executor(executor: Arc<CodebaseSearchExecutor>) -> Self {
        Self::new(executor)
    }
}

impl Tool for CodebaseSearchTool {
    fn name(&self) -> &str {
        "codebase_search"
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "codebase_search".to_string(),
            description:
                "Search the codebase symbol index for Rust functions, structs, enums, traits, \
                 and other symbols by name. Returns matching symbols with file paths and line \
                 numbers. Use this to find where things are defined. Run /reindex first to build \
                 the index."
                    .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Symbol name to search for (case-insensitive substring match)"
                    },
                    "kind": {
                        "type": "string",
                        "description": "Optional: filter by symbol kind",
                        "enum": ["function","struct","enum","trait","impl","const","static","type_alias","macro","module"]
                    }
                },
                "required": ["query"]
            }),
        }
    }

    fn execute(&self, args: &Value) -> Result<String, ToolError> {
        let query = get_str(args, "query")?;
        let kind = args.get("kind").and_then(|value| value.as_str());
        (self.executor)(query, kind)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use serde_json::json;

    use super::*;

    struct MockCapabilityChecker {
        grant_git_write: bool,
    }

    impl CapabilityChecker for MockCapabilityChecker {
        fn is_granted(&self, capability: &str, _pattern: Option<&str>) -> bool {
            capability == "git_write" && self.grant_git_write
        }

        fn denied_hint(&self, capability: &str, _pattern: Option<&str>) -> String {
            format!("{capability} denied")
        }
    }

    fn init_git_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let root = dir.path();

        Command::new("git")
            .args(["init"])
            .current_dir(root)
            .output()
            .expect("test: git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(root)
            .output()
            .expect("test: git config email");
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(root)
            .output()
            .expect("test: git config name");

        dir
    }

    mod git_write {
        use super::*;

        #[test]
        fn test_git_commit_requires_capability() {
            let dir = init_git_repo();
            let checker = Arc::new(MockCapabilityChecker {
                grant_git_write: false,
            });
            let tool = GitCommitTool::with_capabilities(dir.path().to_path_buf(), checker);

            let err = tool
                .execute(&json!({"message": "blocked commit"}))
                .expect_err("git_commit should require capability");

            assert!(matches!(
                err,
                ToolError::CommandDenied { command, reason }
                    if command == "git_commit" && reason == "git_write capability required"
            ));
        }

        #[test]
        fn test_git_commit_succeeds_with_capability() {
            let dir = init_git_repo();
            let root = dir.path();
            fs::write(root.join("hello.txt"), "hello world").expect("test: write file");

            let checker = Arc::new(MockCapabilityChecker {
                grant_git_write: true,
            });
            let tool = GitCommitTool::with_capabilities(root.to_path_buf(), checker);

            let result = tool
                .execute(&json!({
                    "message": "add hello",
                    "files": ["hello.txt"]
                }))
                .expect("git_commit should pass capability gate and commit");

            assert!(result.contains("Committed:"), "unexpected result: {result}");
        }
    }
}
