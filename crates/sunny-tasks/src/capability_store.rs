use std::str::FromStr;

use chrono::{DateTime, Utc};
use rusqlite::OptionalExtension;
use uuid::Uuid;

use crate::error::TaskError;
use crate::model::{CapabilityRequest, CapabilityRequestStatus, CapabilityScope};

pub struct CapabilityStore {
    db: sunny_store::Database,
}

impl CapabilityStore {
    pub fn new(db: sunny_store::Database) -> Self {
        Self { db }
    }

    pub fn open_default() -> Result<Self, TaskError> {
        Ok(Self::new(sunny_store::Database::open_default()?))
    }

    pub fn create_request(
        &self,
        session_id: &str,
        task_id: Option<&str>,
        capability: &str,
        requested_rhs: Option<&[String]>,
        example_command: Option<&str>,
        reason: &str,
    ) -> Result<CapabilityRequest, TaskError> {
        let id = Uuid::new_v4().to_string();
        let requested_at = Utc::now();
        let requested_rhs_json = requested_rhs.map(serde_json::to_string).transpose()?;

        self.db.connection().execute(
            "INSERT INTO capability_requests (
                id, session_id, task_id, capability, requested_rhs, example_command,
                reason, status, scope, requested_at, resolved_at, resolved_by
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', NULL, ?8, NULL, NULL)",
            rusqlite::params![
                id,
                session_id,
                task_id,
                capability,
                requested_rhs_json,
                example_command,
                reason,
                requested_at.to_rfc3339(),
            ],
        )?;

        self.get_request(&id)?.ok_or(TaskError::NotFound { id })
    }

    pub fn approve(
        &self,
        request_id: &str,
        scope: CapabilityScope,
    ) -> Result<CapabilityRequest, TaskError> {
        let changed = self.db.connection().execute(
            "UPDATE capability_requests
             SET status = 'approved', scope = ?1, resolved_at = ?2
             WHERE id = ?3 AND status = 'pending'",
            rusqlite::params![scope.to_string(), Utc::now().to_rfc3339(), request_id],
        )?;

        if changed == 0 {
            return Err(TaskError::NotFound {
                id: request_id.to_string(),
            });
        }

        self.get_request(request_id)?.ok_or(TaskError::NotFound {
            id: request_id.to_string(),
        })
    }

    pub fn deny(&self, request_id: &str) -> Result<CapabilityRequest, TaskError> {
        let changed = self.db.connection().execute(
            "UPDATE capability_requests
             SET status = 'denied', resolved_at = ?1
             WHERE id = ?2 AND status = 'pending'",
            rusqlite::params![Utc::now().to_rfc3339(), request_id],
        )?;

        if changed == 0 {
            return Err(TaskError::NotFound {
                id: request_id.to_string(),
            });
        }

        self.get_request(request_id)?.ok_or(TaskError::NotFound {
            id: request_id.to_string(),
        })
    }

    pub fn pending_requests(&self) -> Result<Vec<CapabilityRequest>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT
                id, session_id, task_id, capability, requested_rhs, example_command,
                reason, status, scope, requested_at, resolved_at, resolved_by
             FROM capability_requests
             WHERE status = 'pending'
             ORDER BY requested_at ASC",
        )?;
        let rows = stmt.query_map([], row_to_capability_request)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    pub fn audit_log(&self, limit: Option<u32>) -> Result<Vec<CapabilityRequest>, TaskError> {
        let max_rows = i64::from(limit.unwrap_or(100));
        let mut stmt = self.db.connection().prepare(
            "SELECT
                id, session_id, task_id, capability, requested_rhs, example_command,
                reason, status, scope, requested_at, resolved_at, resolved_by
             FROM capability_requests
             ORDER BY requested_at DESC
             LIMIT ?1",
        )?;
        let rows = stmt.query_map(rusqlite::params![max_rows], row_to_capability_request)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    pub fn approved_for_session(
        &self,
        session_id: &str,
    ) -> Result<Vec<CapabilityRequest>, TaskError> {
        let mut stmt = self.db.connection().prepare(
            "SELECT
                id, session_id, task_id, capability, requested_rhs, example_command,
                reason, status, scope, requested_at, resolved_at, resolved_by
             FROM capability_requests
             WHERE session_id = ?1 AND status = 'approved'
             ORDER BY requested_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], row_to_capability_request)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(TaskError::Db)
    }

    fn get_request(&self, request_id: &str) -> Result<Option<CapabilityRequest>, TaskError> {
        self.db
            .connection()
            .query_row(
                "SELECT
                    id, session_id, task_id, capability, requested_rhs, example_command,
                    reason, status, scope, requested_at, resolved_at, resolved_by
                 FROM capability_requests
                 WHERE id = ?1",
                rusqlite::params![request_id],
                row_to_capability_request,
            )
            .optional()
            .map_err(TaskError::Db)
    }
}

fn row_to_capability_request(row: &rusqlite::Row<'_>) -> rusqlite::Result<CapabilityRequest> {
    let status_raw: String = row.get(7)?;
    let status = match status_raw.as_str() {
        "pending" => CapabilityRequestStatus::Pending,
        "approved" => CapabilityRequestStatus::Approved,
        "denied" => CapabilityRequestStatus::Denied,
        other => {
            return Err(rusqlite::Error::InvalidColumnType(
                7,
                format!("invalid capability request status: {other}"),
                rusqlite::types::Type::Text,
            ));
        }
    };

    let scope = row
        .get::<_, Option<String>>(8)?
        .map(|raw| {
            CapabilityScope::from_str(&raw).map_err(|e| {
                rusqlite::Error::InvalidColumnType(
                    8,
                    format!("invalid capability scope: {e}"),
                    rusqlite::types::Type::Text,
                )
            })
        })
        .transpose()?;

    let requested_rhs = row
        .get::<_, Option<String>>(4)?
        .map(|raw| {
            serde_json::from_str::<Vec<String>>(&raw).map_err(|e| {
                rusqlite::Error::InvalidColumnType(
                    4,
                    format!("invalid requested_rhs json: {e}"),
                    rusqlite::types::Type::Text,
                )
            })
        })
        .transpose()?;

    Ok(CapabilityRequest {
        id: row.get(0)?,
        session_id: row.get(1)?,
        task_id: row.get(2)?,
        capability: row.get(3)?,
        requested_rhs,
        example_command: row.get(5)?,
        reason: row.get(6)?,
        status,
        scope,
        requested_at: parse_required_datetime(9, row.get::<_, String>(9)?)?,
        resolved_at: parse_optional_datetime(10, row.get::<_, Option<String>>(10)?)?,
        resolved_by: row.get(11)?,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store() -> (CapabilityStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let db = sunny_store::Database::open(dir.path().join("test.db").as_path())
            .expect("should open db");
        db.connection()
            .execute(
                "CREATE TABLE IF NOT EXISTS tasks (
                    id TEXT PRIMARY KEY
                )",
                [],
            )
            .expect("should create tasks table");
        (CapabilityStore::new(db), dir)
    }

    #[test]
    fn test_create_request_stores_pending() {
        let (store, _dir) = make_store();
        let requested_rhs = vec!["tail".to_string(), "grep".to_string()];

        let req = store
            .create_request(
                "session-1",
                None,
                "shell_pipes",
                Some(&requested_rhs),
                Some("echo x | tail -n 1"),
                "need pipeline",
            )
            .expect("should create request");

        assert_eq!(req.status, CapabilityRequestStatus::Pending);
        assert_eq!(req.requested_rhs.as_ref(), Some(&requested_rhs));
    }

    #[test]
    fn test_approve_updates_status_and_scope() {
        let (store, _dir) = make_store();
        let req = store
            .create_request("session-1", None, "shell_pipes", None, None, "need pipes")
            .expect("should create request");

        let approved = store
            .approve(&req.id, CapabilityScope::Workspace)
            .expect("should approve request");

        assert_eq!(approved.status, CapabilityRequestStatus::Approved);
        assert_eq!(approved.scope, Some(CapabilityScope::Workspace));
        assert!(approved.resolved_at.is_some());
    }

    #[test]
    fn test_deny_updates_status() {
        let (store, _dir) = make_store();
        let req = store
            .create_request("session-1", None, "git_write", None, None, "need commit")
            .expect("should create request");

        let denied = store.deny(&req.id).expect("should deny request");

        assert_eq!(denied.status, CapabilityRequestStatus::Denied);
        assert!(denied.resolved_at.is_some());
    }

    #[test]
    fn test_pending_requests_returns_only_pending() {
        let (store, _dir) = make_store();
        let one = store
            .create_request("session-1", None, "shell_pipes", None, None, "need pipes")
            .expect("should create request");
        let two = store
            .create_request("session-1", None, "git_write", None, None, "need commit")
            .expect("should create request");
        store
            .approve(&two.id, CapabilityScope::Session)
            .expect("should approve request");

        let pending = store.pending_requests().expect("should list pending");

        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, one.id);
    }

    #[test]
    fn test_audit_log_returns_all() {
        let (store, _dir) = make_store();
        let one = store
            .create_request("session-1", None, "shell_pipes", None, None, "need pipes")
            .expect("should create request");
        let two = store
            .create_request("session-1", None, "git_write", None, None, "need commit")
            .expect("should create request");
        store.deny(&one.id).expect("should deny request");

        let all = store.audit_log(None).expect("should list audit log");

        assert_eq!(all.len(), 2);
        let ids: Vec<&str> = all.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&one.id.as_str()));
        assert!(ids.contains(&two.id.as_str()));
    }

    #[test]
    fn test_approved_for_session_filters_correctly() {
        let (store, _dir) = make_store();
        let s1_approved = store
            .create_request("session-1", None, "shell_pipes", None, None, "need pipes")
            .expect("should create request");
        let s1_pending = store
            .create_request("session-1", None, "git_write", None, None, "need commit")
            .expect("should create request");
        let s2_approved = store
            .create_request("session-2", None, "shell_pipes", None, None, "need pipes")
            .expect("should create request");

        store
            .approve(&s1_approved.id, CapabilityScope::Session)
            .expect("should approve request");
        store
            .approve(&s2_approved.id, CapabilityScope::Session)
            .expect("should approve request");

        let approved = store
            .approved_for_session("session-1")
            .expect("should list approved requests");

        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].id, s1_approved.id);
        assert_ne!(approved[0].id, s1_pending.id);
    }
}
