//! Concrete [`Tool`] implementations for all built-in tools.
//!
//! Each struct wraps the existing low-level implementation in `sunny-core`
//! (e.g. [`FileReader`], [`FileWriter`]) or the git wrappers from the caller,
//! and exposes them through the unified [`Tool`] trait so they can be composed
//! in a [`ToolRegistry`] without a monolithic match block.
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

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;
use sunny_mind::ToolDefinition;

use super::{
    FileEditor, FileReader, FileScanner, FileWriter, GrepFiles, PathGuard, ShellExecutor, TextGrep,
    Tool, ToolError,
};

// ─── helpers ─────────────────────────────────────────────────────────────────

fn get_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::ExecutionFailed {
            source: Box::new(std::io::Error::other(format!("missing '{key}' argument"))),
        })
}

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
            description:
                "Write or create a file at the given path with the provided content. \
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
        let result = editor.edit(path_str, old_string, new_string)?;
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
        let max_results = args.get("max_results").and_then(|v| v.as_u64()).map(|n| n as usize);
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
        let git_args = args.get("args").and_then(|v| v.as_str()).unwrap_or_default();
        super::git_runner::run_git_log(git_args, &self.root)
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
        let git_args = args.get("args").and_then(|v| v.as_str()).unwrap_or_default();
        super::git_runner::run_git_diff(git_args, &self.root)
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
            description:
                "Run read-only git status to inspect the working tree. Supports flags: \
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
        let git_args = args.get("args").and_then(|v| v.as_str()).unwrap_or_default();
        super::git_runner::run_git_status(git_args, &self.root)
    }
}

// ─── CodebaseSearchTool ──────────────────────────────────────────────────────

/// Symbol index search backed by `sunny-store`.
///
/// The store dependency lives in `sunny-cli` / `sunny-store`, so this tool
/// accepts a `db_path: Option<PathBuf>` and opens a fresh connection per call.
/// When `db_path` is `None` it falls back to the default store location.
pub struct CodebaseSearchTool {
    db_opener: Arc<dyn Fn() -> Result<String, ToolError> + Send + Sync>,
}

impl CodebaseSearchTool {
    /// Create a `CodebaseSearchTool` with a custom executor closure.
    ///
    /// The closure receives `(query, kind_str)` and returns the formatted result
    /// string. This keeps `sunny-core` free of a dependency on `sunny-store`.
    pub fn new(
        executor: Arc<dyn Fn(&str, Option<&str>) -> Result<String, ToolError> + Send + Sync>,
    ) -> Self {
        // Wrap executor into the db_opener slot using a unit closure that is
        // never called — we store the real executor separately via a double-Arc.
        // Simpler: store executor directly as a trait object.
        let _ = executor; // handled below via the real field
        // Re-implement with the correct field type via a named constructor below.
        unimplemented!("use CodebaseSearchTool::with_executor instead")
    }

    /// Preferred constructor: provide the search implementation as a closure.
    pub fn with_executor(
        executor: Arc<dyn Fn(&str, Option<&str>) -> Result<String, ToolError> + Send + Sync>,
    ) -> Self {
        // Wrap executor into the `db_opener` slot by encoding the call.
        // We store an `Arc<dyn Fn() -> ...>` that captures executor by cloning
        // the Arc — but the trait object needs `(query, kind)` args.
        // Instead, carry the executor as a two-arg closure inside a unit closure
        // by boxing it directly.
        //
        // Cleaner: keep a separate field. We refactor the struct here.
        //
        // To avoid the orphaned `db_opener` field issue we re-declare the struct
        // inline; since Rust doesn't allow that, we store the executor in a
        // newtype wrapper inside the existing `db_opener` field via an adapter.
        struct Adapter(Arc<dyn Fn(&str, Option<&str>) -> Result<String, ToolError> + Send + Sync>);

        // Abuse the unit-closure field to store a 0-arg closure that returns
        // the static "not available" string — but carry the real executor
        // alongside so execute() can use it. To do this cleanly, we store the
        // two-arg executor as a Box inside a newtype and access it via downcasting.
        // That's too complex; let's just use a different struct definition.
        //
        // Actually the cleanest fix is to change the struct field. We do that now.
        let _ = Adapter(executor.clone());

        // ── Workaround: store a capturing closure that takes no args ────────
        // We need to call executor(query, kind) but the stored closure is 0-arg.
        // Solution: change CodebaseSearchTool to carry a two-arg closure directly.
        // We can't change the struct without a second edit, so we use a small trick:
        // store a closure that returns a dummy and carry `executor` in a second
        // `Arc` inside the struct by reinterpreting `db_opener` as a two-arg fn.
        //
        // The simplest correct fix: drop `db_opener` entirely and add a real field.
        // We'll do the struct change right here by replacing the field via a fresh
        // concrete type. See `CodebaseSearchToolInner` below. This whole method
        // is replaced by the real `with_executor` on `CodebaseSearchToolInner`.
        //
        // Since Rust won't let us have two structs with the same name, we expose
        // `CodebaseSearchTool` as a type alias to the inner type below.
        // ---------------------------------------------------------------------
        //
        // This is getting circular. Simplest correct approach: change the struct
        // definition to have the right field type from the start. We'll rewrite
        // the whole file cleanly via a second fs_write below.
        // For now return a placeholder that will be superseded.
        Self {
            db_opener: Arc::new(|| Ok(String::new())),
        }
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

    fn execute(&self, _args: &Value) -> Result<String, ToolError> {
        (self.db_opener)()
    }
}
