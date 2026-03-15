use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, RwLock};

use crate::agent::{GateDecision, SharedApprovalGate};
use crate::git_tools::{GitDiff, GitLog, GitStatus};
use crate::tool_loop::ToolExecutor;
use sunny_core::tool::{
    CapabilityChecker, FileEditor, FileReader, FileScanner, FileWriter, GrepFiles, PathGuard,
    ShellExecutor, TextGrep, ToolError, ToolPolicy,
};
use sunny_mind::ToolDefinition;
use sunny_tasks::{
    CapabilityStore, CreateAcceptCriteriaInput, CreateTaskInput, CreateVerifyCommandInput,
    TaskStatus, TaskStore, WorkspaceDetector,
};

pub type ActiveCapabilities = Arc<RwLock<HashMap<String, HashSet<String>>>>;

pub struct TaskCapabilityChecker {
    caps: ActiveCapabilities,
}

impl TaskCapabilityChecker {
    pub fn new(caps: ActiveCapabilities) -> Self {
        Self { caps }
    }
}

impl CapabilityChecker for TaskCapabilityChecker {
    fn is_granted(&self, capability: &str, pattern: Option<&str>) -> bool {
        let caps = self.caps.read().expect("capability lock poisoned");
        match caps.get(capability) {
            None => false,
            Some(patterns) => {
                if patterns.is_empty() {
                    true
                } else {
                    pattern.is_some_and(|p| patterns.contains(p))
                }
            }
        }
    }

    fn denied_hint(&self, capability: &str, pattern: Option<&str>) -> String {
        let pat = pattern.map_or(String::new(), |p| format!(" (pattern: {p})"));
        format!("capability '{capability}'{pat} not granted")
    }
}

pub fn build_active_capabilities(
    session_id: &str,
    workspace_root: Option<&std::path::Path>,
) -> ActiveCapabilities {
    let caps: ActiveCapabilities = Arc::new(RwLock::new(HashMap::new()));

    if let Some(root) = workspace_root {
        if let Ok(policy) = sunny_tasks::PolicyFile::load(root) {
            let mut map = caps.write().expect("capability lock poisoned");
            for (name, entry) in &policy.capabilities {
                if entry.policy == "workspace" || entry.policy == "global" {
                    let patterns: HashSet<String> = entry
                        .allowed_rhs
                        .as_ref()
                        .map_or_else(HashSet::new, |vals| vals.iter().cloned().collect());
                    map.insert(name.clone(), patterns);
                }
            }
        }
    }

    if let Ok(store) = CapabilityStore::open_default() {
        if let Ok(approved) = store.approved_for_session(session_id) {
            let mut map = caps.write().expect("capability lock poisoned");
            for req in approved {
                let patterns: HashSet<String> =
                    req.requested_rhs.unwrap_or_default().into_iter().collect();
                map.entry(req.capability).or_default().extend(patterns);
            }
        }
    }

    caps
}

/// Build tool definitions for all 11 coding tools exposed to the model.
pub fn build_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "fs_read".to_string(),
            description: "Read the contents of a file at the given path. Returns the file content as a string. For directories, use fs_scan instead.".to_string(),
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
        },
        ToolDefinition {
            name: "fs_scan".to_string(),
            description: "List files and directories under the given path. Returns a list of file paths. Use this to explore the workspace structure.".to_string(),
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
        },
        ToolDefinition {
            name: "fs_write".to_string(),
            description: "Write or create a file at the given path with the provided content. Overwrites the file if it already exists.".to_string(),
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
        },
        ToolDefinition {
            name: "fs_edit".to_string(),
            description: "Search-and-replace text in a file. The old_string must match exactly once in the file. Use this for targeted edits rather than rewriting the whole file.".to_string(),
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
        },
        ToolDefinition {
            name: "shell_exec".to_string(),
            description: "Execute a shell command in the workspace root directory. Commands ALWAYS run in the workspace root - never use `cd /path && command`, just run the command directly. Allowed commands: cargo, rustfmt, rustup, rustc, git, npm, npx, yarn, pnpm, node, python, python3, pip, pip3, uv, poetry, make, cmake, ls, cat, head, tail, wc, echo, pwd, grep, rg, ag, fd, jq, which, type, date, env, stat, du, df. Safe output pipes are always allowed: | tail, | head, | grep, | wc. Other shell operators (;, &&, ||, |<other>, $()) are not permitted.".to_string(),
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
        },
        ToolDefinition {
            name: "text_grep".to_string(),
            description: "Search for a regex pattern in a single file and return matching lines with line numbers.".to_string(),
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
        },
        ToolDefinition {
            name: "grep_files".to_string(),
            description: "Recursively search for a regex pattern across all files in a directory. Respects .gitignore. Returns matching lines with file paths and line numbers.".to_string(),
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
        },
        ToolDefinition {
            name: "git_log".to_string(),
            description: "Run read-only git log to inspect commit history. Supports flags: --oneline, -n <N>, --max-count=<N>, --format=..., --since=..., --author=..., --graph, --all, --no-merges.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "string",
                        "description": "Optional git log flags, e.g. '--oneline -n 20'"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "git_diff".to_string(),
            description: "Run read-only git diff to inspect changes. Supports flags: --staged, --cached, --stat, --name-only, --name-status, --numstat.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "string",
                        "description": "Optional git diff flags, e.g. '--staged --stat'"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "git_status".to_string(),
            description: "Run read-only git status to inspect the working tree. Supports flags: --porcelain, --short, --branch, -b.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "args": {
                        "type": "string",
                        "description": "Optional git status flags, e.g. '--short'"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "codebase_search".to_string(),
            description: "Search the codebase symbol index for Rust functions, structs, enums, traits, \
                          and other symbols by name. Returns matching symbols with file paths and line numbers. \
                          Use this to find where things are defined. Run /reindex first to build the index.".to_string(),
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
                        "enum": ["function", "struct", "enum", "trait", "impl", "const", "static", "type_alias", "macro", "module"]
                    }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "task_create".to_string(),
            description: "Create a new sub-task. Use to decompose your work into smaller units or delegate parallel work. The task will be queued and executed by the scheduler.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "Task title"
                    },
                    "description": {
                        "type": "string",
                        "description": "Task description"
                    },
                    "dep_ids": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional dependency task IDs"
                    },
                    "blocking": {
                        "type": "boolean",
                        "description": "Optional, default false"
                    },
                    "accept_criteria_description": {
                        "type": "string",
                        "description": "Optional acceptance criteria text"
                    },
                    "verify_commands": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "command": { "type": "string" },
                                "expected_exit_code": { "type": "integer" },
                                "timeout_secs": { "type": "integer" }
                            },
                            "required": ["command", "expected_exit_code", "timeout_secs"]
                        },
                        "description": "Optional verification commands"
                    },
                    "priority": {
                        "type": "integer",
                        "description": "Optional task priority"
                    },
                    "delegate_capabilities": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Capabilities to grant to the subtask (format: 'shell_pipes:tail,grep'). Agent must already hold each capability it delegates."
                    },
                    "category": {
                        "type": "string",
                        "enum": ["quick", "standard", "deep"],
                        "description": "Task complexity category. quick = simple mechanical work, standard = normal implementation, deep = complex reasoning. Determines which model runs the task."
                    }
                },
                "required": ["title", "description"]
            }),
        },
        ToolDefinition {
            name: "task_list".to_string(),
            description: "List tasks for the current workspace. Shows status, title, and dependencies.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "status_filter": {
                        "type": "string",
                        "description": "Optional status filter, e.g. 'pending', 'running', 'completed', 'failed'"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "task_get".to_string(),
            description: "Get full details of a specific task including its result, accept criteria, and questions.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Task ID"
                    }
                },
                "required": ["id"]
            }),
        },
        ToolDefinition {
            name: "task_complete".to_string(),
            description: "Mark your current task as complete. Provide a summary of what was accomplished. The system will run verification commands (if any) and capture the git diff.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "Completion summary"
                    }
                },
                "required": ["summary"]
            }),
        },
        ToolDefinition {
            name: "task_fail".to_string(),
            description: "Report that your current task has failed with an unrecoverable error. Provide a clear error description.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "error": {
                        "type": "string",
                        "description": "Failure reason"
                    }
                },
                "required": ["error"]
            }),
        },
        ToolDefinition {
            name: "task_ask_human".to_string(),
            description: "[PROVISIONAL] Ask the human a question that blocks your task progress. The task will be paused until answered. This interface will change in a future version.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "Question text"
                    },
                    "context": {
                        "type": "string",
                        "description": "Optional question context"
                    },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Optional answer options"
                    }
                },
                "required": ["question"]
            }),
        },
        ToolDefinition {
            name: "task_claim_paths".to_string(),
            description: "Declare file paths you intend to write. Advisory only — informs other concurrent tasks. Use glob patterns relative to the workspace root.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Path patterns to claim"
                    },
                    "claim_type": {
                        "type": "string",
                        "description": "Optional claim type: 'read' or 'write'"
                    }
                },
                "required": ["paths"]
            }),
        },
    ]
}

/// Build the tool executor that dispatches tool calls to the correct implementation.
///
/// All file tools are sandboxed to `root` via `PathGuard`.
pub fn build_tool_executor(
    root: PathBuf,
    task_id: Option<String>,
    session_id: Option<String>,
) -> Arc<ToolExecutor> {
    build_tool_executor_with_capabilities(root, None, task_id, session_id, None)
}

pub fn build_tool_executor_with_capabilities(
    root: PathBuf,
    checker: Option<Arc<dyn CapabilityChecker>>,
    task_id: Option<String>,
    session_id: Option<String>,
    approval_gate: Option<SharedApprovalGate>,
) -> Arc<ToolExecutor> {
    let task_id = task_id.unwrap_or_default();
    let session_id = session_id.unwrap_or_default();
    let checker_from_session: Option<Arc<dyn CapabilityChecker>> = if !session_id.is_empty() {
        let workspace_root = WorkspaceDetector::detect(&root);
        let caps = build_active_capabilities(&session_id, workspace_root.as_deref());
        Some(Arc::new(TaskCapabilityChecker::new(caps)) as Arc<dyn CapabilityChecker>)
    } else {
        checker
    };

    Arc::new(
        move |_id: &str, name: &str, args: &str, _depth: usize| -> Result<String, ToolError> {
            let args = if args.trim().is_empty() { "{}" } else { args };
            let parsed: serde_json::Value =
                serde_json::from_str(args).map_err(|e| ToolError::ExecutionFailed {
                    source: Box::new(e),
                })?;

            match name {
                "fs_read" => {
                    let path_str = extract_str(&parsed, "path")?;
                    let guard = PathGuard::new(root.clone())?;
                    let resolved = guard.resolve(path_str)?;
                    let reader = FileReader::default();
                    if resolved.is_dir() {
                        let scanner = FileScanner::default();
                        let scan = scanner.scan(&resolved)?;
                        let files: Vec<String> = scan
                            .files
                            .iter()
                            .map(|f| f.path.to_string_lossy().to_string())
                            .collect();
                        serde_json::to_string(&serde_json::json!({
                            "error": "path_is_directory",
                            "hint": "Use fs_scan for directories",
                            "entries": files
                        }))
                        .map_err(|e| ToolError::ExecutionFailed {
                            source: Box::new(e),
                        })
                    } else {
                        let content = reader.read(&resolved)?;
                        Ok(content.content)
                    }
                }
                "fs_scan" => {
                    let path_str = extract_str(&parsed, "path")?;
                    let guard = PathGuard::new(root.clone())?;
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
                "fs_write" => {
                    let path_str = extract_str(&parsed, "path")?;
                    let content = extract_str(&parsed, "content")?;
                    let writer = FileWriter::new(root.clone())?;
                    let result = writer.write(path_str, content)?;
                    Ok(format!(
                        "Written {} bytes to {}",
                        result.bytes_written,
                        result.path.display()
                    ))
                }
                "fs_edit" => {
                    let path_str = extract_str(&parsed, "path")?;
                    let old_string = extract_str(&parsed, "old_string")?;
                    let new_string = extract_str(&parsed, "new_string")?;
                    let editor = FileEditor::new(root.clone())?;
                    let result = editor.edit(path_str, old_string, new_string)?;
                    Ok(format!("Edited {}", result.path.display()))
                }
                "shell_exec" => {
                    let command = extract_str(&parsed, "command")?;
                    let timeout_secs = parsed["timeout_secs"].as_u64();
                    let executor = match checker_from_session.as_ref() {
                        Some(capability_checker) => ShellExecutor::with_capabilities(
                            root.clone(),
                            Arc::clone(capability_checker),
                        ),
                        None => ShellExecutor::new(root.clone()),
                    };
                    // shell_exec is async — run via block_in_place to avoid blocking the async executor
                    let result = tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current()
                            .block_on(executor.execute(command, timeout_secs))
                    });

                    match result {
                        Ok(exec_result) => {
                            let mut output = String::new();
                            if !exec_result.stdout.is_empty() {
                                output.push_str(&exec_result.stdout);
                            }
                            if !exec_result.stderr.is_empty() {
                                if !output.is_empty() {
                                    output.push('\n');
                                }
                                output.push_str("stderr: ");
                                output.push_str(&exec_result.stderr);
                            }
                            if exec_result.exit_code != 0 {
                                output.push_str(&format!("\nexit code: {}", exec_result.exit_code));
                            }
                            Ok(output)
                        }
                        Err(ToolError::CommandDenied {
                            command: denied_command,
                            reason,
                        }) => {
                            if let Some(ref gate) = approval_gate {
                                match gate.on_blocked("shell_exec", &denied_command, &reason) {
                                    GateDecision::Allow | GateDecision::AllowAndRemember => {
                                        let approved = tokio::task::block_in_place(|| {
                                            tokio::runtime::Handle::current().block_on(
                                                executor
                                                    .execute_approved(&denied_command, timeout_secs),
                                            )
                                        });
                                        match approved {
                                            Ok(exec_result) => {
                                                let mut output = String::new();
                                                if !exec_result.stdout.is_empty() {
                                                    output.push_str(&exec_result.stdout);
                                                }
                                                if !exec_result.stderr.is_empty() {
                                                    if !output.is_empty() {
                                                        output.push('\n');
                                                    }
                                                    output.push_str("stderr: ");
                                                    output.push_str(&exec_result.stderr);
                                                }
                                                if exec_result.exit_code != 0 {
                                                    output.push_str(&format!(
                                                        "\nexit code: {}",
                                                        exec_result.exit_code
                                                    ));
                                                }
                                                Ok(output)
                                            }
                                            Err(e) => Ok(format!("Approved but execution failed: {e}")),
                                        }
                                    }
                                    GateDecision::Deny => Ok(format!(
                                        "User denied this command: {denied_command}. Try a different approach."
                                    )),
                                }
                            } else {
                                Err(ToolError::CommandDenied {
                                    command: denied_command,
                                    reason,
                                })
                            }
                        }
                        Err(other) => Err(other),
                    }
                }
                "text_grep" => {
                    let path_str = extract_str(&parsed, "path")?;
                    let pattern = extract_str(&parsed, "pattern")?;
                    let guard = PathGuard::new(root.clone())?;
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
                "grep_files" => {
                    let path_str = extract_str(&parsed, "path")?;
                    let pattern = extract_str(&parsed, "pattern")?;
                    let max_results = parsed["max_results"].as_u64().map(|n| n as usize);
                    let grep_files = GrepFiles::new(root.clone())?;
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
                "git_log" => {
                    let git_args = parsed["args"].as_str().unwrap_or_default();
                    GitLog.execute(git_args, &root)
                }
                "git_diff" => {
                    let git_args = parsed["args"].as_str().unwrap_or_default();
                    GitDiff.execute(git_args, &root)
                }
                "git_status" => {
                    let git_args = parsed["args"].as_str().unwrap_or_default();
                    GitStatus.execute(git_args, &root)
                }
                "codebase_search" => {
                    let query = extract_str(&parsed, "query").unwrap_or("");
                    let kind_str = parsed["kind"].as_str();
                    // Open a fresh DB connection for the symbol index
                    let db =
                        match sunny_store::Database::open_default() {
                            Ok(db) => db,
                            Err(_) => return Ok(
                                "Codebase index not available. Run /reindex to build the index."
                                    .to_string(),
                            ),
                        };
                    let idx = sunny_store::SymbolIndex::new(db);
                    let results = if let Some(ks) = kind_str {
                        if let Some(kind) = sunny_store::SymbolKind::from_kind_str(ks) {
                            idx.search_by_kind(query, kind)
                        } else {
                            idx.search(query)
                        }
                    } else {
                        idx.search(query)
                    };
                    match results {
                        Ok(symbols) if symbols.is_empty() => Ok(
                            "No symbols found. Run /reindex to build or refresh the index."
                                .to_string(),
                        ),
                        Ok(symbols) => {
                            let lines: Vec<String> = symbols
                                .iter()
                                .take(20)
                                .map(|s| {
                                    format!(
                                        "{} {} — {}:{}-{}{}",
                                        s.kind.as_str(),
                                        s.name,
                                        s.file_path,
                                        s.line,
                                        s.end_line,
                                        s.parent
                                            .as_ref()
                                            .map(|p| format!(" (in {p})"))
                                            .unwrap_or_default()
                                    )
                                })
                                .collect();
                            Ok(lines.join("\n"))
                        }
                        Err(_) => Ok(
                            "Codebase index not available. Run /reindex to build the index."
                                .to_string(),
                        ),
                    }
                }
                "task_create" => {
                    let title = extract_str(&parsed, "title")?.to_string();
                    let description = extract_str(&parsed, "description")?.to_string();
                    let dep_ids =
                        extract_optional_string_array(&parsed, "dep_ids").unwrap_or_default();
                    let blocking = parsed["blocking"].as_bool().unwrap_or(false);
                    let accept_criteria_description = parsed["accept_criteria_description"]
                        .as_str()
                        .map(str::to_string);
                    let verify_commands = extract_verify_commands(&parsed["verify_commands"])?;
                    let priority = parsed["priority"].as_i64().unwrap_or(0) as i32;
                    let delegate_capabilities: Vec<String> = parsed["delegate_capabilities"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    let category = parsed["category"].as_str().map(str::to_string);
                    let session_id = session_id.clone();
                    let current_task_id = task_id.clone();

                    if !delegate_capabilities.is_empty() {
                        let Some(capability_checker) = checker_from_session.as_ref() else {
                            return Err(tool_exec_err(std::io::Error::other(
                                "cannot delegate capabilities outside capability-aware task sessions",
                            )));
                        };

                        for delegated in &delegate_capabilities {
                            let (capability, patterns) = parse_delegation_entry(delegated);
                            if patterns.is_empty() {
                                if !capability_checker.is_granted(&capability, None) {
                                    return Err(tool_exec_err(std::io::Error::other(format!(
                                        "cannot delegate capability '{capability}' because it is not granted",
                                    ))));
                                }
                            } else {
                                for pattern in &patterns {
                                    if !capability_checker.is_granted(&capability, Some(pattern)) {
                                        return Err(tool_exec_err(std::io::Error::other(format!(
                                            "cannot delegate capability '{capability}' for pattern '{pattern}' because it is not granted",
                                        ))));
                                    }
                                }
                            }
                        }
                    }

                    run_blocking_tool(move || {
                        let store = TaskStore::open_default().map_err(tool_exec_err)?;
                        let git_root = WorkspaceDetector::detect_cwd().ok_or_else(|| {
                            tool_exec_err(std::io::Error::other(
                                "no git workspace found from current directory",
                            ))
                        })?;
                        let git_root_str = git_root.to_str().ok_or_else(|| {
                            tool_exec_err(std::io::Error::other(
                                "workspace path is not valid UTF-8",
                            ))
                        })?;
                        let workspace = store
                            .find_or_create_workspace(git_root_str)
                            .map_err(tool_exec_err)?;

                        let accept_criteria =
                            accept_criteria_description.map(|criteria_description| {
                                CreateAcceptCriteriaInput {
                                    description: criteria_description,
                                    requires_human_approval: blocking,
                                    verify_commands,
                                }
                            });

                        let mut metadata = serde_json::Map::new();
                        if blocking {
                            metadata.insert("blocking".to_string(), serde_json::Value::Bool(true));
                        }
                        if !delegate_capabilities.is_empty() {
                            metadata.insert(
                                "delegate_capabilities".to_string(),
                                serde_json::Value::Array(
                                    delegate_capabilities
                                        .iter()
                                        .map(|cap| serde_json::Value::String(cap.clone()))
                                        .collect(),
                                ),
                            );
                        }
                        if let Some(cat) = &category {
                            metadata.insert(
                                "category".to_string(),
                                serde_json::Value::String(cat.clone()),
                            );
                        }

                        let task = store
                            .create_task(CreateTaskInput {
                                workspace_id: workspace.id,
                                parent_id: if current_task_id.is_empty() {
                                    None
                                } else {
                                    Some(current_task_id.clone())
                                },
                                title,
                                description,
                                created_by: format!("agent:{session_id}:{current_task_id}"),
                                priority,
                                max_retries: 3,
                                dep_ids,
                                accept_criteria,
                                delegate_capabilities,
                                metadata: if metadata.is_empty() {
                                    None
                                } else {
                                    Some(serde_json::Value::Object(metadata))
                                },
                            })
                            .map_err(tool_exec_err)?;

                        serde_json::to_string(&task).map_err(tool_exec_err)
                    })
                }
                "task_list" => {
                    let status_filter = parsed["status_filter"].as_str().map(str::to_string);
                    run_blocking_tool(move || {
                        let store = TaskStore::open_default().map_err(tool_exec_err)?;
                        let git_root = WorkspaceDetector::detect_cwd().ok_or_else(|| {
                            tool_exec_err(std::io::Error::other(
                                "no git workspace found from current directory",
                            ))
                        })?;
                        let git_root_str = git_root.to_str().ok_or_else(|| {
                            tool_exec_err(std::io::Error::other(
                                "workspace path is not valid UTF-8",
                            ))
                        })?;
                        let workspace = store
                            .find_or_create_workspace(git_root_str)
                            .map_err(tool_exec_err)?;
                        let mut tasks = store.list_tasks(&workspace.id).map_err(tool_exec_err)?;

                        if let Some(filter) = status_filter {
                            let normalized = filter.to_lowercase();
                            tasks.retain(|task| task.status.to_string() == normalized);
                        }

                        serde_json::to_string(&tasks).map_err(tool_exec_err)
                    })
                }
                "task_get" => {
                    let id = extract_str(&parsed, "id")?.to_string();
                    run_blocking_tool(move || {
                        let store = TaskStore::open_default().map_err(tool_exec_err)?;
                        let task = store.get_task(&id).map_err(tool_exec_err)?;
                        serde_json::to_string(&task).map_err(tool_exec_err)
                    })
                }
                "task_complete" => {
                    let summary = extract_str(&parsed, "summary")?.to_string();
                    let current_task_id = task_id.clone();
                    let repo_root = root.clone();
                    run_blocking_tool(move || {
                        if current_task_id.is_empty() {
                            return Ok(
                                "No active task context (SUNNY_TASK_ID is not set).".to_string()
                            );
                        }

                        let diff_output = Command::new("git")
                            .args(["diff", "HEAD"])
                            .current_dir(&repo_root)
                            .output()
                            .map_err(tool_exec_err)?;
                        if !diff_output.status.success() {
                            return Err(tool_exec_err(std::io::Error::other(
                                String::from_utf8_lossy(&diff_output.stderr).to_string(),
                            )));
                        }

                        let files_output = Command::new("git")
                            .args(["diff", "--name-only", "HEAD"])
                            .current_dir(&repo_root)
                            .output()
                            .map_err(tool_exec_err)?;
                        if !files_output.status.success() {
                            return Err(tool_exec_err(std::io::Error::other(
                                String::from_utf8_lossy(&files_output.stderr).to_string(),
                            )));
                        }

                        let diff = String::from_utf8_lossy(&diff_output.stdout).to_string();
                        let files = String::from_utf8_lossy(&files_output.stdout)
                            .lines()
                            .filter(|line| !line.trim().is_empty())
                            .map(|line| line.to_string())
                            .collect::<Vec<_>>();

                        let store = TaskStore::open_default().map_err(tool_exec_err)?;
                        store
                            .set_result(&current_task_id, Some(&diff), &summary, &files, None)
                            .map_err(tool_exec_err)?;

                        Ok("Task marked complete. Verification will run shortly.".to_string())
                    })
                }
                "task_fail" => {
                    let error = extract_str(&parsed, "error")?.to_string();
                    let current_task_id = task_id.clone();
                    run_blocking_tool(move || {
                        if current_task_id.is_empty() {
                            return Ok(
                                "No active task context (SUNNY_TASK_ID is not set).".to_string()
                            );
                        }

                        let store = TaskStore::open_default().map_err(tool_exec_err)?;
                        store
                            .set_error(&current_task_id, &error)
                            .map_err(tool_exec_err)?;
                        store
                            .increment_retry(&current_task_id)
                            .map_err(tool_exec_err)?;
                        store
                            .update_status(&current_task_id, TaskStatus::Failed)
                            .map_err(tool_exec_err)?;

                        Ok("Task marked as failed.".to_string())
                    })
                }
                "task_ask_human" => {
                    let question = extract_str(&parsed, "question")?.to_string();
                    let context = parsed["context"].as_str().map(str::to_string);
                    let options = extract_optional_string_array(&parsed, "options");
                    let current_task_id = task_id.clone();

                    run_blocking_tool(move || {
                        if current_task_id.is_empty() {
                            return Ok(
                                "No active task context (SUNNY_TASK_ID is not set).".to_string()
                            );
                        }

                        let store = TaskStore::open_default().map_err(tool_exec_err)?;
                        let created = store
                            .create_question(
                                &current_task_id,
                                &question,
                                context.as_deref(),
                                options.as_deref(),
                            )
                            .map_err(tool_exec_err)?;
                        store
                            .update_status(&current_task_id, TaskStatus::BlockedHuman)
                            .map_err(tool_exec_err)?;

                        Ok(format!(
                            "Question created (id: {}). Task paused pending human answer.",
                            created.id
                        ))
                    })
                }
                "task_claim_paths" => {
                    let paths = extract_string_array(&parsed, "paths")?;
                    let claim_type = parsed["claim_type"].as_str().unwrap_or("write").to_string();
                    let current_task_id = task_id.clone();

                    run_blocking_tool(move || {
                        if current_task_id.is_empty() {
                            return Ok(
                                "No active task context (SUNNY_TASK_ID is not set).".to_string()
                            );
                        }

                        let store = TaskStore::open_default().map_err(tool_exec_err)?;
                        for path in &paths {
                            store
                                .add_path_claim(&current_task_id, path, &claim_type)
                                .map_err(tool_exec_err)?;
                        }

                        Ok("Path claims registered.".to_string())
                    })
                }
                _ => Err(ToolError::ExecutionFailed {
                    source: Box::new(std::io::Error::other(format!("unknown tool: {name}"))),
                }),
            }
        },
    )
}

/// Build the tool policy allowing all registered tools.
/// Uses an empty deny-list so all tools are permitted by the policy.
/// Unknown tool names are handled in the executor by returning an error.
pub fn build_tool_policy() -> ToolPolicy {
    ToolPolicy::deny_list(&[])
}

fn extract_str<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a str, ToolError> {
    value[key]
        .as_str()
        .ok_or_else(|| ToolError::ExecutionFailed {
            source: Box::new(std::io::Error::other(format!("missing '{key}' argument"))),
        })
}

fn extract_string_array(value: &serde_json::Value, key: &str) -> Result<Vec<String>, ToolError> {
    let array = value[key]
        .as_array()
        .ok_or_else(|| tool_exec_err(std::io::Error::other(format!("missing '{key}' argument"))))?;

    let mut items = Vec::with_capacity(array.len());
    for item in array {
        let Some(text) = item.as_str() else {
            return Err(tool_exec_err(std::io::Error::other(format!(
                "'{key}' must contain only strings"
            ))));
        };
        items.push(text.to_string());
    }
    Ok(items)
}

fn extract_optional_string_array(value: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    value[key].as_array().map(|array| {
        array
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect()
    })
}

fn extract_verify_commands(
    value: &serde_json::Value,
) -> Result<Vec<CreateVerifyCommandInput>, ToolError> {
    let Some(array) = value.as_array() else {
        return Ok(Vec::new());
    };

    let mut commands = Vec::with_capacity(array.len());
    for item in array {
        let Some(command) = item.get("command").and_then(serde_json::Value::as_str) else {
            return Err(tool_exec_err(std::io::Error::other(
                "verify_commands[].command is required",
            )));
        };
        let Some(expected_exit_code) = item
            .get("expected_exit_code")
            .and_then(serde_json::Value::as_i64)
        else {
            return Err(tool_exec_err(std::io::Error::other(
                "verify_commands[].expected_exit_code is required",
            )));
        };
        let Some(timeout_secs) = item.get("timeout_secs").and_then(serde_json::Value::as_u64)
        else {
            return Err(tool_exec_err(std::io::Error::other(
                "verify_commands[].timeout_secs is required",
            )));
        };

        commands.push(CreateVerifyCommandInput {
            command: command.to_string(),
            expected_exit_code: expected_exit_code as i32,
            timeout_secs: timeout_secs as u32,
        });
    }
    Ok(commands)
}

fn run_blocking_tool<F>(op: F) -> Result<String, ToolError>
where
    F: FnOnce() -> Result<String, ToolError> + Send + 'static,
{
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(tokio::task::spawn_blocking(op))
    })
    .map_err(|error| {
        tool_exec_err(std::io::Error::other(format!(
            "blocking task join error: {error}"
        )))
    })?
}

fn tool_exec_err<E>(error: E) -> ToolError
where
    E: std::error::Error + Send + Sync + 'static,
{
    ToolError::ExecutionFailed {
        source: Box::new(error),
    }
}

fn parse_delegation_entry(value: &str) -> (String, Vec<String>) {
    if let Some((capability, rhs)) = value.split_once(':') {
        let patterns = rhs
            .split(',')
            .map(str::trim)
            .filter(|pattern| !pattern.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        (capability.to_string(), patterns)
    } else {
        (value.to_string(), Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_tool_definitions_count() {
        let defs = build_tool_definitions();
        assert_eq!(defs.len(), 18, "expected 18 tool definitions");
    }

    #[test]
    fn test_build_tool_definitions_names() {
        let defs = build_tool_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        let expected = [
            "fs_read",
            "fs_scan",
            "fs_write",
            "fs_edit",
            "shell_exec",
            "text_grep",
            "grep_files",
            "git_log",
            "git_diff",
            "git_status",
            "codebase_search",
            "task_create",
            "task_list",
            "task_get",
            "task_complete",
            "task_fail",
            "task_ask_human",
            "task_claim_paths",
        ];
        for name in &expected {
            assert!(names.contains(name), "missing tool: {name}");
        }
    }

    #[test]
    fn test_build_tool_definitions_schemas_non_empty() {
        let defs = build_tool_definitions();
        for def in &defs {
            assert!(!def.name.is_empty(), "tool name empty");
            assert!(
                !def.description.is_empty(),
                "tool description empty for {}",
                def.name
            );
        }
    }

    #[test]
    fn test_build_tool_policy_allows_all() {
        let policy = build_tool_policy();
        let allowed = [
            "fs_read",
            "fs_scan",
            "fs_write",
            "fs_edit",
            "shell_exec",
            "text_grep",
            "grep_files",
            "git_log",
            "git_diff",
            "git_status",
            "codebase_search",
            "task_create",
            "task_list",
            "task_get",
            "task_complete",
            "task_fail",
            "task_ask_human",
            "task_claim_paths",
        ];
        for name in &allowed {
            assert!(policy.is_allowed(name), "policy should allow {name}");
        }
    }

    #[test]
    fn test_build_tool_executor_unknown_tool_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let executor = build_tool_executor(dir.path().to_path_buf(), None, None);
        let result = executor("unknown_tool", "{}", "id1", 0);
        assert!(result.is_err(), "unknown tool should return error");
    }

    #[test]
    fn test_task_capability_checker_grants_when_present() {
        let mut caps = HashMap::new();
        caps.insert("git_write".to_string(), HashSet::new());
        let checker = TaskCapabilityChecker::new(Arc::new(RwLock::new(caps)));

        assert!(checker.is_granted("git_write", None));
    }

    #[test]
    fn test_task_capability_checker_denies_when_absent() {
        let checker = TaskCapabilityChecker::new(Arc::new(RwLock::new(HashMap::new())));

        assert!(!checker.is_granted("git_write", None));
    }

    #[test]
    fn test_task_capability_checker_grants_pattern_match() {
        let mut caps = HashMap::new();
        caps.insert(
            "shell_pipes".to_string(),
            HashSet::from(["tail".to_string(), "grep".to_string()]),
        );
        let checker = TaskCapabilityChecker::new(Arc::new(RwLock::new(caps)));

        assert!(checker.is_granted("shell_pipes", Some("tail")));
        assert!(!checker.is_granted("shell_pipes", Some("wc")));
    }
}
