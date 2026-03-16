use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use sunny_core::tool::{CapabilityChecker, ToolError};
use sunny_tasks::{CreateAcceptCriteriaInput, CreateTaskInput, TaskStatus};

use super::helpers::{
    detect_workspace_store, extract_optional_string_array, extract_str, extract_string_array,
    extract_verify_commands, parse_delegation_entry, resolve_root_session_id, run_blocking_tool,
    tool_exec_err,
};

pub(super) fn handle_task_create(
    parsed: &serde_json::Value,
    checker_from_session: Option<&Arc<dyn CapabilityChecker>>,
    session_id: &str,
    task_id: &str,
) -> Result<String, ToolError> {
    let title = extract_str(parsed, "title")?.to_string();
    let description = extract_str(parsed, "description")?.to_string();
    let dep_ids = extract_optional_string_array(parsed, "dep_ids").unwrap_or_default();
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
    let session_id = session_id.to_string();
    let current_task_id = task_id.to_string();

    if !delegate_capabilities.is_empty() {
        let Some(capability_checker) = checker_from_session else {
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
        let (store, git_root_str) = detect_workspace_store()?;
        let workspace = store
            .find_or_create_workspace(&git_root_str)
            .map_err(tool_exec_err)?;

        let accept_criteria =
            accept_criteria_description.map(|criteria_description| CreateAcceptCriteriaInput {
                description: criteria_description,
                requires_human_approval: blocking,
                verify_commands,
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

        let parent_id = if current_task_id.is_empty() {
            None
        } else {
            Some(current_task_id.clone())
        };
        let root_session_id = resolve_root_session_id(&store, parent_id.as_deref(), &session_id)?;

        let task = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id,
                parent_id,
                title,
                description,
                created_by: format!("agent:{session_id}:{current_task_id}"),
                priority,
                max_retries: 3,
                dep_ids,
                accept_criteria,
                delegate_capabilities,
                root_session_id,
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

pub(super) fn handle_task_list(parsed: &serde_json::Value) -> Result<String, ToolError> {
    let status_filter = parsed["status_filter"].as_str().map(str::to_string);
    run_blocking_tool(move || {
        let (store, git_root_str) = detect_workspace_store()?;
        let workspace = store
            .find_or_create_workspace(&git_root_str)
            .map_err(tool_exec_err)?;
        let mut tasks = store.list_tasks(&workspace.id).map_err(tool_exec_err)?;

        if let Some(filter) = status_filter {
            let normalized = filter.to_lowercase();
            tasks.retain(|task| task.status.to_string() == normalized);
        }

        serde_json::to_string(&tasks).map_err(tool_exec_err)
    })
}

pub(super) fn handle_task_get(parsed: &serde_json::Value) -> Result<String, ToolError> {
    let id = extract_str(parsed, "id")?.to_string();
    run_blocking_tool(move || {
        let store = sunny_tasks::TaskStore::open_default().map_err(tool_exec_err)?;
        let task = store.get_task(&id).map_err(tool_exec_err)?;
        serde_json::to_string(&task).map_err(tool_exec_err)
    })
}

pub(super) fn handle_task_complete(
    parsed: &serde_json::Value,
    task_id: &str,
    repo_root: &Path,
) -> Result<String, ToolError> {
    let summary = extract_str(parsed, "summary")?.to_string();
    let current_task_id = task_id.to_string();
    let repo_root = repo_root.to_path_buf();

    run_blocking_tool(move || {
        if current_task_id.is_empty() {
            return Ok("No active task context (SUNNY_TASK_ID is not set).".to_string());
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

        let store = sunny_tasks::TaskStore::open_default().map_err(tool_exec_err)?;
        store
            .set_result(&current_task_id, Some(&diff), &summary, &files, None)
            .map_err(tool_exec_err)?;

        Ok("Task marked complete. Verification will run shortly.".to_string())
    })
}

pub(super) fn handle_task_fail(
    parsed: &serde_json::Value,
    task_id: &str,
) -> Result<String, ToolError> {
    let error = extract_str(parsed, "error")?.to_string();
    let current_task_id = task_id.to_string();

    run_blocking_tool(move || {
        if current_task_id.is_empty() {
            return Ok("No active task context (SUNNY_TASK_ID is not set).".to_string());
        }

        let store = sunny_tasks::TaskStore::open_default().map_err(tool_exec_err)?;
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

pub(super) fn handle_task_ask_human(
    parsed: &serde_json::Value,
    task_id: &str,
) -> Result<String, ToolError> {
    let question = extract_str(parsed, "question")?.to_string();
    let context = parsed["context"].as_str().map(str::to_string);
    let options = extract_optional_string_array(parsed, "options");
    let current_task_id = task_id.to_string();

    run_blocking_tool(move || {
        if current_task_id.is_empty() {
            return Ok("No active task context (SUNNY_TASK_ID is not set).".to_string());
        }

        let store = sunny_tasks::TaskStore::open_default().map_err(tool_exec_err)?;
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

pub(super) fn handle_task_claim_paths(
    parsed: &serde_json::Value,
    task_id: &str,
) -> Result<String, ToolError> {
    let paths = extract_string_array(parsed, "paths")?;
    let claim_type = parsed["claim_type"].as_str().unwrap_or("write").to_string();
    let current_task_id = task_id.to_string();

    run_blocking_tool(move || {
        if current_task_id.is_empty() {
            return Ok("No active task context (SUNNY_TASK_ID is not set).".to_string());
        }

        let store = sunny_tasks::TaskStore::open_default().map_err(tool_exec_err)?;
        for path in &paths {
            store
                .add_path_claim(&current_task_id, path, &claim_type)
                .map_err(tool_exec_err)?;
        }

        Ok("Path claims registered.".to_string())
    })
}
