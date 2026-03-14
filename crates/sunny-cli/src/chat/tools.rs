use std::path::PathBuf;
use std::sync::Arc;

use sunny_boys::git_tools::{GitDiff, GitLog, GitStatus};
use sunny_boys::tool_loop::ToolExecutor;
use sunny_core::tool::{
    FileEditor, FileReader, FileScanner, FileWriter, GrepFiles, PathGuard, ShellExecutor, TextGrep,
    ToolError, ToolPolicy,
};
use sunny_mind::ToolDefinition;

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
            description: "Execute a shell command in the workspace directory. Use for running builds, tests, linters, or other tools. Commands are subject to a safety denylist.".to_string(),
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
    ]
}

/// Build the tool executor that dispatches tool calls to the correct implementation.
///
/// All file tools are sandboxed to `root` via `PathGuard`.
pub fn build_tool_executor(root: PathBuf) -> Arc<ToolExecutor> {
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
                    let executor = ShellExecutor::new(root.clone());
                    // shell_exec is async — run via block_in_place to avoid blocking the async executor
                    let result = tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current()
                            .block_on(executor.execute(command, timeout_secs))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_tool_definitions_count() {
        let defs = build_tool_definitions();
        assert_eq!(defs.len(), 11, "expected 11 tool definitions");
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
        ];
        for name in &allowed {
            assert!(policy.is_allowed(name), "policy should allow {name}");
        }
    }

    #[test]
    fn test_build_tool_executor_unknown_tool_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let executor = build_tool_executor(dir.path().to_path_buf());
        let result = executor("unknown_tool", "{}", "id1", 0);
        assert!(result.is_err(), "unknown tool should return error");
    }
}
