use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::model::Task;
use crate::model::TaskStatus;
use crate::store::TaskStore;

#[derive(Debug, Clone)]
pub struct TaskReadyEvent {
    pub task: Task,
    pub workspace_id: String,
}

pub struct TaskScheduler {
    store: Arc<TaskStore>,
    workspace_id: String,
    poll_interval: Duration,
    max_concurrent: usize,
    ready_tx: Option<mpsc::UnboundedSender<TaskReadyEvent>>,
}

impl TaskScheduler {
    pub fn new(store: Arc<TaskStore>, workspace_id: String, max_concurrent: usize) -> Self {
        Self {
            store,
            workspace_id,
            poll_interval: Duration::from_secs(2),
            max_concurrent,
            ready_tx: None,
        }
    }

    pub fn with_ready_channel(mut self, tx: mpsc::UnboundedSender<TaskReadyEvent>) -> Self {
        self.ready_tx = Some(tx);
        self
    }

    pub async fn run(self, cancel: CancellationToken) {
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    info!(event = "scheduler.stopped", "task scheduler stopped");
                    break;
                }
                _ = tokio::time::sleep(self.poll_interval) => {
                    if let Err(error) = self.tick() {
                        warn!(error = %error, "scheduler tick error");
                    }
                }
            }
        }
    }

    fn tick(&self) -> Result<(), crate::error::TaskError> {
        let store = Arc::clone(&self.store);
        let workspace_id = self.workspace_id.clone();
        let max_concurrent = self.max_concurrent;

        let run_tick = || {
            let running = store.list_running_tasks(&workspace_id)?;
            let slots = max_concurrent.saturating_sub(running.len());
            if slots == 0 {
                return Ok(());
            }

            let ready = store.list_ready_tasks(&workspace_id, slots)?;
            for task in ready {
                if let Ok(claims) = store.get_path_claims(&task.id) {
                    for claim in &claims {
                        if let Ok(conflicts) =
                            store.find_conflicting_claims(&task.id, &claim.path_pattern)
                        {
                            if !conflicts.is_empty() {
                                warn!(
                                    task_id = %task.id,
                                    path = %claim.path_pattern,
                                    "advisory: path conflict with running task(s)"
                                );
                            }
                        }
                    }
                }

                store.update_status(&task.id, TaskStatus::Running)?;
                if let Some(tx) = &self.ready_tx {
                    let mut ready_task = task.clone();
                    ready_task.status = TaskStatus::Running;
                    let _ = tx.send(TaskReadyEvent {
                        task: ready_task,
                        workspace_id: workspace_id.clone(),
                    });
                }
                info!(
                    task_id = %task.id,
                    title = %task.title,
                    "scheduler: task marked running"
                );
            }

            Ok(())
        };

        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(run_tick)
            }
            _ => run_tick(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arc_with_non_send_sync)]

    use std::sync::Arc;

    use sunny_store::Database;

    use super::TaskScheduler;
    use crate::model::{CreateTaskInput, Task, TaskStatus, Workspace};
    use crate::store::TaskStore;

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
                    id              TEXT PRIMARY KEY,
                    workspace_id    TEXT NOT NULL REFERENCES workspaces(id),
                    parent_id       TEXT REFERENCES tasks(id),
                    title           TEXT NOT NULL,
                    description     TEXT NOT NULL,
                    status          TEXT NOT NULL DEFAULT 'pending',
                    session_id      TEXT REFERENCES sessions(id),
                    created_by      TEXT NOT NULL,
                    priority        INTEGER DEFAULT 0,
                    created_at      TEXT NOT NULL,
                    updated_at      TEXT NOT NULL,
                    started_at      TEXT,
                    completed_at    TEXT,
                    result_diff     TEXT,
                    result_summary  TEXT,
                    result_files    TEXT,
                    result_verify   TEXT,
                    error           TEXT,
                    retry_count     INTEGER DEFAULT 0,
                    max_retries     INTEGER DEFAULT 3,
                    metadata        TEXT
                );
                CREATE TABLE IF NOT EXISTS task_deps (
                    task_id     TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    depends_on  TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                    PRIMARY KEY (task_id, depends_on)
                );
                CREATE TABLE IF NOT EXISTS accept_criteria (
                    id                      INTEGER PRIMARY KEY AUTOINCREMENT,
                    task_id                 TEXT NOT NULL UNIQUE REFERENCES tasks(id) ON DELETE CASCADE,
                    description             TEXT NOT NULL,
                    requires_human_approval INTEGER NOT NULL DEFAULT 0
                );
                CREATE TABLE IF NOT EXISTS verify_commands (
                    id                  INTEGER PRIMARY KEY AUTOINCREMENT,
                    criteria_id         INTEGER NOT NULL REFERENCES accept_criteria(id) ON DELETE CASCADE,
                    command             TEXT NOT NULL,
                    expected_exit_code  INTEGER DEFAULT 0,
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
        let dir = tempfile::tempdir().expect("should create temp dir");
        let db = Database::open(dir.path().join("test.db").as_path()).expect("should open db");
        ensure_task_tables(&db);
        (Arc::new(TaskStore::new(db)), dir)
    }

    fn make_workspace(store: &TaskStore) -> Workspace {
        store
            .find_or_create_workspace("/tmp/repo")
            .expect("should create workspace")
    }

    fn make_task(store: &TaskStore, workspace_id: &str, title: &str, priority: i32) -> Task {
        store
            .create_task(CreateTaskInput {
                workspace_id: workspace_id.to_string(),
                parent_id: None,
                title: title.to_string(),
                description: format!("description for {title}"),
                created_by: "human".to_string(),
                priority,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                metadata: None,
            })
            .expect("should create task")
    }

    #[tokio::test]
    async fn test_scheduler_marks_ready_task_running() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let task = make_task(&store, &workspace.id, "task-a", 0);

        let scheduler = TaskScheduler::new(Arc::clone(&store), workspace.id.clone(), 1);
        scheduler.tick().expect("tick should succeed");

        let saved = store
            .get_task(&task.id)
            .expect("should load task")
            .expect("task should exist");
        assert_eq!(saved.status, TaskStatus::Running);
    }

    #[tokio::test]
    async fn test_scheduler_respects_max_concurrent() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        make_task(&store, &workspace.id, "task-a", 3);
        make_task(&store, &workspace.id, "task-b", 2);
        make_task(&store, &workspace.id, "task-c", 1);

        let scheduler = TaskScheduler::new(Arc::clone(&store), workspace.id.clone(), 2);
        scheduler.tick().expect("tick should succeed");

        let tasks = store
            .list_tasks(&workspace.id)
            .expect("should list workspace tasks");
        let running_count = tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Running)
            .count();
        let pending_count = tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Pending)
            .count();

        assert_eq!(running_count, 2);
        assert_eq!(pending_count, 1);
    }
}
