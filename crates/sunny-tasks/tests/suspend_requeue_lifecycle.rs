//! Integration test for the full suspend→re-queue→complete lifecycle.
//!
//! Tests the state machine logic end-to-end without requiring LLM calls or a
//! running scheduler. All suspension/re-queue functions are called directly.

use std::sync::Arc;

use sunny_store::Database;
use sunny_tasks::{
    CreateTaskInput, TaskError, TaskSession, TaskStatus, TaskStore, WorkspaceSnapshot,
};

// Re-implement the suspension logic inline so the integration test is self-contained
// and exercises the same code paths work.rs uses.

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
    (Arc::new(TaskStore::new(db)), dir)
}

fn make_task_with_parent(
    store: &TaskStore,
    workspace_id: &str,
    title: &str,
    parent_id: Option<String>,
) -> sunny_tasks::Task {
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
            metadata: None,
        })
        .expect("create task")
}

/// Simulate suspension detection (mirrors handle_no_terminal_action in work.rs)
fn handle_no_terminal_action(store: &TaskStore, task_id: &str) -> Result<(), TaskError> {
    let children = store.list_children(task_id)?;

    if children.is_empty() {
        store.set_error(task_id, "agent ended without terminal action")?;
        return Ok(());
    }

    let all_terminal = children.iter().all(|c| {
        matches!(
            c.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        )
    });

    if all_terminal {
        store.update_status(task_id, TaskStatus::Pending)?;
        return Ok(());
    }

    // Check suspension cap
    let task = store.get_task(task_id)?.ok_or(TaskError::NotFound {
        id: task_id.to_string(),
    })?;
    let suspension_count = task
        .metadata
        .as_ref()
        .and_then(|m| m.get("suspension_count"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if suspension_count >= 5 {
        store.set_error(task_id, "max suspension count (5) exceeded")?;
        return Ok(());
    }

    let new_count = suspension_count + 1;
    let new_metadata = {
        let mut m = task.metadata.unwrap_or_else(|| serde_json::json!({}));
        m["suspension_count"] = serde_json::json!(new_count);
        m
    };
    store.update_metadata(task_id, new_metadata)?;
    store.update_status(task_id, TaskStatus::Suspended)?;
    Ok(())
}

/// Re-queue suspended parent when all children are terminal (mirrors check_parent_requeue)
fn check_parent_requeue(store: &TaskStore, child_id: &str) -> Result<(), TaskError> {
    let child = match store.get_task(child_id)? {
        Some(t) => t,
        None => return Ok(()),
    };
    let parent_id = match child.parent_id.as_deref() {
        Some(id) => id.to_string(),
        None => return Ok(()),
    };
    let parent = match store.get_task(&parent_id)? {
        Some(t) => t,
        None => return Ok(()),
    };
    if parent.status != TaskStatus::Suspended {
        return Ok(());
    }
    if store.all_children_terminal(&parent_id)? {
        store.update_status(&parent_id, TaskStatus::Pending)?;
    }
    Ok(())
}

/// Startup recovery (mirrors recover_suspended_tasks)
fn recover_suspended_tasks(store: &TaskStore, workspace_id: &str) -> Result<(), TaskError> {
    let suspended = store.list_tasks_by_status(workspace_id, TaskStatus::Suspended)?;
    for task in suspended {
        if store.all_children_terminal(&task.id)? {
            store.update_status(&task.id, TaskStatus::Pending)?;
        }
    }
    Ok(())
}

/// Full lifecycle: create parent → suspend → complete children → re-queue → verify prompt
#[test]
fn test_worker_suspend_and_requeue_lifecycle() {
    let (store, _dir) = make_store();
    let workspace = store
        .find_or_create_workspace("/tmp/repo")
        .expect("workspace");

    // Create orchestrator (parent) task
    let parent = make_task_with_parent(&store, &workspace.id, "orchestrator", None);

    // Create 2 child tasks
    let child1 = make_task_with_parent(&store, &workspace.id, "child-1", Some(parent.id.clone()));
    let child2 = make_task_with_parent(&store, &workspace.id, "child-2", Some(parent.id.clone()));

    // 1. Simulate executor returning NoTerminalAction for parent
    //    (parent has 2 pending children → should suspend)
    handle_no_terminal_action(&store, &parent.id).expect("suspension should work");
    let parent_state = store.get_task(&parent.id).expect("load").expect("exists");
    assert_eq!(
        parent_state.status,
        TaskStatus::Suspended,
        "parent should be Suspended"
    );

    // 2. Child 1 completes — parent still has pending child 2
    store
        .set_result(&child1.id, None, "child1 result", &[], None)
        .expect("complete child1");
    check_parent_requeue(&store, &child1.id).expect("requeue check");
    let parent_state = store.get_task(&parent.id).expect("load").expect("exists");
    assert_eq!(
        parent_state.status,
        TaskStatus::Suspended,
        "parent still suspended (child2 pending)"
    );

    // 3. Child 2 completes — all children terminal → parent re-queued
    store
        .set_result(&child2.id, None, "child2 result", &[], None)
        .expect("complete child2");
    check_parent_requeue(&store, &child2.id).expect("requeue check");
    let parent_state = store.get_task(&parent.id).expect("load").expect("exists");
    assert_eq!(
        parent_state.status,
        TaskStatus::Pending,
        "parent should be re-queued (Pending)"
    );

    // 4. Verify build_system_prompt for re-queued parent includes children's results
    let task_session = TaskSession::new(
        parent.id.clone(),
        Arc::clone(&store),
        std::path::PathBuf::from("/tmp/repo"),
    );
    let prompt = task_session
        .build_system_prompt(
            WorkspaceSnapshot {
                branch: "master".to_string(),
                status_short: "".to_string(),
                recent_log: "abc123 test".to_string(),
            },
            None,
            None,
        )
        .expect("build prompt");

    assert!(
        prompt.contains("Child Task Results"),
        "prompt should include child results section"
    );
    assert!(
        prompt.contains("child1 result"),
        "prompt should include child1 result"
    );
    assert!(
        prompt.contains("child2 result"),
        "prompt should include child2 result"
    );

    // 5. Mark parent completed
    store
        .set_result(&parent.id, None, "orchestrator done", &[], None)
        .expect("complete parent");
    let final_state = store.get_task(&parent.id).expect("load").expect("exists");
    assert_eq!(final_state.status, TaskStatus::Completed);
}

/// Max suspension cap: 6th NoTerminalAction should fail the task
#[test]
fn test_max_suspension_cap_fails_task() {
    let (store, _dir) = make_store();
    let workspace = store
        .find_or_create_workspace("/tmp/repo")
        .expect("workspace");

    let parent = make_task_with_parent(&store, &workspace.id, "capped-parent", None);
    // One pending child so suspension is triggered (not the "no children" path)
    let _child = make_task_with_parent(&store, &workspace.id, "child", Some(parent.id.clone()));

    // Simulate 5 suspensions (sets suspension_count = 5)
    for _ in 0..5 {
        // Set back to running so handle_no_terminal_action can be called again
        store
            .update_status(&parent.id, TaskStatus::Running)
            .expect("reset to running");
        handle_no_terminal_action(&store, &parent.id).expect("suspension should work");
    }

    let state = store.get_task(&parent.id).expect("load").expect("exists");
    assert_eq!(state.status, TaskStatus::Suspended);
    let count = state
        .metadata
        .as_ref()
        .and_then(|m| m.get("suspension_count"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    assert_eq!(count, 5);

    // 6th attempt — should fail the task
    store
        .update_status(&parent.id, TaskStatus::Running)
        .expect("reset to running");
    handle_no_terminal_action(&store, &parent.id).expect("should not error");
    let final_state = store.get_task(&parent.id).expect("load").expect("exists");
    assert_eq!(
        final_state.status,
        TaskStatus::Failed,
        "task should fail after max suspension cap"
    );
    assert!(
        final_state
            .error
            .as_deref()
            .unwrap_or("")
            .contains("max suspension count"),
        "error should mention max suspension count"
    );
}

/// Startup recovery: orphaned suspended task re-queued when children are terminal
#[test]
fn test_startup_recovery_requeues_stale_suspended() {
    let (store, _dir) = make_store();
    let workspace = store
        .find_or_create_workspace("/tmp/repo")
        .expect("workspace");

    let parent = make_task_with_parent(&store, &workspace.id, "orphaned-parent", None);
    let child = make_task_with_parent(
        &store,
        &workspace.id,
        "orphaned-child",
        Some(parent.id.clone()),
    );

    // Manually set parent to Suspended (simulating crash before re-queue)
    store
        .update_status(&parent.id, TaskStatus::Suspended)
        .expect("suspend parent");
    // Complete the child
    store
        .set_result(&child.id, None, "child done", &[], None)
        .expect("complete child");

    // Startup recovery should re-queue the parent
    recover_suspended_tasks(&store, &workspace.id).expect("recovery should work");

    let parent_state = store.get_task(&parent.id).expect("load").expect("exists");
    assert_eq!(
        parent_state.status,
        TaskStatus::Pending,
        "startup recovery should re-queue orphaned suspended parent"
    );
}

/// Worker suspends task when child still pending (not re-queued prematurely)
#[test]
fn test_worker_does_not_requeue_when_child_still_running() {
    let (store, _dir) = make_store();
    let workspace = store
        .find_or_create_workspace("/tmp/repo")
        .expect("workspace");

    let parent = make_task_with_parent(&store, &workspace.id, "parent", None);
    let child1 = make_task_with_parent(&store, &workspace.id, "child1", Some(parent.id.clone()));
    let _child2 = make_task_with_parent(&store, &workspace.id, "child2", Some(parent.id.clone()));

    // Suspend parent
    handle_no_terminal_action(&store, &parent.id).expect("suspend");
    let state = store.get_task(&parent.id).expect("load").expect("exists");
    assert_eq!(state.status, TaskStatus::Suspended);

    // child1 completes but child2 is still pending
    store
        .set_result(&child1.id, None, "c1 done", &[], None)
        .expect("complete child1");
    check_parent_requeue(&store, &child1.id).expect("requeue check");

    // Parent should still be Suspended
    let state = store.get_task(&parent.id).expect("load").expect("exists");
    assert_eq!(
        state.status,
        TaskStatus::Suspended,
        "parent should still be suspended while child2 is pending"
    );
}
