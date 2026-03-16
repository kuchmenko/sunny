use std::sync::Arc;

use async_trait::async_trait;
use sunny_tasks::{capability_info, CapabilityScope, CapabilityStore};

#[derive(Debug, Clone)]
pub enum GateDecision {
    Allow,
    AllowAndRemember,
    Deny,
}

#[async_trait]
pub trait HumanApprovalGate: Send + Sync {
    async fn on_blocked(&self, tool_name: &str, command: &str, reason: &str) -> GateDecision;
}

pub struct AlwaysAllowGate;

#[async_trait]
impl HumanApprovalGate for AlwaysAllowGate {
    async fn on_blocked(&self, _tool_name: &str, _command: &str, _reason: &str) -> GateDecision {
        GateDecision::Allow
    }
}

pub struct AlwaysDenyGate;

#[async_trait]
impl HumanApprovalGate for AlwaysDenyGate {
    async fn on_blocked(&self, _tool_name: &str, _command: &str, _reason: &str) -> GateDecision {
        GateDecision::Deny
    }
}

pub struct CliApprovalGate {
    store: tokio::sync::Mutex<CapabilityStore>,
}

impl CliApprovalGate {
    pub fn new(store: CapabilityStore) -> Self {
        Self {
            store: tokio::sync::Mutex::new(store),
        }
    }

    fn resolve_scope(capability: &str) -> CapabilityScope {
        capability_info(capability)
            .map(|info| info.default_scope.clone())
            .unwrap_or(CapabilityScope::Session)
    }

    async fn persist_decision(
        &self,
        request_id: &str,
        capability: &str,
        approved: bool,
    ) -> GateDecision {
        let store = self.store.lock().await;

        if approved {
            let scope = Self::resolve_scope(capability);
            return match store.approve(request_id, scope) {
                Ok(_) => GateDecision::AllowAndRemember,
                Err(_) => GateDecision::Deny,
            };
        }

        let _ = store.deny(request_id);
        GateDecision::Deny
    }
}

#[async_trait]
impl HumanApprovalGate for CliApprovalGate {
    async fn on_blocked(&self, tool_name: &str, command: &str, reason: &str) -> GateDecision {
        let capability = infer_capability(tool_name, command, reason);
        let requested_rhs = infer_requested_rhs(&capability, command);

        let request = {
            let store = self.store.lock().await;
            match store.create_request(
                "cli-session",
                None,
                &capability,
                requested_rhs.as_deref(),
                Some(command),
                reason,
            ) {
                Ok(request) => request,
                Err(_) => return GateDecision::Deny,
            }
        };

        let question = format!(
            "Command requires approval ({capability}):\n{command}\nReason: {reason}\nAllow?"
        );

        let approved = match tokio::task::spawn_blocking(move || {
            inquire::Confirm::new(&question)
                .with_default(false)
                .prompt()
        })
        .await
        {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) | Err(_) => false,
        };

        self.persist_decision(&request.id, &capability, approved)
            .await
    }
}

fn infer_capability(tool_name: &str, command: &str, reason: &str) -> String {
    if let Some(capability) = parse_capability_from_reason(reason) {
        return capability;
    }

    if tool_name == "shell_exec" && command.contains('|') {
        return "shell_pipes".to_string();
    }

    tool_name.to_string()
}

fn parse_capability_from_reason(reason: &str) -> Option<String> {
    const PREFIX: &str = "capability '";
    let start = reason.find(PREFIX)? + PREFIX.len();
    let tail = &reason[start..];
    let end = tail.find('\'')?;
    Some(tail[..end].to_string())
}

fn infer_requested_rhs(capability: &str, command: &str) -> Option<Vec<String>> {
    if capability != "shell_pipes" {
        return None;
    }

    command
        .split('|')
        .nth(1)
        .and_then(|rhs| rhs.split_whitespace().next())
        .map(|rhs| vec![rhs.to_string()])
}

pub type SharedApprovalGate = Arc<dyn HumanApprovalGate>;

#[cfg(test)]
mod tests {
    use super::*;
    use sunny_tasks::{CapabilityRequestStatus, CapabilityStore};

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

    #[tokio::test]
    async fn test_always_allow_gate_returns_allow() {
        let gate = AlwaysAllowGate;
        assert!(matches!(
            gate.on_blocked("shell_exec", "curl https://example.com", "not in allowlist")
                .await,
            GateDecision::Allow
        ));
    }

    #[tokio::test]
    async fn test_always_deny_gate_returns_deny() {
        let gate = AlwaysDenyGate;
        assert!(matches!(
            gate.on_blocked("shell_exec", "curl https://example.com", "not in allowlist")
                .await,
            GateDecision::Deny
        ));
    }

    #[tokio::test]
    async fn test_approval_gate_allow_persists_capability() {
        let (store, _dir) = make_store();
        let request = store
            .create_request("session-1", None, "git_write", None, None, "need git write")
            .expect("should create request");
        let gate = CliApprovalGate::new(store);

        let decision = gate.persist_decision(&request.id, "git_write", true).await;
        assert!(matches!(decision, GateDecision::AllowAndRemember));

        let approved = gate
            .store
            .lock()
            .await
            .approved_for_session("session-1")
            .expect("should list approved requests");
        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].capability, "git_write");
        assert_eq!(approved[0].status, CapabilityRequestStatus::Approved);
    }

    #[tokio::test]
    async fn test_approval_gate_deny_no_persist() {
        let (store, _dir) = make_store();
        let request = store
            .create_request("session-1", None, "git_write", None, None, "need git write")
            .expect("should create request");
        let gate = CliApprovalGate::new(store);

        let decision = gate.persist_decision(&request.id, "git_write", false).await;
        assert!(matches!(decision, GateDecision::Deny));

        let approved = gate
            .store
            .lock()
            .await
            .approved_for_session("session-1")
            .expect("should list approved requests");
        assert!(approved.is_empty());
    }

    #[tokio::test]
    async fn test_approval_gate_remember_scope() {
        let (store, _dir) = make_store();
        let request = store
            .create_request("session-1", None, "git_write", None, None, "need git write")
            .expect("should create request");
        let gate = CliApprovalGate::new(store);

        let decision = gate.persist_decision(&request.id, "git_write", true).await;
        assert!(matches!(decision, GateDecision::AllowAndRemember));

        let audit = gate
            .store
            .lock()
            .await
            .audit_log(Some(10))
            .expect("should list audit requests");
        let approved = audit
            .into_iter()
            .find(|entry| entry.id == request.id)
            .expect("approved request should exist");
        assert_eq!(approved.scope, Some(CapabilityScope::Session));
    }
}
