use std::str::FromStr;

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;
use uuid::Uuid;

use crate::error::TaskError;
use crate::model::{
    AcceptCriteria, CreateAcceptCriteriaInput, CreateTaskInput, HumanQuestion, Task, TaskPathClaim,
    TaskStatus, VerifyCommand, Workspace,
};

pub struct TaskStore {
    db: sunny_store::Database,
}

impl TaskStore {
    pub fn new(db: sunny_store::Database) -> Self {
        Self { db }
    }

    pub fn open_default() -> Result<Self, TaskError> {
        Ok(Self::new(sunny_store::Database::open_default()?))
    }

    pub fn find_or_create_workspace(&self, git_root: &str) -> Result<Workspace, TaskError> {
        if let Some(existing) = self
            .db
            .connection()
            .query_row(
                "SELECT id, git_root, name, created_at FROM workspaces WHERE git_root = ?1",
                rusqlite::params![git_root],
                row_to_workspace,
            )
            .optional()?
        {
            return Ok(existing);
        }

        let workspace = Workspace {
            id: Uuid::new_v4().to_string(),
            git_root: git_root.to_string(),
            name: None,
            created_at: Utc::now(),
        };

        self.db.connection().execute(
            "INSERT INTO workspaces (id, git_root, name, created_at) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                workspace.id,
                workspace.git_root,
                workspace.name,
                workspace.created_at.to_rfc3339()
            ],
        )?;

        Ok(workspace)
    }

    pub fn get_workspace(&self, id: &str) -> Result<Option<Workspace>, TaskError> {
        let workspace = self
            .db
            .connection()
            .query_row(
                "SELECT id, git_root, name, created_at FROM workspaces WHERE id = ?1",
                rusqlite::params![id],
                row_to_workspace,
            )
            .optional()?;
        Ok(workspace)
    }

    pub fn create_task(&self, input: CreateTaskInput) -> Result<Task, TaskError> {
        let task_id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        let metadata_json = input
            .metadata
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;

        self.db.connection().execute(
            "INSERT INTO tasks (
                id, workspace_id, parent_id, title, description, status,
                session_id, created_by, priority, created_at, updated_at,
                started_at, completed_at, result_diff, result_summary, result_files,
                result_verify, error, retry_count, max_retries, metadata, root_session_id
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6,
                NULL, ?7, ?8, ?9, ?10,
                NULL, NULL, NULL, NULL, NULL,
                NULL, NULL, 0, ?11, ?12, ?13
            )",
            rusqlite::params![
                task_id,
                input.workspace_id,
                input.parent_id,
                input.title,
                input.description,
                TaskStatus::Pending.to_string(),
                input.created_by,
                input.priority,
                now_str,
                now_str,
                input.max_retries,
                metadata_json,
                input.root_session_id,
            ],
        )?;

        for dep_id in &input.dep_ids {
            self.add_dep(&task_id, dep_id)?;
        }

        if let Some(criteria) = input.accept_criteria {
            self.set_accept_criteria(criteria, &task_id)?;
        }

        self.get_task(&task_id)?
            .ok_or(TaskError::NotFound { id: task_id })
    }

    pub fn get_task(&self, id: &str) -> Result<Option<Task>, TaskError> {
        let task = self
            .db
            .connection()
            .query_row(
                "SELECT
                    id, workspace_id, parent_id, title, description, status,
                    session_id, created_by, priority, created_at, updated_at,
                    started_at, completed_at, result_diff, result_summary, result_files,
                    result_verify, error, retry_count, max_retries, metadata, root_session_id
                 FROM tasks WHERE id = ?1",
                rusqlite::params![id],
                row_to_task,
            )
            .optional()?;
        Ok(task)
    }

    pub fn update_status(&self, id: &str, status: TaskStatus) -> Result<(), TaskError> {
        let now = Utc::now().to_rfc3339();
        self.db.connection().execute(
            "UPDATE tasks
             SET status = ?1,
                 updated_at = ?2,
                 completed_at = CASE
                    WHEN ?1 IN ('completed', 'failed', 'cancelled') THEN ?2
                    ELSE completed_at
                 END
             WHERE id = ?3",
            rusqlite::params![status.to_string(), now, id],
        )?;
        Ok(())
    }

    pub fn mark_running(&self, id: &str, session_id: &str) -> Result<(), TaskError> {
        let now = Utc::now().to_rfc3339();
        self.db.connection().execute(
            "UPDATE tasks
             SET status = 'running', session_id = ?1, started_at = ?2, updated_at = ?2
             WHERE id = ?3",
            rusqlite::params![session_id, now, id],
        )?;
        Ok(())
    }

    pub fn set_result(
        &self,
        id: &str,
        diff: Option<&str>,
        summary: &str,
        files: &[String],
        verify_output: Option<&str>,
    ) -> Result<(), TaskError> {
        let now = Utc::now().to_rfc3339();
        let files_json = serde_json::to_string(files)?;
        self.db.connection().execute(
            "UPDATE tasks
             SET status = 'completed',
                 result_diff = ?1,
                 result_summary = ?2,
                 result_files = ?3,
                 result_verify = ?4,
                 completed_at = ?5,
                 updated_at = ?5,
                 error = NULL
             WHERE id = ?6",
            rusqlite::params![diff, summary, files_json, verify_output, now, id],
        )?;
        Ok(())
    }

    pub fn set_error(&self, id: &str, error: &str) -> Result<(), TaskError> {
        let now = Utc::now().to_rfc3339();
        self.db.connection().execute(
            "UPDATE tasks
             SET status = 'failed', error = ?1, updated_at = ?2, completed_at = ?2
             WHERE id = ?3",
            rusqlite::params![error, now, id],
        )?;
        Ok(())
    }

    pub fn increment_retry(&self, id: &str) -> Result<(), TaskError> {
        self.db.connection().execute(
            "UPDATE tasks
             SET retry_count = retry_count + 1, updated_at = ?1
             WHERE id = ?2",
            rusqlite::params![Utc::now().to_rfc3339(), id],
        )?;
        Ok(())
    }

    pub fn list_tasks(&self, workspace_id: &str) -> Result<Vec<Task>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT
                id, workspace_id, parent_id, title, description, status,
                session_id, created_by, priority, created_at, updated_at,
                started_at, completed_at, result_diff, result_summary, result_files,
                result_verify, error, retry_count, max_retries, metadata, root_session_id
             FROM tasks
             WHERE workspace_id = ?1
             ORDER BY priority DESC, created_at ASC",
        )?;

        let rows = stmt.query_map(rusqlite::params![workspace_id], row_to_task)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    pub fn list_ready_tasks(
        &self,
        workspace_id: &str,
        root_session_id: &str,
        limit: usize,
    ) -> Result<Vec<Task>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT
                id, workspace_id, parent_id, title, description, status,
                session_id, created_by, priority, created_at, updated_at,
                started_at, completed_at, result_diff, result_summary, result_files,
                result_verify, error, retry_count, max_retries, metadata, root_session_id
             FROM tasks
             WHERE workspace_id = ?1
               AND (root_session_id = ?2 OR root_session_id = '')
               AND status = 'pending'
               AND NOT EXISTS (
                 SELECT 1 FROM task_deps td
                 JOIN tasks dep ON td.depends_on = dep.id
                 WHERE td.task_id = tasks.id
                   AND dep.status != 'completed'
               )
             ORDER BY priority DESC, created_at ASC
             LIMIT ?3",
        )?;

        let rows = stmt.query_map(
            rusqlite::params![workspace_id, root_session_id, limit as i64],
            row_to_task,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    pub fn list_running_tasks(&self, workspace_id: &str) -> Result<Vec<Task>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT
                id, workspace_id, parent_id, title, description, status,
                session_id, created_by, priority, created_at, updated_at,
                started_at, completed_at, result_diff, result_summary, result_files,
                result_verify, error, retry_count, max_retries, metadata, root_session_id
             FROM tasks
             WHERE workspace_id = ?1 AND status = 'running'
             ORDER BY started_at ASC",
        )?;

        let rows = stmt.query_map(rusqlite::params![workspace_id], row_to_task)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    pub fn list_children(&self, parent_id: &str) -> Result<Vec<Task>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT
                id, workspace_id, parent_id, title, description, status,
                session_id, created_by, priority, created_at, updated_at,
                started_at, completed_at, result_diff, result_summary, result_files,
                result_verify, error, retry_count, max_retries, metadata, root_session_id
             FROM tasks
             WHERE parent_id = ?1
             ORDER BY created_at ASC",
        )?;

        let rows = stmt.query_map(rusqlite::params![parent_id], row_to_task)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    pub fn all_children_terminal(&self, parent_id: &str) -> Result<bool, TaskError> {
        let child_count: i64 = self.db.connection().query_row(
            "SELECT COUNT(*) FROM tasks WHERE parent_id = ?1",
            rusqlite::params![parent_id],
            |row| row.get(0),
        )?;

        if child_count == 0 {
            return Ok(false);
        }

        let non_terminal_count: i64 = self.db.connection().query_row(
            "SELECT COUNT(*) FROM tasks WHERE parent_id = ?1 AND status NOT IN ('completed', 'failed', 'cancelled')",
            rusqlite::params![parent_id],
            |row| row.get(0),
        )?;

        Ok(non_terminal_count == 0)
    }

    pub fn list_tasks_by_status(
        &self,
        workspace_id: &str,
        status: TaskStatus,
    ) -> Result<Vec<Task>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT
                id, workspace_id, parent_id, title, description, status,
                session_id, created_by, priority, created_at, updated_at,
                started_at, completed_at, result_diff, result_summary, result_files,
                result_verify, error, retry_count, max_retries, metadata, root_session_id
             FROM tasks
             WHERE workspace_id = ?1 AND status = ?2
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![workspace_id, status.to_string()],
            row_to_task,
        )?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    pub fn update_metadata(&self, id: &str, metadata: serde_json::Value) -> Result<(), TaskError> {
        let now = Utc::now().to_rfc3339();
        let json = serde_json::to_string(&metadata).map_err(TaskError::Serialization)?;
        self.db.connection().execute(
            "UPDATE tasks SET metadata = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![json, now, id],
        )?;
        Ok(())
    }

    pub fn add_dep(&self, task_id: &str, depends_on: &str) -> Result<(), TaskError> {
        if self.has_cycle(task_id, depends_on)? {
            return Err(TaskError::DependencyCycle);
        }

        self.db.connection().execute(
            "INSERT OR IGNORE INTO task_deps (task_id, depends_on) VALUES (?1, ?2)",
            rusqlite::params![task_id, depends_on],
        )?;
        Ok(())
    }

    pub fn get_deps(&self, task_id: &str) -> Result<Vec<String>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT depends_on FROM task_deps WHERE task_id = ?1 ORDER BY depends_on ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![task_id], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    pub fn has_cycle(&self, task_id: &str, new_dep: &str) -> Result<bool, TaskError> {
        if task_id == new_dep {
            return Ok(true);
        }

        let has_cycle: i64 = self.db.connection().query_row(
            "WITH RECURSIVE reachable(id) AS (
                SELECT depends_on FROM task_deps WHERE task_id = ?1
                UNION
                SELECT td.depends_on
                FROM task_deps td
                JOIN reachable r ON td.task_id = r.id
            )
            SELECT EXISTS(SELECT 1 FROM reachable WHERE id = ?2)",
            rusqlite::params![new_dep, task_id],
            |row| row.get(0),
        )?;

        Ok(has_cycle == 1)
    }

    pub fn set_accept_criteria(
        &self,
        input: CreateAcceptCriteriaInput,
        task_id: &str,
    ) -> Result<AcceptCriteria, TaskError> {
        self.db.connection().execute(
            "DELETE FROM accept_criteria WHERE task_id = ?1",
            rusqlite::params![task_id],
        )?;

        self.db.connection().execute(
            "INSERT INTO accept_criteria (task_id, description, requires_human_approval)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![
                task_id,
                input.description,
                if input.requires_human_approval { 1 } else { 0 }
            ],
        )?;

        let criteria_id = self.db.connection().last_insert_rowid();

        for (seq, cmd) in input.verify_commands.iter().enumerate() {
            self.db.connection().execute(
                "INSERT INTO verify_commands (
                    criteria_id, command, expected_exit_code, timeout_secs, seq
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![
                    criteria_id,
                    cmd.command,
                    cmd.expected_exit_code,
                    i64::from(cmd.timeout_secs),
                    seq as i32,
                ],
            )?;
        }

        Ok(AcceptCriteria {
            id: criteria_id,
            task_id: task_id.to_string(),
            description: input.description,
            requires_human_approval: input.requires_human_approval,
        })
    }

    pub fn get_accept_criteria(&self, task_id: &str) -> Result<Option<AcceptCriteria>, TaskError> {
        let criteria = self
            .db
            .connection()
            .query_row(
                "SELECT id, task_id, description, requires_human_approval
                 FROM accept_criteria WHERE task_id = ?1",
                rusqlite::params![task_id],
                |row| {
                    Ok(AcceptCriteria {
                        id: row.get(0)?,
                        task_id: row.get(1)?,
                        description: row.get(2)?,
                        requires_human_approval: row.get::<_, i64>(3)? != 0,
                    })
                },
            )
            .optional()?;
        Ok(criteria)
    }

    pub fn get_verify_commands(&self, criteria_id: i64) -> Result<Vec<VerifyCommand>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT id, criteria_id, command, expected_exit_code, timeout_secs, seq
             FROM verify_commands WHERE criteria_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![criteria_id], |row| {
            Ok(VerifyCommand {
                id: row.get(0)?,
                criteria_id: row.get(1)?,
                command: row.get(2)?,
                expected_exit_code: row.get(3)?,
                timeout_secs: row.get::<_, i64>(4)? as u32,
                seq: row.get(5)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    pub fn create_question(
        &self,
        task_id: &str,
        question: &str,
        context: Option<&str>,
        options: Option<&[String]>,
    ) -> Result<HumanQuestion, TaskError> {
        let q = HumanQuestion {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.to_string(),
            question: question.to_string(),
            context: context.map(str::to_string),
            options: options.map(|vals| vals.to_vec()),
            answer: None,
            asked_at: Utc::now(),
            answered_at: None,
        };

        let options_json = q.options.as_ref().map(serde_json::to_string).transpose()?;
        self.db.connection().execute(
            "INSERT INTO human_questions (
                id, task_id, question, context, options, answer, asked_at, answered_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, NULL)",
            rusqlite::params![
                q.id,
                q.task_id,
                q.question,
                q.context,
                options_json,
                q.asked_at.to_rfc3339()
            ],
        )?;

        Ok(q)
    }

    pub fn answer_question(&self, question_id: &str, answer: &str) -> Result<(), TaskError> {
        self.db.connection().execute(
            "UPDATE human_questions SET answer = ?1, answered_at = ?2 WHERE id = ?3",
            rusqlite::params![answer, Utc::now().to_rfc3339(), question_id],
        )?;
        Ok(())
    }

    pub fn pending_questions(&self) -> Result<Vec<HumanQuestion>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT id, task_id, question, context, options, answer, asked_at, answered_at
             FROM human_questions
             WHERE answered_at IS NULL
             ORDER BY asked_at ASC",
        )?;
        let rows = stmt.query_map([], row_to_human_question)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    pub fn task_questions(&self, task_id: &str) -> Result<Vec<HumanQuestion>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT id, task_id, question, context, options, answer, asked_at, answered_at
             FROM human_questions
             WHERE task_id = ?1
             ORDER BY asked_at ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![task_id], row_to_human_question)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    pub fn all_questions_answered(&self, task_id: &str) -> Result<bool, TaskError> {
        let pending: i64 = self.db.connection().query_row(
            "SELECT COUNT(*) FROM human_questions WHERE task_id = ?1 AND answered_at IS NULL",
            rusqlite::params![task_id],
            |row| row.get(0),
        )?;
        Ok(pending == 0)
    }

    pub fn add_path_claim(
        &self,
        task_id: &str,
        path_pattern: &str,
        claim_type: &str,
    ) -> Result<(), TaskError> {
        self.db.connection().execute(
            "INSERT OR REPLACE INTO task_path_claims (task_id, path_pattern, claim_type)
             VALUES (?1, ?2, ?3)",
            rusqlite::params![task_id, path_pattern, claim_type],
        )?;
        Ok(())
    }

    pub fn get_path_claims(&self, task_id: &str) -> Result<Vec<TaskPathClaim>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT task_id, path_pattern, claim_type
             FROM task_path_claims WHERE task_id = ?1 ORDER BY path_pattern ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![task_id], |row| {
            Ok(TaskPathClaim {
                task_id: row.get(0)?,
                path_pattern: row.get(1)?,
                claim_type: row.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    pub fn find_conflicting_claims(
        &self,
        task_id: &str,
        path_pattern: &str,
    ) -> Result<Vec<(String, TaskPathClaim)>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT tpc.task_id, tpc.path_pattern, tpc.claim_type
             FROM task_path_claims tpc
             JOIN tasks t ON t.id = tpc.task_id
             WHERE tpc.task_id != ?1
               AND t.status = 'running'
               AND (
                    tpc.path_pattern = ?2
                    OR tpc.path_pattern LIKE (?2 || '%')
                    OR ?2 LIKE (tpc.path_pattern || '%')
               )
             ORDER BY tpc.task_id ASC, tpc.path_pattern ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![task_id, path_pattern], |row| {
            let claim = TaskPathClaim {
                task_id: row.get(0)?,
                path_pattern: row.get(1)?,
                claim_type: row.get(2)?,
            };
            Ok((claim.task_id.clone(), claim))
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }
}

fn row_to_workspace(row: &rusqlite::Row<'_>) -> rusqlite::Result<Workspace> {
    let created_at = parse_required_datetime(3, row.get::<_, String>(3)?)?;
    Ok(Workspace {
        id: row.get(0)?,
        git_root: row.get(1)?,
        name: row.get(2)?,
        created_at,
    })
}

fn row_to_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<Task> {
    let status = TaskStatus::from_str(&row.get::<_, String>(5)?).map_err(|e| {
        rusqlite::Error::InvalidColumnType(
            5,
            format!("invalid status: {e}"),
            rusqlite::types::Type::Text,
        )
    })?;

    let result_files = parse_optional_json_vec(15, row.get::<_, Option<String>>(15)?)?;
    let metadata = parse_optional_json_value(20, row.get::<_, Option<String>>(20)?)?;

    Ok(Task {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        root_session_id: row.get(21)?,
        parent_id: row.get(2)?,
        title: row.get(3)?,
        description: row.get(4)?,
        status,
        session_id: row.get(6)?,
        created_by: row.get(7)?,
        priority: row.get(8)?,
        created_at: parse_required_datetime(9, row.get::<_, String>(9)?)?,
        updated_at: parse_required_datetime(10, row.get::<_, String>(10)?)?,
        started_at: parse_optional_datetime(11, row.get::<_, Option<String>>(11)?)?,
        completed_at: parse_optional_datetime(12, row.get::<_, Option<String>>(12)?)?,
        result_diff: row.get(13)?,
        result_summary: row.get(14)?,
        result_files,
        result_verify: row.get(16)?,
        error: row.get(17)?,
        retry_count: row.get(18)?,
        max_retries: row.get(19)?,
        metadata,
    })
}

fn row_to_human_question(row: &rusqlite::Row<'_>) -> rusqlite::Result<HumanQuestion> {
    Ok(HumanQuestion {
        id: row.get(0)?,
        task_id: row.get(1)?,
        question: row.get(2)?,
        context: row.get(3)?,
        options: parse_optional_json_vec(4, row.get::<_, Option<String>>(4)?)?,
        answer: row.get(5)?,
        asked_at: parse_required_datetime(6, row.get::<_, String>(6)?)?,
        answered_at: parse_optional_datetime(7, row.get::<_, Option<String>>(7)?)?,
    })
}

fn parse_required_datetime(col_idx: usize, value: String) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::InvalidColumnType(
                col_idx,
                format!("invalid datetime: {e}"),
                rusqlite::types::Type::Text,
            )
        })
}

fn parse_optional_datetime(
    col_idx: usize,
    value: Option<String>,
) -> rusqlite::Result<Option<DateTime<Utc>>> {
    value
        .map(|v| parse_required_datetime(col_idx, v))
        .transpose()
}

fn parse_optional_json_vec(
    col_idx: usize,
    value: Option<String>,
) -> rusqlite::Result<Option<Vec<String>>> {
    value
        .map(|raw| {
            serde_json::from_str::<Vec<String>>(&raw).map_err(|e| {
                rusqlite::Error::InvalidColumnType(
                    col_idx,
                    format!("invalid json array: {e}"),
                    rusqlite::types::Type::Text,
                )
            })
        })
        .transpose()
}

fn parse_optional_json_value(
    col_idx: usize,
    value: Option<String>,
) -> rusqlite::Result<Option<serde_json::Value>> {
    value
        .map(|raw| {
            serde_json::from_str::<serde_json::Value>(&raw).map_err(|e| {
                rusqlite::Error::InvalidColumnType(
                    col_idx,
                    format!("invalid json value: {e}"),
                    rusqlite::types::Type::Text,
                )
            })
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
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
                    id              TEXT PRIMARY KEY,
                    workspace_id    TEXT NOT NULL REFERENCES workspaces(id),
                    root_session_id TEXT NOT NULL DEFAULT '',
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

    fn make_store() -> (TaskStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let db = Database::open(dir.path().join("test.db").as_path()).expect("should open db");
        ensure_task_tables(&db);
        (TaskStore::new(db), dir)
    }

    fn make_workspace(store: &TaskStore) -> Workspace {
        store
            .find_or_create_workspace("/tmp/repo")
            .expect("should create workspace")
    }

    fn make_task(store: &TaskStore, workspace_id: &str, title: &str) -> Task {
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
                root_session_id: String::new(),
                metadata: None,
            })
            .expect("should create task")
    }

    fn make_session(store: &TaskStore, id: &str) {
        let now = Utc::now().to_rfc3339();
        store
            .db
            .connection()
            .execute(
                "INSERT INTO sessions (id, title, model, working_dir, token_count, created_at, updated_at)
                 VALUES (?1, NULL, NULL, ?2, 0, ?3, ?3)",
                rusqlite::params![id, "/tmp/repo", now],
            )
            .expect("should insert session");
    }

    #[test]
    fn test_find_or_create_workspace_creates_and_returns() {
        let (store, _dir) = make_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo-a")
            .expect("should create workspace");

        assert!(!workspace.id.is_empty());
        assert_eq!(workspace.git_root, "/tmp/repo-a");
    }

    #[test]
    fn test_find_or_create_workspace_idempotent() {
        let (store, _dir) = make_store();
        let first = store
            .find_or_create_workspace("/tmp/repo-a")
            .expect("should create workspace");
        let second = store
            .find_or_create_workspace("/tmp/repo-a")
            .expect("should return existing workspace");

        assert_eq!(first.id, second.id);
    }

    #[test]
    fn test_create_task_returns_valid_task() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let task = make_task(&store, &workspace.id, "task-a");

        assert_eq!(task.workspace_id, workspace.id);
        assert_eq!(task.title, "task-a");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.retry_count, 0);
    }

    #[test]
    fn test_task_has_root_session_id() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let task = make_task(&store, &workspace.id, "task-a");

        assert_eq!(task.root_session_id, "");
    }

    #[test]
    fn test_create_task_stores_root_session_id() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let task = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: None,
                title: "task-with-root-session".to_string(),
                description: "check root_session_id persistence".to_string(),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: "chat-session-123".to_string(),
                metadata: None,
            })
            .expect("should create task");

        assert_eq!(task.root_session_id, "chat-session-123");

        let loaded = store
            .get_task(&task.id)
            .expect("should load task")
            .expect("task should exist");
        assert_eq!(loaded.root_session_id, "chat-session-123");
    }

    #[test]
    fn test_list_ready_tasks_returns_only_pending_with_completed_deps() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let dep = make_task(&store, &workspace.id, "dep");
        store
            .update_status(&dep.id, TaskStatus::Completed)
            .expect("should mark dep completed");

        let ready = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: None,
                title: "ready".to_string(),
                description: "ready with completed dep".to_string(),
                created_by: "human".to_string(),
                priority: 10,
                max_retries: 3,
                dep_ids: vec![dep.id.clone()],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: String::new(),
                metadata: None,
            })
            .expect("should create task");

        let running = make_task(&store, &workspace.id, "running");
        make_session(&store, "session-1");
        store
            .mark_running(&running.id, "session-1")
            .expect("should mark running");

        let ready_tasks = store
            .list_ready_tasks(&workspace.id, "", 10)
            .expect("should list ready tasks");
        assert_eq!(ready_tasks.len(), 1);
        assert_eq!(ready_tasks[0].id, ready.id);
    }

    #[test]
    fn test_list_ready_tasks_excludes_blocked_tasks() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let dep = make_task(&store, &workspace.id, "dep");

        let blocked = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: None,
                title: "blocked".to_string(),
                description: "blocked task".to_string(),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![dep.id.clone()],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: String::new(),
                metadata: None,
            })
            .expect("should create blocked task");

        let ready_tasks = store
            .list_ready_tasks(&workspace.id, "", 10)
            .expect("should list ready tasks");
        assert!(ready_tasks.iter().all(|task| task.id != blocked.id));
    }

    #[test]
    fn test_add_dep_prevents_ready_when_dep_pending() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let dep = make_task(&store, &workspace.id, "dep");
        let task = make_task(&store, &workspace.id, "task");

        store
            .add_dep(&task.id, &dep.id)
            .expect("should add dependency");

        let ready_tasks = store
            .list_ready_tasks(&workspace.id, "", 10)
            .expect("should list ready tasks");
        assert!(ready_tasks.iter().all(|t| t.id != task.id));
    }

    #[test]
    fn test_create_question_and_answer_roundtrip() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let task = make_task(&store, &workspace.id, "task");

        let options = vec!["yes".to_string(), "no".to_string()];
        let question = store
            .create_question(
                &task.id,
                "Proceed?",
                Some("need confirmation"),
                Some(&options),
            )
            .expect("should create question");

        store
            .answer_question(&question.id, "yes")
            .expect("should answer question");

        let task_questions = store
            .task_questions(&task.id)
            .expect("should list task questions");
        assert_eq!(task_questions.len(), 1);
        assert_eq!(task_questions[0].answer.as_deref(), Some("yes"));
        assert_eq!(task_questions[0].options.as_ref(), Some(&options));
    }

    #[test]
    fn test_all_questions_answered_false_when_pending() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let task = make_task(&store, &workspace.id, "task");

        store
            .create_question(&task.id, "Question?", None, None)
            .expect("should create question");

        let answered = store
            .all_questions_answered(&task.id)
            .expect("should evaluate question state");
        assert!(!answered);
    }

    #[test]
    fn test_all_questions_answered_true_when_all_answered() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let task = make_task(&store, &workspace.id, "task");

        let q1 = store
            .create_question(&task.id, "Q1?", None, None)
            .expect("should create first question");
        let q2 = store
            .create_question(&task.id, "Q2?", None, None)
            .expect("should create second question");
        store
            .answer_question(&q1.id, "a1")
            .expect("should answer first question");
        store
            .answer_question(&q2.id, "a2")
            .expect("should answer second question");

        let answered = store
            .all_questions_answered(&task.id)
            .expect("should evaluate question state");
        assert!(answered);
    }

    #[test]
    fn test_has_cycle_detects_direct_cycle() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let task_a = make_task(&store, &workspace.id, "task-a");
        let task_b = make_task(&store, &workspace.id, "task-b");

        store
            .add_dep(&task_a.id, &task_b.id)
            .expect("should add dependency");

        let cycle = store
            .has_cycle(&task_b.id, &task_a.id)
            .expect("should evaluate cycle");
        assert!(cycle);
    }

    #[test]
    fn test_set_result_stores_diff_and_marks_completed() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let task = make_task(&store, &workspace.id, "task");
        let files = vec!["crates/sunny-tasks/src/store.rs".to_string()];

        store
            .set_result(
                &task.id,
                Some("diff --git a/x b/x"),
                "done",
                &files,
                Some("verify ok"),
            )
            .expect("should set result");

        let saved = store
            .get_task(&task.id)
            .expect("should get task")
            .expect("task should exist");
        assert_eq!(saved.status, TaskStatus::Completed);
        assert_eq!(saved.result_diff.as_deref(), Some("diff --git a/x b/x"));
        assert_eq!(saved.result_summary.as_deref(), Some("done"));
        assert_eq!(saved.result_files.as_ref(), Some(&files));
        assert_eq!(saved.result_verify.as_deref(), Some("verify ok"));
        assert!(saved.completed_at.is_some());
    }

    #[test]
    fn test_add_path_claim_and_find_conflicts() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let task_a = make_task(&store, &workspace.id, "task-a");
        let task_b = make_task(&store, &workspace.id, "task-b");

        make_session(&store, "session-a");
        store
            .mark_running(&task_a.id, "session-a")
            .expect("should mark running");
        store
            .add_path_claim(&task_a.id, "/repo/src", "write")
            .expect("should add path claim");

        let conflicts = store
            .find_conflicting_claims(&task_b.id, "/repo/src/main.rs")
            .expect("should find conflicts");
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].0, task_a.id);
        assert_eq!(conflicts[0].1.claim_type, "write");
    }

    #[test]
    fn test_list_children_returns_child_tasks() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let parent = make_task(&store, &workspace.id, "parent");
        let child1 = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: Some(parent.id.clone()),
                title: "child1".to_string(),
                description: "first child".to_string(),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: String::new(),
                metadata: None,
            })
            .expect("should create child1");
        let child2 = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: Some(parent.id.clone()),
                title: "child2".to_string(),
                description: "second child".to_string(),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: String::new(),
                metadata: None,
            })
            .expect("should create child2");

        let children = store
            .list_children(&parent.id)
            .expect("should list children");
        assert_eq!(children.len(), 2);
        assert!(children.iter().any(|c| c.id == child1.id));
        assert!(children.iter().any(|c| c.id == child2.id));
    }

    #[test]
    fn test_list_children_empty_for_no_children() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let parent = make_task(&store, &workspace.id, "parent");

        let children = store
            .list_children(&parent.id)
            .expect("should list children");
        assert_eq!(children.len(), 0);
    }

    #[test]
    fn test_list_children_excludes_non_children() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let parent = make_task(&store, &workspace.id, "parent");
        let child = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: Some(parent.id.clone()),
                title: "child".to_string(),
                description: "child task".to_string(),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: String::new(),
                metadata: None,
            })
            .expect("should create child");
        let sibling = make_task(&store, &workspace.id, "sibling");

        let children = store
            .list_children(&parent.id)
            .expect("should list children");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id, child.id);
        assert!(!children.iter().any(|c| c.id == sibling.id));
    }

    #[test]
    fn test_all_children_terminal_true_when_all_complete() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let parent = make_task(&store, &workspace.id, "parent");
        let child1 = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: Some(parent.id.clone()),
                title: "child1".to_string(),
                description: "first child".to_string(),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: String::new(),
                metadata: None,
            })
            .expect("should create child1");
        let child2 = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: Some(parent.id.clone()),
                title: "child2".to_string(),
                description: "second child".to_string(),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: String::new(),
                metadata: None,
            })
            .expect("should create child2");

        store
            .update_status(&child1.id, TaskStatus::Completed)
            .expect("should mark child1 completed");
        store
            .update_status(&child2.id, TaskStatus::Completed)
            .expect("should mark child2 completed");

        let all_terminal = store
            .all_children_terminal(&parent.id)
            .expect("should evaluate terminal state");
        assert!(all_terminal);
    }

    #[test]
    fn test_all_children_terminal_false_when_child_pending() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let parent = make_task(&store, &workspace.id, "parent");
        let child1 = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: Some(parent.id.clone()),
                title: "child1".to_string(),
                description: "first child".to_string(),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: String::new(),
                metadata: None,
            })
            .expect("should create child1");
        let _child2 = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: Some(parent.id.clone()),
                title: "child2".to_string(),
                description: "second child".to_string(),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: String::new(),
                metadata: None,
            })
            .expect("should create child2");

        store
            .update_status(&child1.id, TaskStatus::Completed)
            .expect("should mark child1 completed");

        let all_terminal = store
            .all_children_terminal(&parent.id)
            .expect("should evaluate terminal state");
        assert!(!all_terminal);
    }

    #[test]
    fn test_all_children_terminal_false_when_child_suspended() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let parent = make_task(&store, &workspace.id, "parent");
        let child = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: Some(parent.id.clone()),
                title: "child".to_string(),
                description: "child task".to_string(),
                created_by: "human".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: String::new(),
                metadata: None,
            })
            .expect("should create child");

        store
            .update_status(&child.id, TaskStatus::Suspended)
            .expect("should mark child suspended");

        let all_terminal = store
            .all_children_terminal(&parent.id)
            .expect("should evaluate terminal state");
        assert!(!all_terminal);
    }

    #[test]
    fn test_all_children_terminal_false_when_no_children() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let parent = make_task(&store, &workspace.id, "parent");

        let all_terminal = store
            .all_children_terminal(&parent.id)
            .expect("should evaluate terminal state");
        assert!(!all_terminal);
    }

    #[test]
    fn test_update_status_suspended_does_not_set_completed_at() {
        let (store, _dir) = make_store();
        let workspace = make_workspace(&store);
        let task = make_task(&store, &workspace.id, "test");

        // Update to suspended
        store
            .update_status(&task.id, TaskStatus::Suspended)
            .expect("should update to suspended");

        let updated = store
            .get_task(&task.id)
            .expect("should get task")
            .expect("task should exist");

        // Verify status is suspended
        assert_eq!(updated.status, TaskStatus::Suspended);
        // Verify completed_at is NOT set
        assert_eq!(updated.completed_at, None);
    }

    #[test]
    fn test_list_tasks_by_status_returns_only_matching() {
        let (store, _dir) = make_store();
        let ws = make_workspace(&store);
        let t1 = make_task(&store, &ws.id, "pending-task");
        let t2 = make_task(&store, &ws.id, "running-task");
        store
            .update_status(&t2.id, TaskStatus::Running)
            .expect("should update");
        let pending = store
            .list_tasks_by_status(&ws.id, TaskStatus::Pending)
            .expect("should list");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, t1.id);
        let running = store
            .list_tasks_by_status(&ws.id, TaskStatus::Running)
            .expect("should list");
        assert_eq!(running.len(), 1);
        assert_eq!(running[0].id, t2.id);
    }

    #[test]
    fn test_list_tasks_by_status_suspended_returns_suspended() {
        let (store, _dir) = make_store();
        let ws = make_workspace(&store);
        let t = make_task(&store, &ws.id, "suspended-task");
        store
            .update_status(&t.id, TaskStatus::Suspended)
            .expect("should update");
        let suspended = store
            .list_tasks_by_status(&ws.id, TaskStatus::Suspended)
            .expect("should list");
        assert_eq!(suspended.len(), 1);
        assert_eq!(suspended[0].id, t.id);
    }

    #[test]
    fn test_update_metadata_stores_and_retrieves() {
        let (store, _dir) = make_store();
        let ws = make_workspace(&store);
        let t = make_task(&store, &ws.id, "meta-task");
        let meta = serde_json::json!({"suspension_count": 3});
        store
            .update_metadata(&t.id, meta.clone())
            .expect("should update metadata");
        let saved = store
            .get_task(&t.id)
            .expect("should load")
            .expect("should exist");
        let count = saved
            .metadata
            .as_ref()
            .and_then(|m| m.get("suspension_count"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        assert_eq!(count, 3);
    }
}
