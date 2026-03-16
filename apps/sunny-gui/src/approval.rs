//! GuiApprovalGate — non-blocking permission dialogs for the GUI.
//!
//! When the agent requests capability approval, `GuiApprovalGate::on_blocked`
//! sends an `ApprovalRequest` to the GUI thread via an mpsc channel, then
//! blocks on a oneshot channel awaiting the user's response.
//!
//! The GUI renders a modal dialog and responds via the shared `PendingApprovals` map.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::warn;
use uuid::Uuid;

use sunny_boys::{GateDecision, HumanApprovalGate};

/// A pending approval request sent to the GUI for display.
#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    /// Unique request ID (used to route the response back).
    pub id: String,
    /// Tool name requesting the capability.
    pub tool: String,
    /// Command or action being requested.
    pub command: String,
    /// Reason the tool is requesting this capability.
    pub reason: String,
}

/// Shared map of pending approval response channels.
///
/// The GUI holds an `Arc` clone of this map. When the user approves or denies
/// a request, the GUI removes the entry by ID and sends the response.
pub type PendingApprovals = Arc<Mutex<HashMap<String, oneshot::Sender<bool>>>>;

/// Approval gate that routes capability requests through the GUI.
///
/// `on_blocked` sends a request to the GUI via `tx` and awaits a bool response.
/// `true` maps to [`GateDecision::Allow`], `false` maps to [`GateDecision::Deny`].
pub struct GuiApprovalGate {
    /// Channel to send approval requests to the GUI.
    pub tx: mpsc::Sender<ApprovalRequest>,
    /// Shared map of pending oneshot response channels, keyed by request ID.
    pub pending: PendingApprovals,
}

impl GuiApprovalGate {
    /// Create a new `GuiApprovalGate`.
    ///
    /// Returns the gate and the receiver end of the approval request channel.
    /// The caller (agent bridge) holds the gate; the GUI receives requests
    /// from `rx`.
    pub fn new() -> (Self, mpsc::Receiver<ApprovalRequest>) {
        let (tx, rx) = mpsc::channel(16);
        let gate = Self {
            tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        (gate, rx)
    }

    /// Returns a clone of the shared pending approvals map.
    ///
    /// Pass this to `SunnyApp` so the GUI can send responses.
    pub fn pending_approvals(&self) -> PendingApprovals {
        Arc::clone(&self.pending)
    }
}

#[async_trait]
impl HumanApprovalGate for GuiApprovalGate {
    async fn on_blocked(&self, tool_name: &str, command: &str, reason: &str) -> GateDecision {
        let id = Uuid::new_v4().to_string();

        // Register response channel BEFORE sending the request to avoid a race.
        let (resp_tx, resp_rx) = oneshot::channel::<bool>();
        self.pending.lock().await.insert(id.clone(), resp_tx);

        let request = ApprovalRequest {
            id: id.clone(),
            tool: tool_name.to_string(),
            command: command.to_string(),
            reason: reason.to_string(),
        };

        // Send request to GUI. If the channel is closed, deny.
        if self.tx.send(request).await.is_err() {
            warn!(id, "approval channel closed, denying request");
            self.pending.lock().await.remove(&id);
            return GateDecision::Deny;
        }

        // Await user response from GUI.
        match resp_rx.await {
            Ok(true) => GateDecision::Allow,
            Ok(false) => GateDecision::Deny,
            Err(_) => {
                warn!(id, "approval response channel dropped, denying request");
                GateDecision::Deny
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_approval_gate_approve_returns_allow() {
        let (gate, mut rx) = GuiApprovalGate::new();
        let pending = gate.pending_approvals();

        // Spawn a task that simulates the GUI approving the request.
        tokio::spawn(async move {
            if let Some(req) = rx.recv().await {
                let mut pending = pending.lock().await;
                if let Some(tx) = pending.remove(&req.id) {
                    let _ = tx.send(true);
                }
            }
        });

        let decision = gate
            .on_blocked("shell_exec", "ls /tmp", "listing files")
            .await;
        assert!(
            matches!(decision, GateDecision::Allow),
            "approval should map to GateDecision::Allow"
        );
    }

    #[tokio::test]
    async fn test_approval_gate_deny_returns_deny() {
        let (gate, mut rx) = GuiApprovalGate::new();
        let pending = gate.pending_approvals();

        tokio::spawn(async move {
            if let Some(req) = rx.recv().await {
                let mut pending = pending.lock().await;
                if let Some(tx) = pending.remove(&req.id) {
                    let _ = tx.send(false);
                }
            }
        });

        let decision = gate
            .on_blocked("shell_exec", "rm -rf /", "dangerous op")
            .await;
        assert!(
            matches!(decision, GateDecision::Deny),
            "denial should map to GateDecision::Deny"
        );
    }

    #[tokio::test]
    async fn test_approval_gate_channel_closed_denies() {
        let (gate, rx) = GuiApprovalGate::new();
        // Drop the receiver immediately to simulate a closed GUI.
        drop(rx);

        let decision = gate
            .on_blocked("fs_write", "write /etc/passwd", "reason")
            .await;
        assert!(
            matches!(decision, GateDecision::Deny),
            "closed channel should deny"
        );
    }
}
