//! Worker domain logic for task suspension, re-queueing, and recovery.
//!
//! This module contains the core business logic for handling tasks that end
//! without calling task_complete or task_fail, managing suspension state,
//! and recovering suspended tasks at startup.

use tracing::{info, warn};

use crate::error::TaskError;
use crate::model::TaskStatus;
use crate::store::TaskStore;

/// The outcome classification returned by handle_no_terminal_action.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerAction {
    /// Task was suspended, awaiting child task completions.
    Suspended,
    /// Task was immediately re-queued (all children were already terminal).
    RequeuedImmediately,
    /// Task was marked as failed with an error message.
    Failed(String),
}

/// Called when an agent ends without calling task_complete or task_fail.
/// Returns the action taken so the caller can log/react accordingly.
///
/// # Logic
/// - If task has no children → mark as Failed
/// - If all children are terminal → re-queue immediately (Pending)
/// - If some children are pending → suspend and track suspension count
/// - If suspension count reaches 5 → mark as Failed
pub fn handle_no_terminal_action(
    store: &TaskStore,
    task_id: &str,
) -> Result<WorkerAction, TaskError> {
    let children = match store.list_children(task_id) {
        Ok(c) => c,
        Err(e) => {
            warn!(task_id = %task_id, error = %e, "failed to list children for suspension check");
            return Err(e);
        }
    };

    if children.is_empty() {
        // No children — genuinely failed without terminal action
        let msg = "agent ended without calling task_complete or task_fail";
        if let Err(e) = store.set_error(task_id, msg) {
            warn!(task_id = %task_id, error = %e, "failed to set error on task");
        }
        warn!(task_id = %task_id, "task failed: no terminal action and no children");
        return Ok(WorkerAction::Failed(msg.to_string()));
    }

    let all_terminal = children.iter().all(|c| {
        matches!(
            c.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        )
    });

    if all_terminal {
        // Children all done already — re-queue immediately
        if let Err(e) = store.update_status(task_id, TaskStatus::Pending) {
            warn!(task_id = %task_id, error = %e, "failed to re-queue task");
            return Err(e);
        }
        info!(task_id = %task_id, "children already complete, re-queuing parent");
        return Ok(WorkerAction::RequeuedImmediately);
    }

    // Check and enforce max suspension cap (5) via task metadata
    let task = match store.get_task(task_id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            warn!(task_id = %task_id, "task not found during suspension check");
            return Err(TaskError::NotFound {
                id: task_id.to_string(),
            });
        }
        Err(e) => {
            warn!(task_id = %task_id, error = %e, "failed to load task for suspension check");
            return Err(e);
        }
    };

    let suspension_count = task
        .metadata
        .as_ref()
        .and_then(|m| m.get("suspension_count"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if suspension_count >= 5 {
        let msg = "max suspension count (5) exceeded";
        if let Err(e) = store.set_error(task_id, msg) {
            warn!(task_id = %task_id, error = %e, "failed to mark task failed after cap");
        }
        warn!(task_id = %task_id, suspension_count, "task failed: max suspension count exceeded");
        return Ok(WorkerAction::Failed(msg.to_string()));
    }

    // Increment suspension_count and suspend
    let new_count = suspension_count + 1;
    let new_metadata = {
        let mut m = task.metadata.unwrap_or_else(|| serde_json::json!({}));
        m["suspension_count"] = serde_json::json!(new_count);
        m
    };
    if let Err(e) = store.update_metadata(task_id, new_metadata) {
        warn!(task_id = %task_id, error = %e, "failed to update suspension_count metadata");
    }
    if let Err(e) = store.update_status(task_id, TaskStatus::Suspended) {
        warn!(task_id = %task_id, error = %e, "failed to suspend task");
        return Err(e);
    }
    info!(task_id = %task_id, children = children.len(), suspension_count = new_count, "task suspended awaiting children");
    Ok(WorkerAction::Suspended)
}

/// Re-queues a suspended parent when all its children reach terminal state.
/// Returns Ok(true) if parent was re-queued, Ok(false) if no action taken.
///
/// # Logic
/// - Load the child task
/// - If child has no parent, return Ok(false)
/// - Load the parent task
/// - If parent is not Suspended, return Ok(false)
/// - If all children are terminal, re-queue parent to Pending and return Ok(true)
pub fn check_parent_requeue(store: &TaskStore, child_task_id: &str) -> Result<bool, TaskError> {
    let child = match store.get_task(child_task_id) {
        Ok(Some(t)) => t,
        Ok(None) => return Ok(false),
        Err(e) => return Err(e),
    };
    let Some(ref parent_id) = child.parent_id else {
        return Ok(false);
    };
    let parent = match store.get_task(parent_id) {
        Ok(Some(t)) => t,
        Ok(None) => return Ok(false),
        Err(e) => return Err(e),
    };
    if parent.status != TaskStatus::Suspended {
        return Ok(false);
    }
    match store.all_children_terminal(parent_id) {
        Ok(true) => {
            if let Err(e) = store.update_status(parent_id, TaskStatus::Pending) {
                warn!(parent_id = %parent_id, error = %e, "failed to re-queue suspended parent");
                return Err(e);
            }
            info!(parent_id = %parent_id, "re-queued suspended parent: all children terminal");
            Ok(true)
        }
        Ok(false) => Ok(false),
        Err(e) => {
            warn!(parent_id = %parent_id, error = %e, "failed to check all_children_terminal");
            Err(e)
        }
    }
}

/// Startup recovery: finds suspended tasks whose children are all terminal and re-queues them.
/// Returns the count of tasks successfully re-queued.
///
/// # Logic
/// - List all suspended tasks in the workspace
/// - For each suspended task, check if all children are terminal
/// - If yes, re-queue to Pending and increment count
/// - Log warnings for individual failures but continue processing
pub fn recover_suspended_tasks(store: &TaskStore, workspace_id: &str) -> Result<usize, TaskError> {
    let suspended = match store.list_tasks_by_status(workspace_id, TaskStatus::Suspended) {
        Ok(tasks) => tasks,
        Err(e) => {
            warn!(error = %e, "startup recovery: failed to list suspended tasks");
            return Err(e);
        }
    };
    let mut count = 0;
    for task in suspended {
        match store.all_children_terminal(&task.id) {
            Ok(true) => {
                if let Err(e) = store.update_status(&task.id, TaskStatus::Pending) {
                    warn!(task_id = %task.id, error = %e, "startup recovery: failed to re-queue");
                } else {
                    info!(task_id = %task.id, "startup recovery: re-queued stale suspended task");
                    count += 1;
                }
            }
            Ok(false) => {}
            Err(e) => {
                warn!(task_id = %task.id, error = %e, "startup recovery: all_children_terminal failed");
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CreateTaskInput;
    use std::sync::Arc;
    use sunny_store::Database;

    fn ensure_task_tables(db: &Database) {
        db.connection()
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS workspaces (
                    id          TEXT PRIMARY KEY,
                    git_root    TEXT NOT NULL UNIQUE,
                    name        TEXT,
                    created_at  TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS tasks (
                    id                  TEXT PRIMARY KEY,
                    workspace_id        TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                    root_session_id     TEXT NOT NULL DEFAULT '',
                    parent_id           TEXT REFERENCES tasks(id) ON DELETE SET NULL,
                    title               TEXT NOT NULL,
                    description         TEXT NOT NULL DEFAULT '',
                    status              TEXT NOT NULL DEFAULT 'pending',
                    session_id          TEXT,
                    created_by          TEXT NOT NULL DEFAULT 'human',
                    priority            INTEGER NOT NULL DEFAULT 0,
                    created_at          TEXT NOT NULL,
                    updated_at          TEXT NOT NULL,
                    started_at          TEXT,
                    completed_at        TEXT,
                    result_diff         TEXT,
                    result_summary      TEXT,
                    result_files        TEXT,
                    result_verify       TEXT,
                    error               TEXT,
                    retry_count         INTEGER NOT NULL DEFAULT 0,
                    max_retries         INTEGER NOT NULL DEFAULT 3,
                    metadata            TEXT
                 );
                 CREATE TABLE IF NOT EXISTS task_deps (
                    task_id    TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    depends_on TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    PRIMARY KEY (task_id, depends_on)
                 );
                 CREATE TABLE IF NOT EXISTS accept_criteria (
                    id                      INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_id                 TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    description             TEXT NOT NULL,
                    requires_human_approval INTEGER NOT NULL DEFAULT 0
                 );
                 CREATE TABLE IF NOT EXISTS task_verify_commands (
                    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                    criteria_id         INTEGER NOT NULL REFERENCES accept_criteria(id) ON DELETE CASCADE,
                    command             TEXT NOT NULL,
                    expected_exit_code  INTEGER NOT NULL DEFAULT 0,
                    timeout_secs        INTEGER DEFAULT 60,
                    seq                 INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS human_questions (
                    id          TEXT PRIMARY KEY,
                    task_id     TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    question    TEXT NOT NULL,
                    context     TEXT,
                    options     TEXT,
                    answer      TEXT,
                    asked_at    TEXT NOT NULL,
                    answered_at TEXT
                 );
                 CREATE TABLE IF NOT EXISTS task_path_claims (
                    task_id      TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    path_pattern TEXT NOT NULL,
                    claim_type   TEXT NOT NULL,
                    PRIMARY KEY (task_id, path_pattern)
                 );",
            )
            .expect("should create task schema");
    }

    fn make_store() -> (Arc<TaskStore>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Database::open(dir.path().join("test.db").as_path()).expect("open db");
        ensure_task_tables(&db);
        #[allow(clippy::arc_with_non_send_sync)]
        let store = Arc::new(TaskStore::new(db));
        (store, dir)
    }

    fn make_task_with_parent(
        store: &TaskStore,
        workspace_id: &str,
        title: &str,
        parent_id: Option<String>,
    ) -> crate::Task {
        store
            .create_task(CreateTaskInput {
                workspace_id: workspace_id.to_string(),
                parent_id,
                title: title.to_string(),
                description: format!("desc for {title}"),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: String::new(),
                metadata: None,
            })
            .expect("create task")
    }

    #[test]
    fn test_handle_no_terminal_action_no_children_returns_failed() {
        let (store, _dir) = make_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("workspace");

        let task = make_task_with_parent(&store, &workspace.id, "orphan", None);

        let result = handle_no_terminal_action(&store, &task.id).expect("should return action");
        assert_eq!(
            result,
            WorkerAction::Failed(
                "agent ended without calling task_complete or task_fail".to_string()
            )
        );

        let final_state = store.get_task(&task.id).expect("load").expect("exists");
        assert_eq!(final_state.status, TaskStatus::Failed);
        assert!(final_state.error.is_some());
    }

    #[test]
    fn test_handle_no_terminal_action_all_children_terminal_returns_requeued() {
        let (store, _dir) = make_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("workspace");

        let parent = make_task_with_parent(&store, &workspace.id, "parent", None);
        let child1 =
            make_task_with_parent(&store, &workspace.id, "child1", Some(parent.id.clone()));
        let child2 =
            make_task_with_parent(&store, &workspace.id, "child2", Some(parent.id.clone()));

        // Complete both children
        store
            .set_result(&child1.id, None, "done", &[], None)
            .expect("complete child1");
        store
            .set_result(&child2.id, None, "done", &[], None)
            .expect("complete child2");

        let result = handle_no_terminal_action(&store, &parent.id).expect("should return action");
        assert_eq!(result, WorkerAction::RequeuedImmediately);

        let final_state = store.get_task(&parent.id).expect("load").expect("exists");
        assert_eq!(final_state.status, TaskStatus::Pending);
    }

    #[test]
    fn test_handle_no_terminal_action_pending_children_returns_suspended() {
        let (store, _dir) = make_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("workspace");

        let parent = make_task_with_parent(&store, &workspace.id, "parent", None);
        let _child = make_task_with_parent(&store, &workspace.id, "child", Some(parent.id.clone()));

        let result = handle_no_terminal_action(&store, &parent.id).expect("should return action");
        assert_eq!(result, WorkerAction::Suspended);

        let final_state = store.get_task(&parent.id).expect("load").expect("exists");
        assert_eq!(final_state.status, TaskStatus::Suspended);
        let count = final_state
            .metadata
            .as_ref()
            .and_then(|m| m.get("suspension_count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert_eq!(count, 1);
    }

    #[test]
    fn test_handle_no_terminal_action_max_suspension_count_returns_failed() {
        let (store, _dir) = make_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("workspace");

        let parent = make_task_with_parent(&store, &workspace.id, "parent", None);
        let _child = make_task_with_parent(&store, &workspace.id, "child", Some(parent.id.clone()));

        // Simulate 5 suspensions
        for _ in 0..5 {
            store
                .update_status(&parent.id, TaskStatus::Running)
                .expect("reset to running");
            handle_no_terminal_action(&store, &parent.id).expect("suspension should work");
        }

        let state = store.get_task(&parent.id).expect("load").expect("exists");
        assert_eq!(state.status, TaskStatus::Suspended);

        // 6th attempt should fail
        store
            .update_status(&parent.id, TaskStatus::Running)
            .expect("reset to running");
        let result = handle_no_terminal_action(&store, &parent.id).expect("should return action");
        assert!(matches!(result, WorkerAction::Failed(_)));

        let final_state = store.get_task(&parent.id).expect("load").expect("exists");
        assert_eq!(final_state.status, TaskStatus::Failed);
    }

    #[test]
    fn test_check_parent_requeue_no_parent_returns_false() {
        let (store, _dir) = make_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("workspace");

        let task = make_task_with_parent(&store, &workspace.id, "orphan", None);

        let result = check_parent_requeue(&store, &task.id).expect("should return bool");
        assert!(!result);
    }

    #[test]
    fn test_check_parent_requeue_parent_suspended_all_done_returns_true() {
        let (store, _dir) = make_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("workspace");

        let parent = make_task_with_parent(&store, &workspace.id, "parent", None);
        let child = make_task_with_parent(&store, &workspace.id, "child", Some(parent.id.clone()));

        // Suspend parent
        store
            .update_status(&parent.id, TaskStatus::Suspended)
            .expect("suspend parent");

        // Complete child
        store
            .set_result(&child.id, None, "done", &[], None)
            .expect("complete child");

        let result = check_parent_requeue(&store, &child.id).expect("should return bool");
        assert!(result);

        let final_state = store.get_task(&parent.id).expect("load").expect("exists");
        assert_eq!(final_state.status, TaskStatus::Pending);
    }

    #[test]
    fn test_recover_suspended_tasks_requeues_stale_suspended_returns_count() {
        let (store, _dir) = make_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("workspace");

        let parent1 = make_task_with_parent(&store, &workspace.id, "parent1", None);
        let child1 =
            make_task_with_parent(&store, &workspace.id, "child1", Some(parent1.id.clone()));

        let parent2 = make_task_with_parent(&store, &workspace.id, "parent2", None);
        let child2 =
            make_task_with_parent(&store, &workspace.id, "child2", Some(parent2.id.clone()));

        // Suspend both parents
        store
            .update_status(&parent1.id, TaskStatus::Suspended)
            .expect("suspend parent1");
        store
            .update_status(&parent2.id, TaskStatus::Suspended)
            .expect("suspend parent2");

        // Complete all children
        store
            .set_result(&child1.id, None, "done", &[], None)
            .expect("complete child1");
        store
            .set_result(&child2.id, None, "done", &[], None)
            .expect("complete child2");

        let count = recover_suspended_tasks(&store, &workspace.id).expect("recovery should work");
        assert_eq!(count, 2);

        let state1 = store.get_task(&parent1.id).expect("load").expect("exists");
        assert_eq!(state1.status, TaskStatus::Pending);

        let state2 = store.get_task(&parent2.id).expect("load").expect("exists");
        assert_eq!(state2.status, TaskStatus::Pending);
    }
}
