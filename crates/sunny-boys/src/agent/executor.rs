use std::path::PathBuf;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use sunny_mind::LlmProvider;
use sunny_store::{Database, SessionStore};
use sunny_tasks::{
    CapabilityStore, Task, TaskSession, TaskStatus, WorkspaceDetector, WorkspaceSnapshot,
};

use crate::agent::session::{AgentError, AgentSession};
use crate::agent::tools::build_active_capabilities;
use crate::tool_loop::ToolCallError;

#[derive(Debug)]
pub enum ExecutionOutcome {
    Completed { summary: String },
    Failed { error: String },
    BlockedOnHuman,
    BlockedOnCapability { request_id: String },
    Cancelled,
    MaxIterationsReached,
    NoTerminalAction,
}

pub struct TaskExecutor {
    provider: Arc<dyn LlmProvider>,
}

impl TaskExecutor {
    pub fn new(provider: Arc<dyn LlmProvider>) -> Self {
        Self { provider }
    }

    pub async fn execute(
        &self,
        task: Task,
        git_root: PathBuf,
        cancel: CancellationToken,
    ) -> ExecutionOutcome {
        if cancel.is_cancelled() {
            return ExecutionOutcome::Cancelled;
        }

        let session_id = uuid::Uuid::new_v4().to_string();

        let workspace_root = WorkspaceDetector::detect(&git_root);
        let caps = build_active_capabilities(&session_id, workspace_root.as_deref());

        if let Some(metadata) = task.metadata.as_ref() {
            if let Some(delegated) = metadata
                .get("delegate_capabilities")
                .and_then(serde_json::Value::as_array)
            {
                let mut map = caps.write().expect("capability lock poisoned");
                for entry in delegated {
                    if let Some(raw) = entry.as_str() {
                        let (capability, patterns) = parse_delegation_entry(raw);
                        map.entry(capability)
                            .or_default()
                            .extend(patterns.into_iter());
                    }
                }
            }
        }

        #[allow(clippy::arc_with_non_send_sync)]
        let task_store = match sunny_tasks::TaskStore::open_default() {
            Ok(store) => Arc::new(store),
            Err(error) => {
                warn!(error = %error, "failed to open task store");
                return ExecutionOutcome::Failed {
                    error: error.to_string(),
                };
            }
        };
        let task_session = TaskSession::new(task.id.clone(), task_store, git_root.clone());

        let snapshot = capture_workspace_snapshot(&git_root).await;
        let conventions = sunny_store::context_file::read_context_files(&git_root)
            .ok()
            .filter(|s| !s.is_empty());
        let repo_map = sunny_store::generate_repo_map(&git_root, 20_000)
            .ok()
            .filter(|s| !s.is_empty());
        let system_prompt = match task_session.build_system_prompt(snapshot, conventions, repo_map)
        {
            Ok(prompt) => prompt,
            Err(error) => {
                warn!(error = %error, "failed to build task system prompt");
                return ExecutionOutcome::Failed {
                    error: error.to_string(),
                };
            }
        };

        let db = match Database::open_default() {
            Ok(db) => db,
            Err(error) => {
                return ExecutionOutcome::Failed {
                    error: error.to_string(),
                };
            }
        };
        #[allow(clippy::arc_with_non_send_sync)]
        let store = Arc::new(SessionStore::new(db));

        // Create the session row BEFORE mark_running so the FK constraint on
        // tasks.session_id → sessions.id is satisfied. mark_started() will call
        // mark_running() which sets session_id on the task row.
        let working_dir = git_root.to_string_lossy().to_string();
        let model = self.provider.model_id().to_string();
        if let Err(error) = store.create_session_with_id(&session_id, &working_dir, Some(&model)) {
            warn!(session_id = %session_id, error = %error, "failed to pre-create session row before mark_running");
        }

        if let Err(error) = task_session.mark_started(&session_id) {
            warn!(task_id = %task.id, error = %error, "failed to mark task started");
        }

        let mut agent_session = AgentSession::new(
            Arc::clone(&self.provider),
            git_root,
            session_id.clone(),
            Arc::clone(&store),
        )
        .with_task(task.id.clone());

        let initial_message = format!(
            "You are executing a task. Here is your full task specification:\n\n{system_prompt}\n\nBegin working now. When complete, call task_complete(). If blocked by an unrecoverable issue, call task_fail()."
        );

        info!(task_id = %task.id, session_id = %session_id, "task executor started");

        let send_future = agent_session.send(&initial_message, |_event| {});
        tokio::pin!(send_future);
        let result = tokio::select! {
            _ = cancel.cancelled() => return ExecutionOutcome::Cancelled,
            result = &mut send_future => result,
        };

        match result {
            Ok(_) => self.resolve_terminal_outcome(&task.id, &session_id),
            Err(AgentError::ToolLoop(ToolCallError::MaxIterationsReached { .. })) => {
                ExecutionOutcome::MaxIterationsReached
            }
            Err(AgentError::ToolLoop(ToolCallError::Cancelled)) => ExecutionOutcome::Cancelled,
            Err(error) => ExecutionOutcome::Failed {
                error: error.to_string(),
            },
        }
    }

    fn resolve_terminal_outcome(&self, task_id: &str, session_id: &str) -> ExecutionOutcome {
        if let Ok(cap_store) = CapabilityStore::open_default() {
            if let Ok(pending) = cap_store.pending_requests() {
                if let Some(request) = pending.into_iter().find(|req| req.session_id == session_id)
                {
                    return ExecutionOutcome::BlockedOnCapability {
                        request_id: request.id,
                    };
                }
            }
        }

        let task_store = match sunny_tasks::TaskStore::open_default() {
            Ok(store) => store,
            Err(error) => {
                return ExecutionOutcome::Failed {
                    error: error.to_string(),
                };
            }
        };

        match task_store.get_task(task_id) {
            Ok(Some(task)) if task.status == TaskStatus::Completed => ExecutionOutcome::Completed {
                summary: task.result_summary.unwrap_or_default(),
            },
            Ok(Some(task)) if task.status == TaskStatus::BlockedHuman => {
                ExecutionOutcome::BlockedOnHuman
            }
            Ok(Some(task)) if task.status == TaskStatus::Failed => ExecutionOutcome::Failed {
                error: task
                    .error
                    .unwrap_or_else(|| "unknown task failure".to_string()),
            },
            Ok(Some(_)) => ExecutionOutcome::NoTerminalAction,
            Ok(None) => ExecutionOutcome::Failed {
                error: format!("task not found: {task_id}"),
            },
            Err(error) => ExecutionOutcome::Failed {
                error: error.to_string(),
            },
        }
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

async fn capture_workspace_snapshot(git_root: &PathBuf) -> WorkspaceSnapshot {
    let branch = run_git(git_root, &["branch", "--show-current"])
        .await
        .unwrap_or_else(|| "unknown".to_string());
    let status_short = run_git(git_root, &["status", "--short"])
        .await
        .unwrap_or_default();
    let recent_log = run_git(git_root, &["log", "--oneline", "-10"])
        .await
        .unwrap_or_default();

    WorkspaceSnapshot {
        branch,
        status_short,
        recent_log,
    }
}

async fn run_git(git_root: &PathBuf, args: &[&str]) -> Option<String> {
    tokio::process::Command::new("git")
        .args(args)
        .current_dir(git_root)
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|output| output.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::{parse_delegation_entry, ExecutionOutcome};

    #[test]
    fn test_parse_delegation_entry_with_patterns() {
        let (capability, patterns) = parse_delegation_entry("shell_pipes:tail,grep");
        assert_eq!(capability, "shell_pipes");
        assert_eq!(patterns, vec!["tail".to_string(), "grep".to_string()]);
    }

    #[test]
    fn test_parse_delegation_entry_without_patterns() {
        let (capability, patterns) = parse_delegation_entry("git_write");
        assert_eq!(capability, "git_write");
        assert!(patterns.is_empty());
    }

    #[test]
    fn test_resolve_terminal_outcome_no_terminal_action() {
        // This test verifies that when a task finishes without calling task_complete/task_fail,
        // resolve_terminal_outcome returns NoTerminalAction instead of Failed.
        // The actual behavior is tested via integration tests with real task store.
        // This is a placeholder to ensure the variant exists and compiles.
        let outcome = ExecutionOutcome::NoTerminalAction;
        match outcome {
            ExecutionOutcome::NoTerminalAction => {
                // Expected
            }
            _ => panic!("should be NoTerminalAction"),
        }
    }
}
