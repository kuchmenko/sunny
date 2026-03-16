use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, RwLock};

use sunny_core::tool::{CapabilityChecker, ToolError};
use sunny_tasks::{
    CapabilityRequest, CapabilityRequestStatus, CapabilityScope, CapabilityStore,
    CreateVerifyCommandInput, TaskStore, WorkspaceDetector, HARD_BLOCKED_CAPABILITIES,
};

pub type ActiveCapabilities = Arc<RwLock<HashMap<String, HashSet<String>>>>;

pub struct TaskCapabilityChecker {
    caps: ActiveCapabilities,
    session_id: String,
}

impl TaskCapabilityChecker {
    pub fn new(caps: ActiveCapabilities, session_id: String) -> Self {
        Self { caps, session_id }
    }
}

impl CapabilityChecker for TaskCapabilityChecker {
    fn is_granted(&self, capability: &str, pattern: Option<&str>) -> bool {
        if is_hard_blocked(capability) {
            return false;
        }

        let caps = self.caps.read().expect("capability lock poisoned");
        match caps.get(capability) {
            None => false,
            Some(patterns) => {
                if patterns.is_empty() {
                    true
                } else {
                    pattern.is_some_and(|p| patterns.contains(p))
                }
            }
        }
    }

    fn denied_hint(&self, capability: &str, pattern: Option<&str>) -> String {
        let pat = pattern.map_or(String::new(), |p| format!(" (pattern: {p})"));
        if is_hard_blocked(capability) {
            format!("capability '{capability}'{pat} is hard-blocked")
        } else {
            format!(
                "capability '{capability}'{pat} not granted in session {}",
                self.session_id
            )
        }
    }
}

pub fn is_hard_blocked(capability: &str) -> bool {
    HARD_BLOCKED_CAPABILITIES.contains(&capability)
}

fn capability_scope_allows_session(request: &CapabilityRequest, session_id: &str) -> bool {
    match request.scope.clone().unwrap_or(CapabilityScope::Session) {
        CapabilityScope::Invocation => false,
        CapabilityScope::Session => request.session_id == session_id,
        CapabilityScope::Workspace | CapabilityScope::Global => true,
    }
}

fn extend_active_capabilities(
    map: &mut HashMap<String, HashSet<String>>,
    request: CapabilityRequest,
    session_id: &str,
) {
    if !matches!(request.status, CapabilityRequestStatus::Approved)
        || !capability_scope_allows_session(&request, session_id)
    {
        return;
    }

    let patterns: HashSet<String> = request
        .requested_rhs
        .unwrap_or_default()
        .into_iter()
        .collect();
    map.entry(request.capability).or_default().extend(patterns);
}

pub fn build_active_capabilities(
    session_id: &str,
    workspace_root: Option<&Path>,
) -> ActiveCapabilities {
    let caps: ActiveCapabilities = Arc::new(RwLock::new(HashMap::new()));

    if let Some(root) = workspace_root {
        if let Ok(policy) = sunny_tasks::PolicyFile::load(root) {
            let mut map = caps.write().expect("capability lock poisoned");
            for (name, entry) in &policy.capabilities {
                if entry.policy == "workspace" || entry.policy == "global" {
                    let patterns: HashSet<String> = entry
                        .allowed_rhs
                        .as_ref()
                        .map_or_else(HashSet::new, |vals| vals.iter().cloned().collect());
                    map.insert(name.clone(), patterns);
                }
            }
        }
    }

    if let Ok(store) = CapabilityStore::open_default() {
        if let Ok(approved) = store.audit_log(Some(u32::MAX)) {
            let mut map = caps.write().expect("capability lock poisoned");
            for req in approved {
                extend_active_capabilities(&mut map, req, session_id);
            }
        }
    }

    caps
}

pub(super) fn extract_str<'a>(
    value: &'a serde_json::Value,
    key: &str,
) -> Result<&'a str, ToolError> {
    value[key]
        .as_str()
        .ok_or_else(|| ToolError::ExecutionFailed {
            source: Box::new(std::io::Error::other(format!("missing '{key}' argument"))),
        })
}

pub(super) fn extract_string_array(
    value: &serde_json::Value,
    key: &str,
) -> Result<Vec<String>, ToolError> {
    let array = value[key]
        .as_array()
        .ok_or_else(|| tool_exec_err(std::io::Error::other(format!("missing '{key}' argument"))))?;

    let mut items = Vec::with_capacity(array.len());
    for item in array {
        let Some(text) = item.as_str() else {
            return Err(tool_exec_err(std::io::Error::other(format!(
                "'{key}' must contain only strings"
            ))));
        };
        items.push(text.to_string());
    }
    Ok(items)
}

pub(super) fn extract_optional_string_array(
    value: &serde_json::Value,
    key: &str,
) -> Option<Vec<String>> {
    value[key].as_array().map(|array| {
        array
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect()
    })
}

pub(super) fn extract_verify_commands(
    value: &serde_json::Value,
) -> Result<Vec<CreateVerifyCommandInput>, ToolError> {
    let Some(array) = value.as_array() else {
        return Ok(Vec::new());
    };

    let mut commands = Vec::with_capacity(array.len());
    for item in array {
        let Some(command) = item.get("command").and_then(serde_json::Value::as_str) else {
            return Err(tool_exec_err(std::io::Error::other(
                "verify_commands[].command is required",
            )));
        };
        let Some(expected_exit_code) = item
            .get("expected_exit_code")
            .and_then(serde_json::Value::as_i64)
        else {
            return Err(tool_exec_err(std::io::Error::other(
                "verify_commands[].expected_exit_code is required",
            )));
        };
        let Some(timeout_secs) = item.get("timeout_secs").and_then(serde_json::Value::as_u64)
        else {
            return Err(tool_exec_err(std::io::Error::other(
                "verify_commands[].timeout_secs is required",
            )));
        };

        commands.push(CreateVerifyCommandInput {
            command: command.to_string(),
            expected_exit_code: expected_exit_code as i32,
            timeout_secs: timeout_secs as u32,
        });
    }
    Ok(commands)
}

pub(super) fn run_blocking_tool<F>(op: F) -> Result<String, ToolError>
where
    F: FnOnce() -> Result<String, ToolError> + Send + 'static,
{
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(tokio::task::spawn_blocking(op))
    })
    .map_err(|error| {
        tool_exec_err(std::io::Error::other(format!(
            "blocking task join error: {error}"
        )))
    })?
}

pub(super) fn tool_exec_err<E>(error: E) -> ToolError
where
    E: std::error::Error + Send + Sync + 'static,
{
    ToolError::ExecutionFailed {
        source: Box::new(error),
    }
}

pub(super) fn parse_delegation_entry(value: &str) -> (String, Vec<String>) {
    if let Some((capability, rhs)) = value.split_once(':') {
        let patterns = rhs
            .split(',')
            .map(str::trim)
            .filter(|pattern| !pattern.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        (capability.to_string(), patterns)
    } else {
        (value.to_string(), Vec::new())
    }
}

pub(super) fn resolve_root_session_id(
    store: &TaskStore,
    parent_id: Option<&str>,
    session_id: &str,
) -> Result<String, ToolError> {
    if let Some(parent_task_id) = parent_id {
        match store.get_task(parent_task_id) {
            Ok(Some(parent)) => Ok(parent.root_session_id),
            _ => Ok(session_id.to_string()),
        }
    } else {
        Ok(session_id.to_string())
    }
}

pub(super) fn detect_workspace_store() -> Result<(TaskStore, String), ToolError> {
    let store = TaskStore::open_default().map_err(tool_exec_err)?;
    let git_root = WorkspaceDetector::detect_cwd().ok_or_else(|| {
        tool_exec_err(std::io::Error::other(
            "no git workspace found from current directory",
        ))
    })?;
    let git_root_str = git_root
        .to_str()
        .ok_or_else(|| tool_exec_err(std::io::Error::other("workspace path is not valid UTF-8")))?;

    Ok((store, git_root_str.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(
        capability: &str,
        scope: Option<CapabilityScope>,
        session_id: &str,
    ) -> CapabilityRequest {
        serde_json::from_value(serde_json::json!({
            "id": "req-1",
            "session_id": session_id,
            "task_id": null,
            "capability": capability,
            "requested_rhs": null,
            "example_command": null,
            "reason": "test",
            "status": "Approved",
            "scope": scope,
            "requested_at": "2026-03-16T00:00:00Z",
            "resolved_at": null,
            "resolved_by": null
        }))
        .expect("request should deserialize")
    }

    #[test]
    fn test_capability_scope_session_scope_only_valid_for_current_session() {
        let mut map = HashMap::new();

        extend_active_capabilities(
            &mut map,
            request("git_write", Some(CapabilityScope::Session), "session-a"),
            "session-b",
        );

        assert!(!map.contains_key("git_write"));
    }

    #[test]
    fn test_capability_scope_hard_blocked_capability_always_denied() {
        assert!(is_hard_blocked("shell_arbitrary"));
    }
}
