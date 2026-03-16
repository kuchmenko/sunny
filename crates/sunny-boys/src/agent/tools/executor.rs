use std::path::PathBuf;
use std::sync::Arc;

use crate::agent::interview::InterviewRunner;
use crate::agent::{GateDecision, SharedApprovalGate};
use crate::git_tools::{GitBranch, GitCheckout, GitCommit, GitDiff, GitLog, GitStatus};
use crate::tool_loop::ToolExecutor;
use sunny_core::tool::{
    lsp::{
        LspDiagnosticsTool, LspFindReferencesTool, LspGotoDefinitionTool, LspRenameTool,
        LspSymbolsTool,
    },
    CapabilityChecker, FileEditor, FileReader, FileScanner, FileSnapshot, FileSnapshotStore,
    FileWriter, FsGlobTool, GrepFiles, InterviewOption, InterviewQuestion, LspClient, PathGuard,
    QuestionType, ShellExecutor, TextGrep, ToolError,
};
use sunny_tasks::WorkspaceDetector;
use tokio::sync::Mutex;

use super::helpers::{
    build_active_capabilities, extract_str, tool_exec_err, TaskCapabilityChecker,
};
use super::task_handlers::{
    handle_task_ask_human, handle_task_claim_paths, handle_task_complete, handle_task_create,
    handle_task_fail, handle_task_get, handle_task_list,
};

/// Build the tool executor that dispatches tool calls to the correct implementation.
///
/// All file tools are sandboxed to `root` via `PathGuard`.
pub fn build_tool_executor_with_capabilities(
    root: PathBuf,
    checker: Option<Arc<dyn CapabilityChecker>>,
    task_id: Option<String>,
    session_id: Option<String>,
    approval_gate: Option<SharedApprovalGate>,
) -> Arc<ToolExecutor> {
    let task_id = task_id.unwrap_or_default();
    let session_id = session_id.unwrap_or_default();
    let snapshot_store = Arc::new(FileSnapshotStore::new());
    let lsp_client: Arc<Mutex<Option<LspClient>>> = Arc::new(Mutex::new(None));
    let interview_runner = Arc::new(InterviewRunner::new());
    let checker_from_session: Option<Arc<dyn CapabilityChecker>> = if !session_id.is_empty() {
        let workspace_root = WorkspaceDetector::detect(&root);
        let caps = build_active_capabilities(&session_id, workspace_root.as_deref());
        Some(
            Arc::new(TaskCapabilityChecker::new(caps, session_id.clone()))
                as Arc<dyn CapabilityChecker>,
        )
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
                        if let Ok(snapshot) = FileSnapshot::capture(&resolved) {
                            snapshot_store.record(snapshot);
                        }
                        let numbered = content
                            .lines
                            .iter()
                            .enumerate()
                            .map(|(i, line)| format!("{}:{}", i + 1, line))
                            .collect::<Vec<_>>()
                            .join("\n");
                        Ok(numbered)
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
                    let guard = PathGuard::new(root.clone())?;
                    let resolved = guard.resolve_for_write(path_str)?;
                    if let Some(snapshot) = snapshot_store.get_snapshot(&resolved) {
                        if snapshot.is_stale(25)? {
                            return Err(ToolError::ExecutionFailed {
                                source: Box::new(std::io::Error::other(
                                    "File has been modified since it was last read. Re-read the file before modifying it.",
                                )),
                            });
                        }
                    }
                    let writer = FileWriter::new(root.clone())?;
                    let result = writer.write(path_str, content)?;
                    if let Ok(snapshot) = FileSnapshot::capture(&result.path) {
                        snapshot_store.record(snapshot);
                    }
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
                    let line_hint = parsed["line_hint"].as_u64().map(|n| n as usize);
                    let context_before = parsed["context_before"].as_str().map(ToString::to_string);
                    let context_after = parsed["context_after"].as_str().map(ToString::to_string);
                    let guard = PathGuard::new(root.clone())?;
                    let resolved = guard.resolve(path_str)?;
                    if let Some(snapshot) = snapshot_store.get_snapshot(&resolved) {
                        if snapshot.is_stale(25)? {
                            return Err(ToolError::ExecutionFailed {
                                source: Box::new(std::io::Error::other(
                                    "File has been modified since it was last read. Re-read the file before modifying it.",
                                )),
                            });
                        }
                    }
                    let editor = FileEditor::new(root.clone())?;
                    let result = editor.edit(
                        path_str,
                        old_string,
                        new_string,
                        line_hint,
                        context_before.as_deref(),
                        context_after.as_deref(),
                    )?;
                    if let Ok(snapshot) = FileSnapshot::capture(&result.path) {
                        snapshot_store.record(snapshot);
                    }
                    Ok(format!("Edited {}", result.path.display()))
                }
                "fs_glob" => {
                    let pattern = extract_str(&parsed, "pattern")?;
                    let base_path = parsed["path"].as_str();
                    let glob_tool = FsGlobTool::new(root.clone()).map_err(tool_exec_err)?;
                    let matches = glob_tool.glob(pattern, base_path)?;
                    serde_json::to_string(
                        &matches
                            .iter()
                            .map(|path| path.display().to_string())
                            .collect::<Vec<_>>(),
                    )
                    .map_err(tool_exec_err)
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
                                let gate_decision = tokio::task::block_in_place(|| {
                                    tokio::runtime::Handle::current().block_on(gate.on_blocked(
                                        "shell_exec",
                                        &denied_command,
                                        &reason,
                                    ))
                                });

                                match gate_decision {
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
                "git_commit" => {
                    ensure_capability(checker_from_session.as_ref(), "git_write")?;
                    let message = extract_str(&parsed, "message")?;
                    let files = parsed["files"]
                        .as_array()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|item| item.as_str().map(str::to_string))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    GitCommit.execute(message, &files, &root)
                }
                "git_branch" => {
                    ensure_capability(checker_from_session.as_ref(), "git_write")?;
                    let name = extract_str(&parsed, "name")?;
                    let base = parsed["base"].as_str();
                    GitBranch.execute(name, base, &root)
                }
                "git_checkout" => {
                    ensure_capability(checker_from_session.as_ref(), "git_write")?;
                    let target = extract_str(&parsed, "target")?;
                    GitCheckout.execute(target, &root)
                }
                "lsp_goto_definition" => {
                    let path = resolve_tool_path(&root, extract_str(&parsed, "path")?)?;
                    let line = extract_u32(&parsed, "line")?;
                    let character = extract_u32(&parsed, "character")?;
                    with_lsp_client(&lsp_client, &root, |client| {
                        tokio::task::block_in_place(|| {
                            tokio::runtime::Handle::current().block_on(async {
                                LspGotoDefinitionTool::new(client)
                                    .run(&path, line, character)
                                    .await
                            })
                        })
                    })
                }
                "lsp_find_references" => {
                    let path = resolve_tool_path(&root, extract_str(&parsed, "path")?)?;
                    let line = extract_u32(&parsed, "line")?;
                    let character = extract_u32(&parsed, "character")?;
                    with_lsp_client(&lsp_client, &root, |client| {
                        tokio::task::block_in_place(|| {
                            tokio::runtime::Handle::current().block_on(async {
                                LspFindReferencesTool::new(client)
                                    .run(&path, line, character)
                                    .await
                            })
                        })
                    })
                }
                "lsp_diagnostics" => {
                    let path = resolve_tool_path(&root, extract_str(&parsed, "path")?)?;
                    let severity = validate_diagnostic_severity(parsed["severity"].as_str())?;
                    with_lsp_client(&lsp_client, &root, |client| {
                        let output = tokio::task::block_in_place(|| {
                            tokio::runtime::Handle::current().block_on(async {
                                LspDiagnosticsTool::new(client).run(&path, None).await
                            })
                        })?;
                        Ok(filter_diagnostics_by_severity(&output, severity.as_deref()))
                    })
                }
                "lsp_symbols" => {
                    let path = resolve_tool_path(&root, extract_str(&parsed, "path")?)?;
                    let query = parsed["query"].as_str().map(str::to_string);
                    with_lsp_client(&lsp_client, &root, |client| {
                        tokio::task::block_in_place(|| {
                            tokio::runtime::Handle::current().block_on(async {
                                LspSymbolsTool::new(client)
                                    .run(&path, query.as_deref())
                                    .await
                            })
                        })
                    })
                }
                "lsp_rename" => {
                    let path = resolve_tool_path(&root, extract_str(&parsed, "path")?)?;
                    let line = extract_u32(&parsed, "line")?;
                    let character = extract_u32(&parsed, "character")?;
                    let new_name = extract_str(&parsed, "new_name")?.to_string();
                    with_lsp_client(&lsp_client, &root, |client| {
                        tokio::task::block_in_place(|| {
                            tokio::runtime::Handle::current().block_on(async {
                                LspRenameTool::new(client)
                                    .run(&path, line, character, &new_name)
                                    .await
                            })
                        })
                    })
                }
                "interview" => {
                    let questions = parse_interview_questions(&parsed)?;
                    let runner = Arc::clone(&interview_runner);
                    tokio::task::block_in_place(|| {
                        tokio::runtime::Handle::current().block_on(async move {
                            let answers = runner.present(questions).await?;
                            serde_json::to_string(&answers).map_err(tool_exec_err)
                        })
                    })
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
                "task_create" => handle_task_create(
                    &parsed,
                    checker_from_session.as_ref(),
                    &session_id,
                    &task_id,
                ),
                "task_list" => handle_task_list(&parsed),
                "task_get" => handle_task_get(&parsed),
                "task_complete" => handle_task_complete(&parsed, &task_id, &root),
                "task_fail" => handle_task_fail(&parsed, &task_id),
                "task_ask_human" => handle_task_ask_human(&parsed, &task_id),
                "task_claim_paths" => handle_task_claim_paths(&parsed, &task_id),
                "plan_create" => {
                    let store = sunny_plans::store::PlanStore::open_default().map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                    sunny_plans::tools::handlers::handle_plan_create(&store, &parsed).map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })
                }
                "plan_add_task" => {
                    let store = sunny_plans::store::PlanStore::open_default().map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                    sunny_plans::tools::handlers::handle_plan_add_task(&store, &parsed).map_err(
                        |e| ToolError::ExecutionFailed {
                            source: Box::new(e),
                        },
                    )
                }
                "plan_add_dependency" => {
                    let store = sunny_plans::store::PlanStore::open_default().map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                    sunny_plans::tools::handlers::handle_plan_add_dependency(&store, &parsed)
                        .map_err(|e| ToolError::ExecutionFailed {
                            source: Box::new(e),
                        })
                }
                "plan_remove_task" => {
                    let store = sunny_plans::store::PlanStore::open_default().map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                    sunny_plans::tools::handlers::handle_plan_remove_task(&store, &parsed).map_err(
                        |e| ToolError::ExecutionFailed {
                            source: Box::new(e),
                        },
                    )
                }
                "plan_query_state" => {
                    let store = sunny_plans::store::PlanStore::open_default().map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                    sunny_plans::tools::handlers::handle_plan_query_state(&store, &parsed).map_err(
                        |e| ToolError::ExecutionFailed {
                            source: Box::new(e),
                        },
                    )
                }
                "plan_finalize" => {
                    let store = sunny_plans::store::PlanStore::open_default().map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                    sunny_plans::tools::handlers::handle_plan_finalize(&store, &parsed).map_err(
                        |e| ToolError::ExecutionFailed {
                            source: Box::new(e),
                        },
                    )
                }
                "plan_replan" => {
                    let store = sunny_plans::store::PlanStore::open_default().map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                    sunny_plans::tools::handlers::handle_plan_replan(&store, &parsed).map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })
                }
                "plan_record_decision" => {
                    let store = sunny_plans::store::PlanStore::open_default().map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                    sunny_plans::tools::handlers::handle_plan_record_decision(&store, &parsed)
                        .map_err(|e| ToolError::ExecutionFailed {
                            source: Box::new(e),
                        })
                }
                "plan_add_constraint" => {
                    let store = sunny_plans::store::PlanStore::open_default().map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                    sunny_plans::tools::handlers::handle_plan_add_constraint(&store, &parsed)
                        .map_err(|e| ToolError::ExecutionFailed {
                            source: Box::new(e),
                        })
                }
                "plan_add_goal" => {
                    let store = sunny_plans::store::PlanStore::open_default().map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                    sunny_plans::tools::handlers::handle_plan_add_goal(&store, &parsed).map_err(
                        |e| ToolError::ExecutionFailed {
                            source: Box::new(e),
                        },
                    )
                }
                "plan_update_goal" => {
                    let store = sunny_plans::store::PlanStore::open_default().map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                    sunny_plans::tools::handlers::handle_plan_update_goal(&store, &parsed).map_err(
                        |e| ToolError::ExecutionFailed {
                            source: Box::new(e),
                        },
                    )
                }
                "task_request_replan" => {
                    let store = sunny_plans::store::PlanStore::open_default().map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                    sunny_plans::tools::handlers::handle_task_request_replan(&store, &parsed)
                        .map_err(|e| ToolError::ExecutionFailed {
                            source: Box::new(e),
                        })
                }
                _ => Err(ToolError::ExecutionFailed {
                    source: Box::new(std::io::Error::other(format!("unknown tool: {name}"))),
                }),
            }
        },
    )
}

fn ensure_capability(
    checker: Option<&Arc<dyn CapabilityChecker>>,
    capability: &str,
) -> Result<(), ToolError> {
    if checker.is_some_and(|checker| !checker.is_granted(capability, None)) {
        return Err(tool_exec_err(std::io::Error::other(format!(
            "missing '{capability}' capability"
        ))));
    }
    Ok(())
}

fn extract_u32(value: &serde_json::Value, key: &str) -> Result<u32, ToolError> {
    let raw = value[key]
        .as_u64()
        .ok_or_else(|| tool_exec_err(std::io::Error::other(format!("missing '{key}' argument"))))?;
    u32::try_from(raw).map_err(|_| {
        tool_exec_err(std::io::Error::other(format!(
            "'{key}' is too large for u32"
        )))
    })
}

fn resolve_tool_path(root: &std::path::Path, path: &str) -> Result<std::path::PathBuf, ToolError> {
    let guard = PathGuard::new(root.to_path_buf())?;
    guard.resolve(path)
}

fn validate_diagnostic_severity(severity: Option<&str>) -> Result<Option<String>, ToolError> {
    match severity {
        None => Ok(None),
        Some("error") => Ok(Some("ERROR".to_string())),
        Some("warning") => Ok(Some("WARN".to_string())),
        Some("information") => Ok(Some("INFO".to_string())),
        Some("hint") => Ok(Some("HINT".to_string())),
        Some(other) => Err(tool_exec_err(std::io::Error::other(format!(
            "invalid severity: {other}"
        )))),
    }
}

fn filter_diagnostics_by_severity(output: &str, severity: Option<&str>) -> String {
    let Some(severity) = severity else {
        return output.to_string();
    };

    let lines = output
        .lines()
        .filter(|line| line.contains(&format!("[{severity}]")))
        .collect::<Vec<_>>();

    if lines.is_empty() {
        "No diagnostics found".to_string()
    } else {
        lines.join("\n")
    }
}

async fn ensure_lsp_initialized(
    slot: &Arc<Mutex<Option<LspClient>>>,
    root: &std::path::Path,
) -> Result<(), ToolError> {
    let mut guard = slot.lock().await;
    if guard.is_none() {
        let mut client = LspClient::spawn("rust-analyzer", root).await?;
        client.initialize(root).await?;
        *guard = Some(client);
    }
    Ok(())
}

fn with_lsp_client<F>(
    slot: &Arc<Mutex<Option<LspClient>>>,
    root: &std::path::Path,
    f: F,
) -> Result<String, ToolError>
where
    F: FnOnce(&LspClient) -> Result<String, ToolError>,
{
    let slot = Arc::clone(slot);
    let root = root.to_path_buf();
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(async move {
            ensure_lsp_initialized(&slot, &root).await?;
            let guard = slot.lock().await;
            let client = guard.as_ref().ok_or_else(|| {
                tool_exec_err(std::io::Error::other("LSP client not initialized"))
            })?;
            f(client)
        })
    })
}

fn parse_interview_questions(
    parsed: &serde_json::Value,
) -> Result<Vec<InterviewQuestion>, ToolError> {
    let entries = parsed["questions"]
        .as_array()
        .ok_or_else(|| tool_exec_err(std::io::Error::other("missing 'questions' argument")))?;

    let mut questions = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = entry["id"].as_str().ok_or_else(|| {
            tool_exec_err(std::io::Error::other("interview question missing 'id'"))
        })?;
        let text = entry["text"].as_str().ok_or_else(|| {
            tool_exec_err(std::io::Error::other("interview question missing 'text'"))
        })?;
        let question_type = parse_question_type(entry["type"].as_str())?;
        let options = entry["options"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| {
                        item["label"].as_str().map(|label| InterviewOption {
                            label: label.to_string(),
                            description: None,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let header = entry["header"].as_str().map(str::to_string);

        questions.push(InterviewQuestion {
            id: id.to_string(),
            text: text.to_string(),
            question_type,
            options,
            header,
        });
    }

    Ok(questions)
}

fn parse_question_type(question_type: Option<&str>) -> Result<QuestionType, ToolError> {
    match question_type {
        Some("single_choice") => Ok(QuestionType::SingleChoice),
        Some("multi_choice") => Ok(QuestionType::MultiChoice),
        Some("free_text") => Ok(QuestionType::FreeText),
        Some("confirm") => Ok(QuestionType::Confirm),
        Some(other) => Err(tool_exec_err(std::io::Error::other(format!(
            "invalid interview question type: {other}"
        )))),
        None => Err(tool_exec_err(std::io::Error::other(
            "interview question missing 'type'",
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::build_tool_executor_with_capabilities;
    use crate::agent::tools::build_tool_definitions;
    use std::collections::HashSet;
    use std::thread;
    use std::time::Duration;

    fn run_tool(
        executor: &std::sync::Arc<crate::tool_loop::ToolExecutor>,
        name: &str,
        args: serde_json::Value,
    ) -> Result<String, sunny_core::tool::ToolError> {
        executor.as_ref()("test-call", name, &args.to_string(), 0)
    }

    #[test]
    fn test_all_defined_tools_have_executor_match() {
        let source = include_str!("executor.rs");
        let mut matched: HashSet<String> = HashSet::new();

        for line in source.lines() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix('"') {
                if let Some((name, _)) = rest.split_once("\" =>") {
                    matched.insert(name.to_string());
                }
            }
        }

        for definition in build_tool_definitions() {
            assert!(
                matched.contains(&definition.name),
                "tool '{}' is defined but missing executor match arm",
                definition.name
            );
        }
    }

    #[test]
    fn test_stale_read_blocks_write_after_external_change() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let file = dir.path().join("tracked.txt");
        std::fs::write(&file, "initial\nvalue\n").expect("test: seed tracked file");
        let executor =
            build_tool_executor_with_capabilities(dir.path().to_path_buf(), None, None, None, None);

        run_tool(
            &executor,
            "fs_read",
            serde_json::json!({ "path": "tracked.txt" }),
        )
        .expect("test: read file before write");

        thread::sleep(Duration::from_millis(50));
        std::fs::write(&file, "external update\n").expect("test: externally modify file");

        let err = run_tool(
            &executor,
            "fs_write",
            serde_json::json!({ "path": "tracked.txt", "content": "agent update\n" }),
        )
        .expect_err("test: stale snapshot should block write");

        assert_eq!(
            err.to_string(),
            "tool execution failed: File has been modified since it was last read. Re-read the file before modifying it."
        );
        let content = std::fs::read_to_string(&file).expect("test: read final file content");
        assert_eq!(content, "external update\n");
    }

    #[test]
    fn test_write_succeeds_after_fresh_read() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let file = dir.path().join("tracked.txt");
        std::fs::write(&file, "initial\nvalue\n").expect("test: seed tracked file");
        let executor =
            build_tool_executor_with_capabilities(dir.path().to_path_buf(), None, None, None, None);

        run_tool(
            &executor,
            "fs_read",
            serde_json::json!({ "path": "tracked.txt" }),
        )
        .expect("test: initial read");

        thread::sleep(Duration::from_millis(50));
        std::fs::write(&file, "external update\n").expect("test: externally modify file");

        run_tool(
            &executor,
            "fs_read",
            serde_json::json!({ "path": "tracked.txt" }),
        )
        .expect("test: refresh read after external change");

        let output = run_tool(
            &executor,
            "fs_write",
            serde_json::json!({ "path": "tracked.txt", "content": "agent update\n" }),
        )
        .expect("test: fresh snapshot should allow write");

        assert_eq!(output, format!("Written 13 bytes to {}", file.display()));
        let content = std::fs::read_to_string(&file).expect("test: read final file content");
        assert_eq!(content, "agent update\n");
    }

    #[test]
    fn test_write_allowed_for_new_file_without_prior_read() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let file = dir.path().join("new.txt");
        let executor =
            build_tool_executor_with_capabilities(dir.path().to_path_buf(), None, None, None, None);

        let output = run_tool(
            &executor,
            "fs_write",
            serde_json::json!({ "path": "new.txt", "content": "brand new\n" }),
        )
        .expect("test: new file write should succeed");

        assert_eq!(output, format!("Written 10 bytes to {}", file.display()));
        let content = std::fs::read_to_string(&file).expect("test: read created file");
        assert_eq!(content, "brand new\n");
    }
}
