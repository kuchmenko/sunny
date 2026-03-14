//! SQLite database connection and schema management

use crate::error::StoreError;
use std::path::Path;

/// SQLite database connection wrapper
pub struct Database {
    conn: rusqlite::Connection,
}

impl Database {
    /// Open a database at the given path, creating it if necessary
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = rusqlite::Connection::open(path)?;
        let db = Self { conn };
        db.set_pragmas()?;
        db.migrate()?;
        Ok(db)
    }

    /// Open the default database at ~/.sunny/sunny.db
    pub fn open_default() -> Result<Self, StoreError> {
        let home = dirs::home_dir()
            .ok_or_else(|| StoreError::Migration("cannot determine home directory".into()))?;
        let sunny_dir = home.join(".sunny");
        std::fs::create_dir_all(&sunny_dir)?;
        Self::open(&sunny_dir.join("sunny.db"))
    }

    /// Get a reference to the underlying SQLite connection
    pub fn connection(&self) -> &rusqlite::Connection {
        &self.conn
    }

    /// Set SQLite pragmas for optimal performance and safety
    fn set_pragmas(&self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;
        Ok(())
    }

    /// Run schema migrations
    fn migrate(&self) -> Result<(), StoreError> {
        // Create sessions table
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                title TEXT,
                model TEXT,
                working_dir TEXT NOT NULL,
                token_count INTEGER DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
            [],
        )?;

        // Create messages table
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq INTEGER NOT NULL,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                tool_calls TEXT,
                tool_call_id TEXT,
                reasoning_content TEXT,
                created_at TEXT NOT NULL
            )",
            [],
        )?;

        // Create index on messages
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, seq)",
            [],
        )?;

        // Create symbols table
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS symbols (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                file_path TEXT NOT NULL,
                line INTEGER NOT NULL,
                end_line INTEGER NOT NULL,
                kind TEXT NOT NULL,
                signature TEXT,
                parent TEXT,
                content_hash TEXT NOT NULL
            )",
            [],
        )?;

        // Create indexes on symbols
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_symbols_name ON symbols(name)",
            [],
        )?;

        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_symbols_file ON symbols(file_path)",
            [],
        )?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_database_open_creates_schema() {
        let dir = tempdir().expect("should create temp dir");
        let db_path = dir.path().join("test.db");
        let db = Database::open(&db_path).expect("should open database");
        let conn = db.connection();

        let count: i32 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('sessions', 'messages', 'symbols')",
                [],
                |row| row.get(0),
            )
            .expect("should query table count");

        assert_eq!(count, 3, "all 3 tables should be created");
    }

    #[test]
    fn test_database_open_idempotent() {
        let dir = tempdir().expect("should create temp dir");
        let db_path = dir.path().join("test.db");

        Database::open(&db_path).expect("first open should succeed");
        Database::open(&db_path).expect("second open should also succeed");
    }
}
