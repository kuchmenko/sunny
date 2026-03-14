//! Session storage and retrieval

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sunny_mind::{ChatMessage, ChatRole, ToolCall};
use uuid::Uuid;

use crate::{Database, StoreError};

/// A persisted chat session record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SavedSession {
    pub id: String,
    pub title: Option<String>,
    pub model: Option<String>,
    pub working_dir: String,
    pub token_count: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Synchronous session store backed by SQLite.
///
/// All methods are synchronous — callers in async contexts must use
/// `tokio::task::spawn_blocking`.
pub struct SessionStore {
    db: Database,
}

impl SessionStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Create a new session record and return it.
    pub fn create_session(
        &self,
        working_dir: &str,
        model: Option<&str>,
    ) -> Result<SavedSession, StoreError> {
        let id = Uuid::new_v4().to_string();
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.db.connection().execute(
            "INSERT INTO sessions (id, title, model, working_dir, token_count, created_at, updated_at) \
             VALUES (?1, NULL, ?2, ?3, 0, ?4, ?5)",
            rusqlite::params![id, model, working_dir, now_str, now_str],
        )?;
        Ok(SavedSession {
            id,
            title: None,
            model: model.map(String::from),
            working_dir: working_dir.to_string(),
            token_count: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Create a new session record with an explicit ID and return it.
    pub fn create_session_with_id(
        &self,
        id: &str,
        working_dir: &str,
        model: Option<&str>,
    ) -> Result<SavedSession, StoreError> {
        let now = Utc::now();
        let now_str = now.to_rfc3339();
        self.db.connection().execute(
            "INSERT INTO sessions (id, title, model, working_dir, token_count, created_at, updated_at) \
             VALUES (?1, NULL, ?2, ?3, 0, ?4, ?5)",
            rusqlite::params![id, model, working_dir, now_str, now_str],
        )?;
        Ok(SavedSession {
            id: id.to_string(),
            title: None,
            model: model.map(String::from),
            working_dir: working_dir.to_string(),
            token_count: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Persist messages for a session (replaces any existing messages).
    pub fn save_messages(
        &self,
        session_id: &str,
        messages: &[ChatMessage],
    ) -> Result<(), StoreError> {
        let conn = self.db.connection();
        conn.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            rusqlite::params![session_id],
        )?;
        let now_str = Utc::now().to_rfc3339();
        for (seq, msg) in messages.iter().enumerate() {
            let role = role_to_str(&msg.role);
            let tool_calls_json = msg
                .tool_calls
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?;
            conn.execute(
                "INSERT INTO messages \
                 (session_id, seq, role, content, tool_calls, tool_call_id, reasoning_content, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    session_id,
                    seq as i64,
                    role,
                    msg.content,
                    tool_calls_json,
                    msg.tool_call_id,
                    msg.reasoning_content,
                    now_str
                ],
            )?;
        }
        conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            rusqlite::params![Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }

    /// Load session metadata by ID.
    pub fn load_session(&self, session_id: &str) -> Result<Option<SavedSession>, StoreError> {
        let conn = self.db.connection();
        let mut stmt = conn.prepare(
            "SELECT id, title, model, working_dir, token_count, created_at, updated_at \
             FROM sessions WHERE id = ?1",
        )?;
        let result = stmt.query_row(rusqlite::params![session_id], row_to_session);
        match result {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StoreError::Db(e)),
        }
    }

    /// Load all messages for a session in sequence order.
    pub fn load_messages(&self, session_id: &str) -> Result<Vec<ChatMessage>, StoreError> {
        let conn = self.db.connection();
        let mut stmt = conn.prepare(
            "SELECT role, content, tool_calls, tool_call_id, reasoning_content \
             FROM messages WHERE session_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut messages = Vec::new();
        for row in rows {
            let (role_str, content, tool_calls_json, tool_call_id, reasoning_content) = row?;
            let role = str_to_role(&role_str).map_err(StoreError::InvalidData)?;
            let tool_calls = tool_calls_json
                .map(|json| serde_json::from_str::<Vec<ToolCall>>(&json))
                .transpose()?;
            messages.push(ChatMessage {
                role,
                content,
                tool_calls,
                tool_call_id,
                reasoning_content,
            });
        }
        Ok(messages)
    }

    /// List sessions, optionally filtered by working directory. Ordered by updated_at DESC.
    pub fn list_sessions(
        &self,
        working_dir: Option<&str>,
    ) -> Result<Vec<SavedSession>, StoreError> {
        let conn = self.db.connection();
        if let Some(dir) = working_dir {
            let mut stmt = conn.prepare(
                "SELECT id, title, model, working_dir, token_count, created_at, updated_at \
                 FROM sessions WHERE working_dir = ?1 ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map(rusqlite::params![dir], row_to_session)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::Db)
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, title, model, working_dir, token_count, created_at, updated_at \
                 FROM sessions ORDER BY updated_at DESC",
            )?;
            let rows = stmt.query_map([], row_to_session)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::Db)
        }
    }

    pub fn search_sessions(&self, query: &str) -> Result<Vec<SavedSession>, StoreError> {
        let conn = self.db.connection();
        let pattern = format!("%{query}%");
        let mut stmt = conn.prepare(
            "SELECT id, title, model, working_dir, token_count, created_at, updated_at \
             FROM sessions WHERE id LIKE ?1 OR title LIKE ?1 ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map(rusqlite::params![pattern], row_to_session)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(StoreError::Db)
    }

    /// Return the most recently updated session for the given working directory.
    pub fn most_recent_session(
        &self,
        working_dir: &str,
    ) -> Result<Option<SavedSession>, StoreError> {
        let conn = self.db.connection();
        let mut stmt = conn.prepare(
            "SELECT id, title, model, working_dir, token_count, created_at, updated_at \
             FROM sessions WHERE working_dir = ?1 ORDER BY updated_at DESC LIMIT 1",
        )?;
        let result = stmt.query_row(rusqlite::params![working_dir], row_to_session);
        match result {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(StoreError::Db(e)),
        }
    }

    /// Update the display title of a session.
    pub fn update_title(&self, session_id: &str, title: &str) -> Result<(), StoreError> {
        self.db.connection().execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![title, Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }

    /// Update the tracked token count for a session.
    pub fn update_token_count(&self, session_id: &str, count: u32) -> Result<(), StoreError> {
        self.db.connection().execute(
            "UPDATE sessions SET token_count = ?1, updated_at = ?2 WHERE id = ?3",
            rusqlite::params![count as i64, Utc::now().to_rfc3339(), session_id],
        )?;
        Ok(())
    }

    /// Delete a session and all its messages (CASCADE).
    pub fn delete_session(&self, session_id: &str) -> Result<(), StoreError> {
        self.db.connection().execute(
            "DELETE FROM sessions WHERE id = ?1",
            rusqlite::params![session_id],
        )?;
        Ok(())
    }
}

fn row_to_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedSession> {
    let created_at_str: String = row.get(5)?;
    let updated_at_str: String = row.get(6)?;
    let created_at = DateTime::parse_from_rfc3339(&created_at_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::InvalidColumnType(
                5,
                format!("invalid created_at timestamp: {e}"),
                rusqlite::types::Type::Text,
            )
        })?;
    let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            rusqlite::Error::InvalidColumnType(
                6,
                format!("invalid updated_at timestamp: {e}"),
                rusqlite::types::Type::Text,
            )
        })?;
    Ok(SavedSession {
        id: row.get(0)?,
        title: row.get(1)?,
        model: row.get(2)?,
        working_dir: row.get(3)?,
        token_count: row.get::<_, i64>(4)? as u32,
        created_at,
        updated_at,
    })
}

fn role_to_str(role: &ChatRole) -> &'static str {
    match role {
        ChatRole::System => "system",
        ChatRole::User => "user",
        ChatRole::Assistant => "assistant",
        ChatRole::Tool => "tool",
    }
}

fn str_to_role(s: &str) -> Result<ChatRole, String> {
    match s {
        "system" => Ok(ChatRole::System),
        "user" => Ok(ChatRole::User),
        "assistant" => Ok(ChatRole::Assistant),
        "tool" => Ok(ChatRole::Tool),
        other => Err(format!("unknown role: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Database;
    use sunny_mind::{ChatMessage, ChatRole, ToolCall};
    use tempfile::tempdir;

    fn make_store() -> (SessionStore, tempfile::TempDir) {
        let dir = tempdir().expect("should create temp dir");
        let db =
            Database::open(dir.path().join("test.db").as_path()).expect("should open database");
        (SessionStore::new(db), dir)
    }

    #[test]
    fn test_create_session_returns_valid_session() {
        let (store, _dir) = make_store();
        let session = store
            .create_session("/project/a", Some("claude-sonnet"))
            .expect("should create session");
        assert!(!session.id.is_empty());
        assert_eq!(session.working_dir, "/project/a");
        assert_eq!(session.model.as_deref(), Some("claude-sonnet"));
        assert_eq!(session.token_count, 0);
        assert!(session.title.is_none());
    }

    #[test]
    fn test_session_save_load_roundtrip() {
        let (store, _dir) = make_store();
        let session = store
            .create_session("/project/a", None)
            .expect("should create session");

        let messages = vec![
            ChatMessage {
                role: ChatRole::System,
                content: "You are helpful".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: ChatRole::User,
                content: "Hello".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: ChatRole::Assistant,
                content: String::new(),
                tool_calls: Some(vec![ToolCall {
                    id: "call-1".to_string(),
                    name: "fs_read".to_string(),
                    arguments: "{\"path\":\"a.rs\"}".to_string(),
                    execution_depth: 0,
                }]),
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: ChatRole::Tool,
                content: "file content".to_string(),
                tool_calls: None,
                tool_call_id: Some("call-1".to_string()),
                reasoning_content: None,
            },
            ChatMessage {
                role: ChatRole::Assistant,
                content: "I read the file".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ];

        store
            .save_messages(&session.id, &messages)
            .expect("should save messages");
        let loaded = store
            .load_messages(&session.id)
            .expect("should load messages");

        assert_eq!(loaded.len(), 5);
        assert_eq!(loaded[0].role, ChatRole::System);
        assert_eq!(loaded[1].role, ChatRole::User);
        assert_eq!(loaded[2].role, ChatRole::Assistant);
        let tool_calls = loaded[2]
            .tool_calls
            .as_ref()
            .expect("should have tool calls");
        assert_eq!(tool_calls[0].name, "fs_read");
        assert_eq!(loaded[3].role, ChatRole::Tool);
        assert_eq!(loaded[3].tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(loaded[4].content, "I read the file");
    }

    #[test]
    fn test_list_sessions_by_working_dir() {
        let (store, _dir) = make_store();
        store.create_session("/project/a", None).expect("create 1");
        store.create_session("/project/a", None).expect("create 2");
        store.create_session("/project/b", None).expect("create 3");

        let sessions_a = store
            .list_sessions(Some("/project/a"))
            .expect("should list sessions");
        assert_eq!(sessions_a.len(), 2);

        let sessions_b = store
            .list_sessions(Some("/project/b"))
            .expect("should list sessions");
        assert_eq!(sessions_b.len(), 1);

        let all = store.list_sessions(None).expect("should list all");
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_most_recent_session() {
        let (store, _dir) = make_store();
        let s1 = store.create_session("/project/a", None).expect("create 1");
        let _s2 = store.create_session("/project/a", None).expect("create 2");

        // Bump s1's updated_at by updating its title
        std::thread::sleep(std::time::Duration::from_millis(10));
        store
            .update_title(&s1.id, "Updated First")
            .expect("should update title");

        let recent = store
            .most_recent_session("/project/a")
            .expect("should find recent session");
        assert!(recent.is_some());
        assert_eq!(recent.unwrap().id, s1.id);
    }

    #[test]
    fn test_delete_session_cascades_to_messages() {
        let (store, _dir) = make_store();
        let session = store
            .create_session("/project/a", None)
            .expect("should create session");
        let messages = vec![ChatMessage {
            role: ChatRole::User,
            content: "test".to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }];
        store
            .save_messages(&session.id, &messages)
            .expect("should save messages");

        let loaded = store
            .load_messages(&session.id)
            .expect("should load messages");
        assert_eq!(loaded.len(), 1);

        store
            .delete_session(&session.id)
            .expect("should delete session");

        // Messages should be gone via CASCADE
        let after_delete = store
            .load_messages(&session.id)
            .expect("should load messages after delete");
        assert_eq!(after_delete.len(), 0);

        // Session should be gone
        let session_check = store
            .load_session(&session.id)
            .expect("should query session");
        assert!(session_check.is_none());
    }

    #[test]
    fn test_update_token_count() {
        let (store, _dir) = make_store();
        let session = store
            .create_session("/project/a", None)
            .expect("should create session");
        store
            .update_token_count(&session.id, 1234)
            .expect("should update token count");
        let loaded = store
            .load_session(&session.id)
            .expect("should load session")
            .expect("session should exist");
        assert_eq!(loaded.token_count, 1234);
    }

    #[test]
    fn test_create_session_with_id_then_save_messages() {
        let (store, _dir) = make_store();
        let session = store
            .create_session_with_id("test-session-123", "/project/a", Some("claude-sonnet"))
            .expect("should create session with explicit id");
        assert_eq!(session.id, "test-session-123");
        assert_eq!(session.working_dir, "/project/a");
        assert_eq!(session.model.as_deref(), Some("claude-sonnet"));

        let messages = vec![
            ChatMessage {
                role: ChatRole::User,
                content: "Hello".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: ChatRole::Assistant,
                content: "Hi there".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ];

        store
            .save_messages(&session.id, &messages)
            .expect("should save messages with matching session id");

        let loaded = store
            .load_messages(&session.id)
            .expect("should load messages");
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].role, ChatRole::User);
        assert_eq!(loaded[1].role, ChatRole::Assistant);
    }

    #[test]
    fn test_save_messages_without_session_fails() {
        let (store, _dir) = make_store();
        let messages = vec![ChatMessage {
            role: ChatRole::User,
            content: "test".to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }];

        let result = store.save_messages("nonexistent-session-id", &messages);
        assert!(result.is_err(), "should fail with FK constraint error");
    }
}
