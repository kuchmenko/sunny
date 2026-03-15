use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum GateDecision {
    Allow,
    AllowAndRemember,
    Deny,
}

pub trait HumanApprovalGate: Send + Sync {
    fn on_blocked(&self, tool_name: &str, command: &str, reason: &str) -> GateDecision;
}

pub struct AlwaysAllowGate;

impl HumanApprovalGate for AlwaysAllowGate {
    fn on_blocked(&self, _tool_name: &str, _command: &str, _reason: &str) -> GateDecision {
        GateDecision::Allow
    }
}

pub struct AlwaysDenyGate;

impl HumanApprovalGate for AlwaysDenyGate {
    fn on_blocked(&self, _tool_name: &str, _command: &str, _reason: &str) -> GateDecision {
        GateDecision::Deny
    }
}

pub type SharedApprovalGate = Arc<dyn HumanApprovalGate>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_always_allow_gate_returns_allow() {
        let gate = AlwaysAllowGate;
        assert!(matches!(
            gate.on_blocked("shell_exec", "curl https://example.com", "not in allowlist"),
            GateDecision::Allow
        ));
    }

    #[test]
    fn test_always_deny_gate_returns_deny() {
        let gate = AlwaysDenyGate;
        assert!(matches!(
            gate.on_blocked("shell_exec", "curl https://example.com", "not in allowlist"),
            GateDecision::Deny
        ));
    }
}
