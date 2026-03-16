use std::str::FromStr;

use chrono::{DateTime, Utc};
use rusqlite::{params, OptionalExtension, Transaction};
use uuid::Uuid;

use crate::error::PlanError;
use crate::events::{PlanEvent, StoredEvent};
use crate::model::{
    Constraint, ConstraintType, Decision, DecisionAuthor, DecisionType, Goal, GoalPriority,
    GoalStatus, Plan, PlanMode, PlanStatus,
};
use crate::schema::ensure_plan_schema;

pub struct PlanStore {
    db: sunny_store::Database,
}

#[derive(Debug, Clone)]
pub struct PlanState {
    pub plan: Plan,
    pub task_ids: Vec<String>,
    pub decisions: Vec<Decision>,
    pub constraints: Vec<Constraint>,
    pub goals: Vec<Goal>,
    pub events: Vec<StoredEvent>,
}

impl PlanStore {
    pub fn new(db: sunny_store::Database) -> Self {
        ensure_plan_schema(&db);
        Self { db }
    }

    pub fn open_default() -> Result<Self, PlanError> {
        Ok(Self::new(sunny_store::Database::open_default().map_err(
            |err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)),
        )?))
    }

    pub fn create_plan(
        &self,
        workspace_id: &str,
        name: &str,
        description: Option<&str>,
        mode: PlanMode,
        root_session_id: Option<&str>,
    ) -> Result<Plan, PlanError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();

        let tx = self.db.connection().unchecked_transaction()?;
        tx.execute(
            "INSERT INTO plans (
                id, workspace_id, name, description, mode, status,
                root_session_id, created_at, updated_at, completed_at, metadata
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, NULL)",
            params![
                id,
                workspace_id,
                name,
                description,
                mode.to_string(),
                PlanStatus::Draft.to_string(),
                root_session_id,
                now_str,
                now_str,
            ],
        )?;

        self.append_event_in_tx(&tx, &id, &PlanEvent::PlanActivated, "system")?;
        tx.commit()?;

        self.get_plan(&id)?.ok_or(PlanError::NotFound { id })
    }

    pub fn get_plan(&self, id: &str) -> Result<Option<Plan>, PlanError> {
        self.db
            .connection()
            .query_row(
                "SELECT
                    id, workspace_id, name, description, mode, status,
                    root_session_id, created_at, updated_at, completed_at, metadata
                 FROM plans WHERE id = ?1",
                params![id],
                row_to_plan,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn update_plan_status(&self, id: &str, new_status: PlanStatus) -> Result<(), PlanError> {
        let now = Utc::now().to_rfc3339();
        self.db.connection().execute(
            "UPDATE plans
             SET status = ?1,
                 updated_at = ?2,
                 completed_at = CASE
                     WHEN ?1 IN ('completed', 'failed') THEN ?2
                     ELSE completed_at
                 END
             WHERE id = ?3",
            params![new_status.to_string(), now, id],
        )?;
        Ok(())
    }

    pub fn update_plan_mode(&self, id: &str, mode: PlanMode) -> Result<(), PlanError> {
        let now = Utc::now().to_rfc3339();
        self.db.connection().execute(
            "UPDATE plans
             SET mode = ?1,
                 updated_at = ?2
             WHERE id = ?3",
            params![mode.to_string(), now, id],
        )?;
        Ok(())
    }

    pub fn list_plans(&self, workspace_id: &str) -> Result<Vec<Plan>, PlanError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT
                id, workspace_id, name, description, mode, status,
                root_session_id, created_at, updated_at, completed_at, metadata
             FROM plans
             WHERE workspace_id = ?1
             ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![workspace_id], row_to_plan)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn add_decision(
        &self,
        plan_id: &str,
        decision: &str,
        rationale: Option<&str>,
        decided_by: DecisionAuthor,
        decision_type: Option<DecisionType>,
        is_locked: bool,
    ) -> Result<Decision, PlanError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let tx = self.db.connection().unchecked_transaction()?;
        tx.execute(
            "INSERT INTO plan_decisions (
                id, plan_id, decision, rationale, alternatives_considered,
                decided_by, decision_type, is_locked, created_at, superseded_by
             ) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, NULL)",
            params![
                id,
                plan_id,
                decision,
                rationale,
                decided_by.to_string(),
                decision_type.as_ref().map(ToString::to_string),
                if is_locked { 1 } else { 0 },
                now.to_rfc3339(),
            ],
        )?;

        self.append_event_in_tx(
            &tx,
            plan_id,
            &PlanEvent::DecisionRecorded {
                decision_id: id.clone(),
                decision: decision.to_string(),
                rationale: rationale.map(ToString::to_string),
            },
            "system",
        )?;
        tx.commit()?;

        self.get_decision(&id)?.ok_or(PlanError::NotFound { id })
    }

    pub fn add_constraint(
        &self,
        plan_id: &str,
        constraint_type: ConstraintType,
        description: &str,
        source_decision_id: Option<&str>,
    ) -> Result<Constraint, PlanError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let tx = self.db.connection().unchecked_transaction()?;
        tx.execute(
            "INSERT INTO plan_constraints (
                id, plan_id, constraint_type, description, source_decision_id, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                plan_id,
                constraint_type.to_string(),
                description,
                source_decision_id,
                now,
            ],
        )?;
        self.append_event_in_tx(
            &tx,
            plan_id,
            &PlanEvent::ConstraintAdded {
                constraint_id: id.clone(),
                constraint_type: constraint_type.to_string(),
                description: description.to_string(),
            },
            "system",
        )?;
        tx.commit()?;
        self.get_constraint(&id)?.ok_or(PlanError::NotFound { id })
    }

    pub fn add_goal(
        &self,
        plan_id: &str,
        description: &str,
        priority: GoalPriority,
    ) -> Result<Goal, PlanError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let tx = self.db.connection().unchecked_transaction()?;
        tx.execute(
            "INSERT INTO plan_goals (id, plan_id, description, priority, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                plan_id,
                description,
                priority.to_string(),
                GoalStatus::Pending.to_string(),
                now,
            ],
        )?;
        self.append_event_in_tx(
            &tx,
            plan_id,
            &PlanEvent::GoalAdded {
                goal_id: id.clone(),
                description: description.to_string(),
                priority: priority.to_string(),
            },
            "system",
        )?;
        tx.commit()?;
        self.get_goal(&id)?.ok_or(PlanError::NotFound { id })
    }

    pub fn update_goal_status(&self, goal_id: &str, status: GoalStatus) -> Result<(), PlanError> {
        let tx = self.db.connection().unchecked_transaction()?;
        let existing = tx
            .query_row(
                "SELECT id, plan_id, description, priority, status, created_at
                 FROM plan_goals WHERE id = ?1",
                params![goal_id],
                row_to_goal,
            )
            .optional()?;

        let Some(goal) = existing else {
            return Err(PlanError::NotFound {
                id: goal_id.to_string(),
            });
        };

        tx.execute(
            "UPDATE plan_goals SET status = ?1 WHERE id = ?2",
            params![status.to_string(), goal_id],
        )?;
        self.append_event_in_tx(
            &tx,
            &goal.plan_id,
            &PlanEvent::GoalStatusChanged {
                goal_id: goal_id.to_string(),
                old_status: goal.status.to_string(),
                new_status: status.to_string(),
            },
            "system",
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_task_to_plan(
        &self,
        plan_id: &str,
        task_id: &str,
        title: &str,
        dep_ids: &[String],
    ) -> Result<(), PlanError> {
        let tx = self.db.connection().unchecked_transaction()?;

        let workspace_id: String = tx.query_row(
            "SELECT workspace_id FROM plans WHERE id = ?1",
            params![plan_id],
            |row| row.get(0),
        )?;

        let task_exists = tx
            .query_row(
                "SELECT id FROM tasks WHERE id = ?1",
                params![task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .is_some();
        let now = Utc::now().to_rfc3339();

        if task_exists {
            tx.execute(
                "UPDATE tasks
                 SET plan_id = ?1, title = ?2, updated_at = ?3
                 WHERE id = ?4",
                params![plan_id, title, now, task_id],
            )?;
        } else {
            tx.execute(
                "INSERT INTO tasks (
                    id, workspace_id, root_session_id, parent_id, title, description,
                    status, session_id, created_by, priority, created_at, updated_at,
                    started_at, completed_at, result_diff, result_summary, result_files,
                    result_verify, error, retry_count, max_retries, metadata, plan_id
                 ) VALUES (
                    ?1, ?2, '', NULL, ?3, ?4,
                    'pending', NULL, 'planner', 0, ?5, ?6,
                    NULL, NULL, NULL, NULL, NULL,
                    NULL, NULL, 0, 3, NULL, ?7
                 )",
                params![task_id, workspace_id, title, title, now, now, plan_id],
            )?;
        }

        for dep_id in dep_ids {
            if self.has_cycle(task_id, dep_id)? {
                return Err(PlanError::CycleDetected);
            }
            tx.execute(
                "INSERT OR IGNORE INTO task_deps (task_id, depends_on) VALUES (?1, ?2)",
                params![task_id, dep_id],
            )?;
        }

        self.append_event_in_tx(
            &tx,
            plan_id,
            &PlanEvent::TaskAdded {
                task_id: task_id.to_string(),
                title: title.to_string(),
                dep_ids: dep_ids.to_vec(),
            },
            "system",
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_task_from_plan(&self, plan_id: &str, task_id: &str) -> Result<(), PlanError> {
        let tx = self.db.connection().unchecked_transaction()?;
        tx.execute(
            "DELETE FROM task_deps WHERE task_id = ?1 OR depends_on = ?1",
            params![task_id],
        )?;
        tx.execute(
            "UPDATE tasks SET plan_id = NULL WHERE id = ?1 AND plan_id = ?2",
            params![task_id, plan_id],
        )?;

        self.append_event_in_tx(
            &tx,
            plan_id,
            &PlanEvent::TaskRemoved {
                task_id: task_id.to_string(),
                strategy: crate::events::RemovalStrategy::Skip,
            },
            "system",
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn add_dependency(
        &self,
        plan_id: &str,
        from_task: &str,
        to_task: &str,
    ) -> Result<(), PlanError> {
        if self.has_cycle(from_task, to_task)? {
            return Err(PlanError::CycleDetected);
        }

        let tx = self.db.connection().unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO task_deps (task_id, depends_on) VALUES (?1, ?2)",
            params![from_task, to_task],
        )?;
        self.append_event_in_tx(
            &tx,
            plan_id,
            &PlanEvent::DependencyAdded {
                from_task: from_task.to_string(),
                to_task: to_task.to_string(),
            },
            "system",
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove_dependency(
        &self,
        plan_id: &str,
        from_task: &str,
        to_task: &str,
    ) -> Result<(), PlanError> {
        let tx = self.db.connection().unchecked_transaction()?;
        tx.execute(
            "DELETE FROM task_deps WHERE task_id = ?1 AND depends_on = ?2",
            params![from_task, to_task],
        )?;
        self.append_event_in_tx(
            &tx,
            plan_id,
            &PlanEvent::DependencyRemoved {
                from_task: from_task.to_string(),
                to_task: to_task.to_string(),
            },
            "system",
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn has_cycle(&self, task_id: &str, new_dep: &str) -> Result<bool, PlanError> {
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
            params![new_dep, task_id],
            |row| row.get(0),
        )?;

        Ok(has_cycle == 1)
    }

    pub fn append_event(
        &self,
        plan_id: &str,
        event: &PlanEvent,
        created_by: &str,
    ) -> Result<(), PlanError> {
        let tx = self.db.connection().unchecked_transaction()?;
        self.append_event_in_tx(&tx, plan_id, event, created_by)?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_events(&self, plan_id: &str) -> Result<Vec<StoredEvent>, PlanError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT id, plan_id, sequence, event_type, payload, created_by, created_at
             FROM plan_events
             WHERE plan_id = ?1
             ORDER BY sequence ASC",
        )?;
        let rows = stmt.query_map(params![plan_id], row_to_stored_event)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn get_plan_state(&self, plan_id: &str) -> Result<PlanState, PlanError> {
        let plan = self.get_plan(plan_id)?.ok_or_else(|| PlanError::NotFound {
            id: plan_id.to_string(),
        })?;

        let mut task_stmt = self
            .db
            .connection()
            .prepare("SELECT id FROM tasks WHERE plan_id = ?1 ORDER BY created_at ASC, id ASC")?;
        let task_rows = task_stmt.query_map(params![plan_id], |row| row.get::<_, String>(0))?;
        let task_ids = task_rows.collect::<Result<Vec<_>, _>>()?;

        let mut decisions_stmt = self.db.connection().prepare(
            "SELECT
                id, plan_id, decision, rationale, alternatives_considered,
                decided_by, decision_type, is_locked, created_at, superseded_by
             FROM plan_decisions
             WHERE plan_id = ?1
             ORDER BY created_at ASC",
        )?;
        let decisions_rows = decisions_stmt.query_map(params![plan_id], row_to_decision)?;
        let decisions = decisions_rows.collect::<Result<Vec<_>, _>>()?;

        let mut constraints_stmt = self.db.connection().prepare(
            "SELECT
                id, plan_id, constraint_type, description, source_decision_id, created_at
             FROM plan_constraints
             WHERE plan_id = ?1
             ORDER BY created_at ASC",
        )?;
        let constraints_rows = constraints_stmt.query_map(params![plan_id], row_to_constraint)?;
        let constraints = constraints_rows.collect::<Result<Vec<_>, _>>()?;

        let mut goals_stmt = self.db.connection().prepare(
            "SELECT id, plan_id, description, priority, status, created_at
             FROM plan_goals
             WHERE plan_id = ?1
             ORDER BY created_at ASC",
        )?;
        let goals_rows = goals_stmt.query_map(params![plan_id], row_to_goal)?;
        let goals = goals_rows.collect::<Result<Vec<_>, _>>()?;

        let events = self.get_events(plan_id)?;

        Ok(PlanState {
            plan,
            task_ids,
            decisions,
            constraints,
            goals,
            events,
        })
    }

    fn append_event_in_tx(
        &self,
        tx: &Transaction<'_>,
        plan_id: &str,
        event: &PlanEvent,
        created_by: &str,
    ) -> Result<(), PlanError> {
        let sequence: i64 = tx.query_row(
            "SELECT COALESCE(MAX(sequence), 0) + 1 FROM plan_events WHERE plan_id = ?1",
            params![plan_id],
            |row| row.get(0),
        )?;
        let payload = serde_json::to_string(event)?;
        tx.execute(
            "INSERT INTO plan_events (plan_id, sequence, event_type, payload, created_by, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                plan_id,
                sequence,
                event_type(event),
                payload,
                created_by,
                Utc::now().to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    fn get_decision(&self, id: &str) -> Result<Option<Decision>, PlanError> {
        self.db
            .connection()
            .query_row(
                "SELECT
                    id, plan_id, decision, rationale, alternatives_considered,
                    decided_by, decision_type, is_locked, created_at, superseded_by
                 FROM plan_decisions
                 WHERE id = ?1",
                params![id],
                row_to_decision,
            )
            .optional()
            .map_err(Into::into)
    }

    fn get_constraint(&self, id: &str) -> Result<Option<Constraint>, PlanError> {
        self.db
            .connection()
            .query_row(
                "SELECT
                    id, plan_id, constraint_type, description, source_decision_id, created_at
                 FROM plan_constraints
                 WHERE id = ?1",
                params![id],
                row_to_constraint,
            )
            .optional()
            .map_err(Into::into)
    }

    fn get_goal(&self, id: &str) -> Result<Option<Goal>, PlanError> {
        self.db
            .connection()
            .query_row(
                "SELECT id, plan_id, description, priority, status, created_at
                 FROM plan_goals
                 WHERE id = ?1",
                params![id],
                row_to_goal,
            )
            .optional()
            .map_err(Into::into)
    }
}

fn event_type(event: &PlanEvent) -> &'static str {
    match event {
        PlanEvent::TaskAdded { .. } => "task_added",
        PlanEvent::TaskRemoved { .. } => "task_removed",
        PlanEvent::DependencyAdded { .. } => "dependency_added",
        PlanEvent::DependencyRemoved { .. } => "dependency_removed",
        PlanEvent::DecisionRecorded { .. } => "decision_recorded",
        PlanEvent::ConstraintAdded { .. } => "constraint_added",
        PlanEvent::GoalAdded { .. } => "goal_added",
        PlanEvent::GoalStatusChanged { .. } => "goal_status_changed",
        PlanEvent::PlanFinalized { .. } => "plan_finalized",
        PlanEvent::PlanActivated => "plan_activated",
        PlanEvent::PlanCompleted { .. } => "plan_completed",
        PlanEvent::PlanFailed { .. } => "plan_failed",
        PlanEvent::ModeSwitched { .. } => "mode_switched",
        PlanEvent::ReplanTriggered { .. } => "replan_triggered",
        PlanEvent::TaskStatusChanged { .. } => "task_status_changed",
    }
}

fn parse_ts(ts: String) -> Result<DateTime<Utc>, rusqlite::Error> {
    DateTime::parse_from_rfc3339(&ts)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
        })
}

fn parse_opt_ts(ts: Option<String>) -> Result<Option<DateTime<Utc>>, rusqlite::Error> {
    ts.map(parse_ts).transpose()
}

fn row_to_plan(row: &rusqlite::Row<'_>) -> rusqlite::Result<Plan> {
    let mode: String = row.get(4)?;
    let status: String = row.get(5)?;
    let metadata: Option<String> = row.get(10)?;
    Ok(Plan {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        mode: PlanMode::from_str(&mode).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    err.to_string(),
                )),
            )
        })?,
        status: PlanStatus::from_str(&status).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    err.to_string(),
                )),
            )
        })?,
        root_session_id: row.get(6)?,
        created_at: parse_ts(row.get(7)?)?,
        updated_at: parse_ts(row.get(8)?)?,
        completed_at: parse_opt_ts(row.get(9)?)?,
        metadata: metadata
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(|err| {
                rusqlite::Error::FromSqlConversionFailure(
                    10,
                    rusqlite::types::Type::Text,
                    Box::new(err),
                )
            })?,
    })
}

fn row_to_decision(row: &rusqlite::Row<'_>) -> rusqlite::Result<Decision> {
    let decided_by: String = row.get(5)?;
    let decision_type: Option<String> = row.get(6)?;
    Ok(Decision {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        decision: row.get(2)?,
        rationale: row.get(3)?,
        alternatives_considered: row.get(4)?,
        decided_by: DecisionAuthor::from_str(&decided_by).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    err.to_string(),
                )),
            )
        })?,
        decision_type: decision_type
            .map(|val| {
                DecisionType::from_str(&val).map_err(|err| {
                    rusqlite::Error::FromSqlConversionFailure(
                        6,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            err.to_string(),
                        )),
                    )
                })
            })
            .transpose()?,
        is_locked: row.get::<_, i64>(7)? == 1,
        created_at: parse_ts(row.get(8)?)?,
        superseded_by: row.get(9)?,
    })
}

fn row_to_constraint(row: &rusqlite::Row<'_>) -> rusqlite::Result<Constraint> {
    let ctype: String = row.get(2)?;
    Ok(Constraint {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        constraint_type: ConstraintType::from_str(&ctype).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                2,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    err.to_string(),
                )),
            )
        })?,
        description: row.get(3)?,
        source_decision_id: row.get(4)?,
        created_at: parse_ts(row.get(5)?)?,
    })
}

fn row_to_goal(row: &rusqlite::Row<'_>) -> rusqlite::Result<Goal> {
    let priority: String = row.get(3)?;
    let status: String = row.get(4)?;
    Ok(Goal {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        description: row.get(2)?,
        priority: GoalPriority::from_str(&priority).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    err.to_string(),
                )),
            )
        })?,
        status: GoalStatus::from_str(&status).map_err(|err| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    err.to_string(),
                )),
            )
        })?,
        created_at: parse_ts(row.get(5)?)?,
    })
}

fn row_to_stored_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredEvent> {
    let payload_json: String = row.get(4)?;
    let payload = serde_json::from_str(&payload_json).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(err))
    })?;
    Ok(StoredEvent {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        sequence: row.get(2)?,
        event_type: row.get(3)?,
        payload,
        created_by: row.get(5)?,
        created_at: parse_ts(row.get(6)?)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sunny_tasks::store::TaskStore;

    fn make_store() -> (PlanStore, TaskStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let db_path = dir.path().join("test.db");

        let task_db = sunny_store::Database::open(db_path.as_path()).expect("should open task db");
        let task_store = TaskStore::new(task_db);

        let plan_db = sunny_store::Database::open(db_path.as_path()).expect("should open plan db");
        let plan_store = PlanStore::new(plan_db);

        (plan_store, task_store, dir)
    }

    #[test]
    fn test_plan_store_lifecycle_state_and_events() {
        let (store, task_store, _dir) = make_store();
        let workspace = task_store
            .find_or_create_workspace("/tmp/repo")
            .expect("should create workspace");
        let plan = store
            .create_plan(
                &workspace.id,
                "My Plan",
                Some("plan description"),
                PlanMode::Smart,
                Some("root-session"),
            )
            .expect("should create plan");

        let task_a = "task-a";
        let task_b = "task-b";
        let task_c = "task-c";

        store
            .add_task_to_plan(&plan.id, task_a, "Task A", &[])
            .expect("should add task a");
        store
            .add_task_to_plan(&plan.id, task_b, "Task B", &[])
            .expect("should add task b");
        store
            .add_task_to_plan(&plan.id, task_c, "Task C", &[])
            .expect("should add task c");
        store
            .add_dependency(&plan.id, task_a, task_b)
            .expect("should add dependency a->b");
        store
            .add_dependency(&plan.id, task_b, task_c)
            .expect("should add dependency b->c");

        let state = store
            .get_plan_state(&plan.id)
            .expect("should return plan state");

        assert_eq!(state.plan.id, plan.id);
        assert_eq!(state.task_ids.len(), 3);
        assert!(state.task_ids.contains(&task_a.to_string()));
        assert!(state.task_ids.contains(&task_b.to_string()));
        assert!(state.task_ids.contains(&task_c.to_string()));
        assert!(state.decisions.is_empty());
        assert!(state.constraints.is_empty());
        assert!(state.goals.is_empty());
        assert_eq!(state.events.len(), 6);
        assert_eq!(state.events[0].sequence, 1);
        assert_eq!(state.events[5].sequence, 6);
    }

    #[test]
    fn test_add_dependency_blocks_cycle() {
        let (store, task_store, _dir) = make_store();
        let workspace = task_store
            .find_or_create_workspace("/tmp/repo")
            .expect("should create workspace");
        let plan = store
            .create_plan(&workspace.id, "My Plan", None, PlanMode::Quick, None)
            .expect("should create plan");

        store
            .add_task_to_plan(&plan.id, "a", "Task A", &[])
            .expect("should add task a");
        store
            .add_task_to_plan(&plan.id, "b", "Task B", &[])
            .expect("should add task b");
        store
            .add_task_to_plan(&plan.id, "c", "Task C", &[])
            .expect("should add task c");

        store
            .add_dependency(&plan.id, "a", "b")
            .expect("should add a->b");
        store
            .add_dependency(&plan.id, "b", "c")
            .expect("should add b->c");

        let err = store
            .add_dependency(&plan.id, "c", "a")
            .expect_err("should fail with cycle");

        assert!(matches!(err, PlanError::CycleDetected));

        let events = store.get_events(&plan.id).expect("should get events");
        assert_eq!(events.len(), 6);
    }
}
