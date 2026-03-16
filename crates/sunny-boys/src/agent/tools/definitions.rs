use serde_json;
use sunny_mind::ToolDefinition;

pub fn build_tool_definitions() -> Vec<ToolDefinition> {
    let mut defs = vec![
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
                    },
                    "line_hint": {
                        "type": "integer",
                        "description": "Hint for which line the match is near, for disambiguation when old_string appears multiple times"
                    },
                    "context_before": {
                        "type": "string",
                        "description": "Text that should appear before the match, for disambiguation"
                    },
                    "context_after": {
                        "type": "string",
                        "description": "Text that should appear after the match, for disambiguation"
                    }
                },
                "required": ["path", "old_string", "new_string"]
            }),
        },
        ToolDefinition {
            name: "fs_glob".to_string(),
            description: "Find files matching a glob pattern in the workspace. Respects .gitignore. Returns list of matching file paths.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern, e.g. '**/*.rs'"
                    },
                    "path": {
                        "type": "string",
                        "description": "Base directory to search (optional, defaults to workspace root)"
                    }
                },
                "required": ["pattern"]
            }),
        },
        ToolDefinition {
            name: "shell_exec".to_string(),
            description: "Execute a shell command in the workspace root directory. Commands ALWAYS run in the workspace root - never use `cd /path && command`, just run the command directly. Shell operators are restricted for safety.".to_string(),
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
            name: "git_commit".to_string(),
            description: "Create a git commit with a message. Optionally stage specific files before committing.".to_string(),
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
                        "description": "Optional list of files to stage before commit"
                    }
                },
                "required": ["message"]
            }),
        },
        ToolDefinition {
            name: "git_branch".to_string(),
            description: "Create a git branch. Optionally create and switch from a base target.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Branch name"
                    },
                    "base": {
                        "type": "string",
                        "description": "Optional base target to branch from"
                    }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "git_checkout".to_string(),
            description: "Switch to a branch, commit, or other git target.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Branch, commit, or other checkout target"
                    }
                },
                "required": ["target"]
            }),
        },
        ToolDefinition {
            name: "lsp_goto_definition".to_string(),
            description: "Jump to the definition location for the symbol at the given file position.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the source file, relative to the workspace root"
                    },
                    "line": {
                        "type": "integer",
                        "description": "1-based line number"
                    },
                    "character": {
                        "type": "integer",
                        "description": "0-based character offset"
                    }
                },
                "required": ["path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "lsp_find_references".to_string(),
            description: "Find references for the symbol at the given file position.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the source file, relative to the workspace root"
                    },
                    "line": {
                        "type": "integer",
                        "description": "1-based line number"
                    },
                    "character": {
                        "type": "integer",
                        "description": "0-based character offset"
                    }
                },
                "required": ["path", "line", "character"]
            }),
        },
        ToolDefinition {
            name: "lsp_diagnostics".to_string(),
            description: "Get language-server diagnostics for a source file.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the source file, relative to the workspace root"
                    },
                    "severity": {
                        "type": "string",
                        "enum": ["error", "warning", "information", "hint"],
                        "description": "Optional severity filter"
                    }
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "lsp_symbols".to_string(),
            description: "List symbols from a file or query workspace symbols.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the source file, relative to the workspace root"
                    },
                    "query": {
                        "type": "string",
                        "description": "Optional workspace symbol query; when omitted returns document symbols"
                    }
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "lsp_rename".to_string(),
            description: "Rename the symbol at the given file position and apply edits.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the source file, relative to the workspace root"
                    },
                    "line": {
                        "type": "integer",
                        "description": "1-based line number"
                    },
                    "character": {
                        "type": "integer",
                        "description": "0-based character offset"
                    },
                    "new_name": {
                        "type": "string",
                        "description": "New symbol name"
                    }
                },
                "required": ["path", "line", "character", "new_name"]
            }),
        },
        ToolDefinition {
            name: "interview".to_string(),
            description: "Present structured questions to the user and collect answers. Use this to gather context or requirements before proceeding.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "questions": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string" },
                                "text": { "type": "string" },
                                "type": {
                                    "type": "string",
                                    "enum": ["single_choice", "multi_choice", "free_text", "confirm"]
                                },
                                "options": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "label": { "type": "string" }
                                        },
                                        "required": ["label"]
                                    }
                                },
                                "header": { "type": "string" }
                            },
                            "required": ["id", "text", "type"]
                        }
                    }
                },
                "required": ["questions"]
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
    ];

    defs.extend(sunny_plans::tools::definitions::build_plan_tool_definitions());
    defs.push(ToolDefinition {
        name: "task_request_replan".to_string(),
        description: "Request that the current plan be replanned when execution discovers new information or a blocking issue. Use this to record an agent-triggered replan reason against an existing plan.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "plan_id": {
                    "type": "string",
                    "description": "ID of the plan that should receive the replan request"
                },
                "reason": {
                    "type": "string",
                    "description": "Explanation for why execution needs a replan"
                }
            },
            "required": ["plan_id", "reason"]
        }),
    });
    defs
}
