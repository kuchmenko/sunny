//! Session store helpers for the GUI.
//!
//! All functions open a fresh SQLite connection via `tokio::task::spawn_blocking`.
//! The `SessionStore` is synchronous and `!Send`; never share instances across threads.

use crate::bridge::{DisplayMessage, SavedSessionInfo};
use sunny_store::{Database, SessionStore};
use tracing::warn;

/// List all saved sessions, newest first.
///
/// Returns an empty vec on error rather than propagating (UI shows nothing, not a crash).
#[expect(dead_code)]
pub async fn list_sessions() -> Vec<SavedSessionInfo> {
    tokio::task::spawn_blocking(|| {
        let db = match Database::open_default() {
            Ok(db) => db,
            Err(e) => {
                warn!(error = %e, "sessions: failed to open DB for list");
                return Vec::new();
            }
        };
        let store = SessionStore::new(db);
        match store.list_sessions(None) {
            Ok(sessions) => sessions
                .into_iter()
                .map(|s| SavedSessionInfo {
                    id: s.id,
                    title: s.title,
                    message_count: s.token_count as i64,
                    updated_at: s.updated_at.to_rfc3339(),
                })
                .collect(),
            Err(e) => {
                warn!(error = %e, "sessions: failed to list sessions");
                Vec::new()
            }
        }
    })
    .await
    .unwrap_or_default()
}

/// Load messages for a session and convert to `DisplayMessage`.
///
/// Returns an empty vec on error.
#[expect(dead_code)]
pub async fn load_session_messages(session_id: &str) -> Vec<DisplayMessage> {
    let id = session_id.to_string();
    tokio::task::spawn_blocking(move || {
        let db = match Database::open_default() {
            Ok(db) => db,
            Err(e) => {
                warn!(error = %e, "sessions: failed to open DB for load");
                return Vec::new();
            }
        };
        let store = SessionStore::new(db);
        match store.load_messages(&id) {
            Ok(messages) => messages.into_iter().map(DisplayMessage::from).collect(),
            Err(e) => {
                warn!(error = %e, session_id = %id, "sessions: failed to load messages");
                Vec::new()
            }
        }
    })
    .await
    .unwrap_or_default()
}
