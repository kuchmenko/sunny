use std::path::PathBuf;
use std::sync::Arc;

use crate::tool_loop::ToolExecutor;
use sunny_core::tool::ToolPolicy;

mod definitions;
mod executor;
mod helpers;
mod task_handlers;

pub use definitions::build_tool_definitions;
pub use executor::build_tool_executor_with_capabilities;
pub use helpers::{build_active_capabilities, ActiveCapabilities, TaskCapabilityChecker};

/// Build the tool executor that dispatches tool calls to the correct implementation.
///
/// All file tools are sandboxed to `root` via `PathGuard`.
pub fn build_tool_executor(
    root: PathBuf,
    task_id: Option<String>,
    session_id: Option<String>,
) -> Arc<ToolExecutor> {
    build_tool_executor_with_capabilities(root, None, task_id, session_id, None)
}

/// Build the tool policy allowing all registered tools.
/// Uses an empty deny-list so all tools are permitted by the policy.
/// Unknown tool names are handled in the executor by returning an error.
pub fn build_tool_policy() -> ToolPolicy {
    ToolPolicy::deny_list(&[])
}

#[cfg(test)]
mod tests {
    use super::helpers::resolve_root_session_id;
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::RwLock;
    use sunny_core::tool::CapabilityChecker;
    use sunny_store::Database;
    use sunny_tasks::{CreateTaskInput, TaskStore};

    fn make_task_store() -> (TaskStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let db = Database::open(dir.path().join("test.db").as_path()).expect("should open db");
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
                );",
            )
            .expect("should create task schema");
        (TaskStore::new(db), dir)
    }

    #[test]
    fn test_build_tool_definitions_count() {
        let defs = build_tool_definitions();
        assert_eq!(defs.len(), 28, "expected 28 tool definitions");
    }

    #[test]
    fn test_build_tool_definitions_names() {
        let defs = build_tool_definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        let expected = [
            "fs_read",
            "fs_scan",
            "fs_write",
            "fs_edit",
            "fs_glob",
            "shell_exec",
            "text_grep",
            "grep_files",
            "git_log",
            "git_diff",
            "git_status",
            "git_commit",
            "git_branch",
            "git_checkout",
            "lsp_goto_definition",
            "lsp_find_references",
            "lsp_diagnostics",
            "lsp_symbols",
            "lsp_rename",
            "interview",
            "codebase_search",
            "task_create",
            "task_list",
            "task_get",
            "task_complete",
            "task_fail",
            "task_ask_human",
            "task_claim_paths",
        ];
        for name in &expected {
            assert!(names.contains(name), "missing tool: {name}");
        }
    }

    #[test]
    fn test_build_tool_definitions_schemas_non_empty() {
        let defs = build_tool_definitions();
        for def in &defs {
            assert!(!def.name.is_empty(), "tool name empty");
            assert!(
                !def.description.is_empty(),
                "tool description empty for {}",
                def.name
            );
        }
    }

    #[test]
    fn test_build_tool_policy_allows_all() {
        let policy = build_tool_policy();
        let allowed = [
            "fs_read",
            "fs_scan",
            "fs_write",
            "fs_edit",
            "fs_glob",
            "shell_exec",
            "text_grep",
            "grep_files",
            "git_log",
            "git_diff",
            "git_status",
            "git_commit",
            "git_branch",
            "git_checkout",
            "lsp_goto_definition",
            "lsp_find_references",
            "lsp_diagnostics",
            "lsp_symbols",
            "lsp_rename",
            "interview",
            "codebase_search",
            "task_create",
            "task_list",
            "task_get",
            "task_complete",
            "task_fail",
            "task_ask_human",
            "task_claim_paths",
        ];
        for name in &allowed {
            assert!(policy.is_allowed(name), "policy should allow {name}");
        }
    }

    #[test]
    fn test_unknown_tool_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let executor = build_tool_executor(dir.path().to_path_buf(), None, None);
        let result = executor("id1", "unknown_tool", "{}", 0);
        assert!(result.is_err(), "unknown tool should return error");
    }

    #[test]
    fn test_task_capability_checker_grants_when_present() {
        let mut caps = HashMap::new();
        caps.insert("git_write".to_string(), HashSet::new());
        let checker =
            TaskCapabilityChecker::new(Arc::new(RwLock::new(caps)), "session-1".to_string());

        assert!(checker.is_granted("git_write", None));
    }

    #[test]
    fn test_task_capability_checker_denies_when_absent() {
        let checker = TaskCapabilityChecker::new(
            Arc::new(RwLock::new(HashMap::new())),
            "session-1".to_string(),
        );

        assert!(!checker.is_granted("git_write", None));
    }

    #[test]
    fn test_task_capability_checker_grants_pattern_match() {
        let mut caps = HashMap::new();
        caps.insert(
            "shell_pipes".to_string(),
            HashSet::from(["tail".to_string(), "grep".to_string()]),
        );
        let checker =
            TaskCapabilityChecker::new(Arc::new(RwLock::new(caps)), "session-1".to_string());

        assert!(checker.is_granted("shell_pipes", Some("tail")));
        assert!(!checker.is_granted("shell_pipes", Some("wc")));
    }

    #[test]
    fn test_task_create_stamps_root_session_id() {
        let (store, _dir) = make_task_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("should create workspace");
        let task = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id,
                parent_id: None,
                title: "child".to_string(),
                description: "task from chat session".to_string(),
                created_by: "agent:chat-session:".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: resolve_root_session_id(&store, None, "chat-session")
                    .expect("should resolve root session"),
                metadata: None,
            })
            .expect("should create task");

        assert_eq!(task.root_session_id, "chat-session");
    }

    #[test]
    fn test_task_create_inherits_parent_root_session_id() {
        let (store, _dir) = make_task_store();
        let workspace = store
            .find_or_create_workspace("/tmp/repo")
            .expect("should create workspace");

        let parent = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id.clone(),
                parent_id: None,
                title: "parent".to_string(),
                description: "root task".to_string(),
                created_by: "agent:root-session:".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: "root-session".to_string(),
                metadata: None,
            })
            .expect("should create parent");

        let child = store
            .create_task(CreateTaskInput {
                workspace_id: workspace.id,
                parent_id: Some(parent.id.clone()),
                title: "child".to_string(),
                description: "child task".to_string(),
                created_by: "agent:other-session:parent".to_string(),
                priority: 0,
                max_retries: 3,
                dep_ids: vec![],
                accept_criteria: None,
                delegate_capabilities: vec![],
                root_session_id: resolve_root_session_id(
                    &store,
                    Some(parent.id.as_str()),
                    "other-session",
                )
                .expect("should inherit root session"),
                metadata: None,
            })
            .expect("should create child");

        assert_eq!(child.root_session_id, "root-session");
    }
}
