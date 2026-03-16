//! Plan schema — SQLite table definitions and migrations

/// Create all plan-related tables and add plan_id column to tasks table
pub fn ensure_plan_schema(db: &sunny_store::Database) {
    db.connection()
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS plans (
                id              TEXT PRIMARY KEY,
                workspace_id    TEXT NOT NULL,
                name            TEXT NOT NULL,
                description     TEXT,
                mode            TEXT NOT NULL CHECK(mode IN ('quick', 'smart')),
                status          TEXT NOT NULL CHECK(status IN ('draft', 'ready', 'active', 'completed', 'failed')),
                root_session_id TEXT,
                created_at      TEXT NOT NULL,
                updated_at      TEXT NOT NULL,
                completed_at    TEXT,
                metadata        TEXT
            );
            CREATE TABLE IF NOT EXISTS plan_events (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                plan_id     TEXT NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
                sequence    INTEGER NOT NULL,
                event_type  TEXT NOT NULL,
                payload     TEXT NOT NULL,
                created_by  TEXT NOT NULL,
                created_at  TEXT NOT NULL,
                UNIQUE(plan_id, sequence)
            );
            CREATE TABLE IF NOT EXISTS plan_decisions (
                id                      TEXT PRIMARY KEY,
                plan_id                 TEXT NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
                decision                TEXT NOT NULL,
                rationale               TEXT,
                alternatives_considered TEXT,
                decided_by              TEXT NOT NULL,
                decision_type           TEXT,
                is_locked               INTEGER NOT NULL DEFAULT 1,
                created_at              TEXT NOT NULL,
                superseded_by           TEXT REFERENCES plan_decisions(id)
            );
            CREATE TABLE IF NOT EXISTS plan_constraints (
                id                  TEXT PRIMARY KEY,
                plan_id             TEXT NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
                constraint_type     TEXT NOT NULL CHECK(constraint_type IN ('must_do', 'must_not_do', 'prefer', 'avoid')),
                description         TEXT NOT NULL,
                source_decision_id  TEXT REFERENCES plan_decisions(id),
                created_at          TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS plan_goals (
                id          TEXT PRIMARY KEY,
                plan_id     TEXT NOT NULL REFERENCES plans(id) ON DELETE CASCADE,
                description TEXT NOT NULL,
                priority    TEXT NOT NULL CHECK(priority IN ('critical', 'important', 'nice_to_have')),
                status      TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending', 'achieved', 'abandoned')),
                created_at  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_plans_workspace_status ON plans(workspace_id, status);
            CREATE INDEX IF NOT EXISTS idx_plan_events_plan_seq ON plan_events(plan_id, sequence);
            CREATE INDEX IF NOT EXISTS idx_plan_decisions_plan ON plan_decisions(plan_id);"
        )
        .expect("should create plan schema");

    // Idempotent migration: add plan_id column to tasks table if it doesn't exist
    let _ = db.connection().execute(
        "ALTER TABLE tasks ADD COLUMN plan_id TEXT REFERENCES plans(id)",
        [],
    );

    let _ = db.connection().execute(
        "CREATE INDEX IF NOT EXISTS idx_tasks_plan_id ON tasks(plan_id)",
        [],
    );
}
