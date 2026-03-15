use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use crate::error::TaskError;
use crate::model::{TaskStatus, VerifyCommand};
use crate::store::TaskStore;
use crate::system_prompt::{
    CompletedDepResult, SiblingTask, SystemPromptBuilder, TaskPromptContext, WorkspaceSnapshot,
};

pub struct TaskSession {
    task_id: String,
    store: Arc<TaskStore>,
    git_root: PathBuf,
}

#[derive(Debug)]
pub enum TaskOutcome {
    Completed { summary: String },
    Failed { error: String },
    BlockedOnHuman { question_id: String },
}

impl TaskSession {
    pub fn new(task_id: String, store: Arc<TaskStore>, git_root: PathBuf) -> Self {
        Self {
            task_id,
            store,
            git_root,
        }
    }

    pub fn build_system_prompt(
        &self,
        workspace_snapshot: WorkspaceSnapshot,
        conventions: Option<String>,
        repo_map: Option<String>,
    ) -> Result<String, TaskError> {
        let task = self
            .store
            .get_task(&self.task_id)?
            .ok_or_else(|| TaskError::NotFound {
                id: self.task_id.clone(),
            })?;

        let accept_criteria = self.store.get_accept_criteria(&self.task_id)?;
        let verify_commands = match accept_criteria.as_ref() {
            Some(criteria) => self.store.get_verify_commands(criteria.id)?,
            None => Vec::<VerifyCommand>::new(),
        };

        let dep_ids = self.store.get_deps(&self.task_id)?;
        let mut dep_results = Vec::new();
        for dep_id in dep_ids {
            let Some(dep_task) = self.store.get_task(&dep_id)? else {
                continue;
            };
            if dep_task.status != TaskStatus::Completed {
                continue;
            }

            dep_results.push(CompletedDepResult {
                task_id: dep_task.id,
                title: dep_task.title,
                completed_at: dep_task.completed_at.unwrap_or(dep_task.updated_at),
                summary: dep_task
                    .result_summary
                    .unwrap_or_else(|| "No summary recorded.".to_string()),
                diff: dep_task.result_diff,
                changed_files: dep_task.result_files.unwrap_or_default(),
            });
        }

        let children = self.store.list_children(&self.task_id)?;
        let mut children_results = Vec::new();
        for child in children {
            if child.status != TaskStatus::Completed {
                continue;
            }
            children_results.push(CompletedDepResult {
                task_id: child.id,
                title: child.title,
                completed_at: child.completed_at.unwrap_or(child.updated_at),
                summary: child
                    .result_summary
                    .unwrap_or_else(|| "No summary recorded.".to_string()),
                diff: child.result_diff,
                changed_files: child.result_files.unwrap_or_default(),
            });
        }

        let mut running_siblings = Vec::new();
        for sibling in self.store.list_running_tasks(&task.workspace_id)? {
            if sibling.id == self.task_id {
                continue;
            }

            let claimed_paths = self
                .store
                .get_path_claims(&sibling.id)?
                .into_iter()
                .map(|claim| claim.path_pattern)
                .collect::<Vec<_>>();

            running_siblings.push(SiblingTask {
                task_id: sibling.id,
                title: sibling.title,
                claimed_paths,
            });
        }

        let context = TaskPromptContext {
            git_root: self.git_root.clone(),
            task,
            accept_criteria,
            verify_commands,
            dep_results,
            children_results,
            workspace_snapshot,
            running_siblings,
            conventions,
            repo_map,
        };

        Ok(SystemPromptBuilder::build(&context))
    }

    pub fn mark_started(&self, session_id: &str) -> Result<(), TaskError> {
        self.store.mark_running(&self.task_id, session_id)
    }

    pub fn apply_outcome(&self, outcome: TaskOutcome) -> Result<(), TaskError> {
        match outcome {
            TaskOutcome::Completed { summary } => {
                let diff_output = Command::new("git")
                    .arg("diff")
                    .arg("HEAD")
                    .current_dir(&self.git_root)
                    .output()?;
                if !diff_output.status.success() {
                    return Err(TaskError::Io(std::io::Error::other(
                        String::from_utf8_lossy(&diff_output.stderr).to_string(),
                    )));
                }

                let files_output = Command::new("git")
                    .arg("diff")
                    .arg("--name-only")
                    .arg("HEAD")
                    .current_dir(&self.git_root)
                    .output()?;
                if !files_output.status.success() {
                    return Err(TaskError::Io(std::io::Error::other(
                        String::from_utf8_lossy(&files_output.stderr).to_string(),
                    )));
                }

                let diff = String::from_utf8_lossy(&diff_output.stdout).to_string();
                let files = String::from_utf8_lossy(&files_output.stdout)
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| line.to_string())
                    .collect::<Vec<_>>();

                self.store
                    .set_result(&self.task_id, Some(&diff), &summary, &files, None)
            }
            TaskOutcome::Failed { error } => {
                self.store.set_error(&self.task_id, &error)?;
                self.store.increment_retry(&self.task_id)
            }
            TaskOutcome::BlockedOnHuman { question_id } => {
                let _ = question_id;
                self.store
                    .update_status(&self.task_id, TaskStatus::BlockedHuman)
            }
        }
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    pub fn git_root(&self) -> &PathBuf {
        &self.git_root
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arc_with_non_send_sync)]

    use std::process::Command;
    use std::sync::Arc;

    use chrono::Utc;
    use sunny_store::{Database, SessionStore};

    use super::{TaskOutcome, TaskSession};
    use crate::model::{CreateTaskInput, TaskStatus};
    use crate::store::TaskStore;
    use crate::system_prompt::WorkspaceSnapshot;

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

    fn init_git_repo(path: &std::path::Path) {
        let now = Utc::now().timestamp_millis().to_string();
        let status = Command::new("git")
            .arg("init")
            .current_dir(path)
            .status()
            .expect("git init should run");
        assert!(status.success());

        let status = Command::new("git")
            .arg("config")
            .arg("user.email")
            .arg("task-session@test.local")
            .current_dir(path)
            .status()
            .expect("git config user.email should run");
        assert!(status.success());

        let status = Command::new("git")
            .arg("config")
            .arg("user.name")
            .arg("Task Session Test")
            .current_dir(path)
            .status()
            .expect("git config user.name should run");
        assert!(status.success());

        std::fs::write(path.join("tracked.txt"), format!("seed-{now}\n")).expect("write tracked");
        let status = Command::new("git")
            .arg("add")
            .arg("tracked.txt")
            .current_dir(path)
            .status()
            .expect("git add should run");
        assert!(status.success());

        let status = Command::new("git")
            .arg("commit")
            .arg("-m")
            .arg("seed")
            .current_dir(path)
            .status()
            .expect("git commit should run");
        assert!(status.success());
    }

    fn make_task(store: &TaskStore, workspace_id: &str, title: &str) -> crate::model::Task {
        store
            .create_task(CreateTaskInput {
                workspace_id: workspace_id.to_string(),
                parent_id: None,
                title: title.to_string(),
                description: format!("description for {title}"),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                metadata: None,
            })
            .expect("should create task")
    }

    #[test]
    fn test_mark_started_updates_session_id() {
        let (store, dir) = make_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("should create workspace");
        let task = make_task(&store, &workspace.id, "task");
        let session = TaskSession::new(
            task.id.clone(),
            Arc::clone(&store),
            PathBuf::from("/tmp/repo"),
        );

        let db = Database::open(dir.path().join("test.db").as_path()).expect("should open db");
        let session_store = SessionStore::new(db);
        session_store
            .create_session_with_id("session-123", "/tmp/repo", None)
            .expect("session should exist before mark_started");

        session
            .mark_started("session-123")
            .expect("mark started should succeed");
        let saved = store
            .get_task(&task.id)
            .expect("should load task")
            .expect("task should exist");

        assert_eq!(saved.status, TaskStatus::Running);
        assert_eq!(saved.session_id.as_deref(), Some("session-123"));
    }

    use std::path::PathBuf;

    #[test]
    fn test_apply_outcome_completed_marks_task() {
        let (store, dir) = make_store();
        init_git_repo(dir.path());

        let workspace = store
            .find_or_create_workspace(&dir.path().to_string_lossy())
            .expect("should create workspace");
        let task = make_task(&store, &workspace.id, "task");
        let session = TaskSession::new(
            task.id.clone(),
            Arc::clone(&store),
            dir.path().to_path_buf(),
        );

        std::fs::write(dir.path().join("tracked.txt"), "changed\n").expect("should modify file");
        session
            .apply_outcome(TaskOutcome::Completed {
                summary: "done".to_string(),
            })
            .expect("apply outcome should succeed");

        let saved = store
            .get_task(&task.id)
            .expect("should load task")
            .expect("task should exist");
        assert_eq!(saved.status, TaskStatus::Completed);
        assert_eq!(saved.result_summary.as_deref(), Some("done"));
        assert!(saved
            .result_diff
            .as_deref()
            .unwrap_or_default()
            .contains("tracked.txt"));
    }

    #[test]
    fn test_apply_outcome_failed_increments_retry() {
        let (store, _dir) = make_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("should create workspace");
        let task = make_task(&store, &workspace.id, "task");
        let session = TaskSession::new(
            task.id.clone(),
            Arc::clone(&store),
            PathBuf::from("/tmp/repo"),
        );

        session
            .apply_outcome(TaskOutcome::Failed {
                error: "boom".to_string(),
            })
            .expect("apply outcome should succeed");

        let saved = store
            .get_task(&task.id)
            .expect("should load task")
            .expect("task should exist");
        assert_eq!(saved.status, TaskStatus::Failed);
        assert_eq!(saved.retry_count, 1);
        assert_eq!(saved.error.as_deref(), Some("boom"));
    }

    #[test]
    fn test_build_system_prompt_contains_task_title() {
        let (store, _dir) = make_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("should create workspace");
        let task = make_task(&store, &workspace.id, "task title");
        let session = TaskSession::new(
            task.id.clone(),
            Arc::clone(&store),
            PathBuf::from("/tmp/repo"),
        );

        let prompt = session
            .build_system_prompt(
                WorkspaceSnapshot {
                    branch: "master".to_string(),
                    status_short: "".to_string(),
                    recent_log: "abc123 test".to_string(),
                },
                None,
                None,
            )
            .expect("build system prompt should succeed");

        assert!(prompt.contains("**Title**: task title"));
    }

    #[test]
    fn test_build_system_prompt_includes_children_results() {
        let (store, _dir) = make_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("should create workspace");
        let parent = make_task(&store, &workspace.id, "parent task");
        let child = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: Some(parent.id.clone()),
                title: "child task".to_string(),
                description: "description for child task".to_string(),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                metadata: None,
            })
            .expect("should create child task");
        store
            .set_result(&child.id, None, "child done", &[], None)
            .expect("should complete child task");
        let session = TaskSession::new(
            parent.id.clone(),
            Arc::clone(&store),
            PathBuf::from("/tmp/repo"),
        );

        let prompt = session
            .build_system_prompt(
                WorkspaceSnapshot {
                    branch: "master".to_string(),
                    status_short: "".to_string(),
                    recent_log: "abc123 test".to_string(),
                },
                None,
                None,
            )
            .expect("build system prompt should succeed");

        assert!(prompt.contains("# Child Task Results"));
        assert!(prompt.contains("child done"));
    }
}
