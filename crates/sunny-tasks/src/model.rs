use std::fmt::{Display, Formatter};
use std::str::FromStr;

use crate::TaskError;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TaskStatus {
    Pending,
    Running,
    BlockedHuman,
    Completed,
    Failed,
    Cancelled,
    Suspended,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::BlockedHuman => "blocked_human",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Suspended => "suspended",
        }
    }
}

impl Display for TaskStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TaskStatus {
    type Err = TaskError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "blocked_human" => Ok(Self::BlockedHuman),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "suspended" => Ok(Self::Suspended),
            status => Err(TaskError::InvalidStatus {
                status: status.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Task {
    pub id: String,
    pub workspace_id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub description: String,
    pub status: TaskStatus,
    pub session_id: Option<String>,
    pub created_by: String,
    pub priority: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    pub result_diff: Option<String>,
    pub result_summary: Option<String>,
    pub result_files: Option<Vec<String>>,
    pub result_verify: Option<String>,
    pub error: Option<String>,
    pub retry_count: i32,
    pub max_retries: i32,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Workspace {
    pub id: String,
    pub git_root: String,
    pub name: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AcceptCriteria {
    pub id: i64,
    pub task_id: String,
    pub description: String,
    pub requires_human_approval: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VerifyCommand {
    pub id: i64,
    pub criteria_id: i64,
    pub command: String,
    pub expected_exit_code: i32,
    pub timeout_secs: u32,
    pub seq: i32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HumanQuestion {
    pub id: String,
    pub task_id: String,
    pub question: String,
    pub context: Option<String>,
    pub options: Option<Vec<String>>,
    pub answer: Option<String>,
    pub asked_at: chrono::DateTime<chrono::Utc>,
    pub answered_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskPathClaim {
    pub task_id: String,
    pub path_pattern: String,
    pub claim_type: String,
}

#[derive(Debug, Clone)]
pub struct CreateTaskInput {
    pub workspace_id: String,
    pub parent_id: Option<String>,
    pub title: String,
    pub description: String,
    pub created_by: String,
    pub priority: i32,
    pub max_retries: i32,
    pub dep_ids: Vec<String>,
    pub accept_criteria: Option<CreateAcceptCriteriaInput>,
    pub delegate_capabilities: Vec<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct CreateAcceptCriteriaInput {
    pub description: String,
    pub requires_human_approval: bool,
    pub verify_commands: Vec<CreateVerifyCommandInput>,
}

#[derive(Debug, Clone)]
pub struct CreateVerifyCommandInput {
    pub command: String,
    pub expected_exit_code: i32,
    pub timeout_secs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CapabilityRisk {
    Low,
    Medium,
    HardBlocked,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CapabilityScope {
    Invocation,
    Session,
    Workspace,
    Global,
}

impl std::fmt::Display for CapabilityScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invocation => write!(f, "invocation"),
            Self::Session => write!(f, "session"),
            Self::Workspace => write!(f, "workspace"),
            Self::Global => write!(f, "global"),
        }
    }
}

impl std::str::FromStr for CapabilityScope {
    type Err = crate::error::TaskError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "invocation" => Ok(Self::Invocation),
            "session" => Ok(Self::Session),
            "workspace" => Ok(Self::Workspace),
            "global" => Ok(Self::Global),
            other => Err(crate::error::TaskError::InvalidStatus {
                status: other.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CapabilityRequestStatus {
    Pending,
    Approved,
    Denied,
}

impl std::fmt::Display for CapabilityRequestStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Approved => write!(f, "approved"),
            Self::Denied => write!(f, "denied"),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CapabilityRequest {
    pub id: String,
    pub session_id: String,
    pub task_id: Option<String>,
    pub capability: String,
    pub requested_rhs: Option<Vec<String>>,
    pub example_command: Option<String>,
    pub reason: String,
    pub status: CapabilityRequestStatus,
    pub scope: Option<CapabilityScope>,
    pub requested_at: chrono::DateTime<chrono::Utc>,
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,
    pub resolved_by: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CapabilityInfo {
    pub name: &'static str,
    pub description: &'static str,
    pub risk: CapabilityRisk,
    pub default_scope: CapabilityScope,
}

pub const HARD_BLOCKED_CAPABILITIES: &[&str] = &["write_outside_workspace", "shell_arbitrary"];

pub const CAPABILITY_REGISTRY: &[CapabilityInfo] = &[
    CapabilityInfo {
        name: "shell_pipes",
        description: "Allow piped commands where both sides are in the command allowlist",
        risk: CapabilityRisk::Low,
        default_scope: CapabilityScope::Workspace,
    },
    CapabilityInfo {
        name: "extended_timeout",
        description: "Allow shell commands with timeout beyond the default 30s",
        risk: CapabilityRisk::Low,
        default_scope: CapabilityScope::Workspace,
    },
    CapabilityInfo {
        name: "git_write",
        description: "Allow git write operations: commit, push, checkout, branch, merge",
        risk: CapabilityRisk::Medium,
        default_scope: CapabilityScope::Session,
    },
    CapabilityInfo {
        name: "install_packages",
        description: "Allow package installation: cargo add, npm install, pip install",
        risk: CapabilityRisk::Medium,
        default_scope: CapabilityScope::Session,
    },
];

pub fn capability_info(name: &str) -> Option<&'static CapabilityInfo> {
    CAPABILITY_REGISTRY.iter().find(|c| c.name == name)
}

pub fn is_hard_blocked(capability: &str) -> bool {
    HARD_BLOCKED_CAPABILITIES.contains(&capability)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_status_roundtrip_from_str() {
        let statuses = [
            "pending",
            "running",
            "blocked_human",
            "completed",
            "failed",
            "cancelled",
            "suspended",
        ];

        for status in statuses {
            let parsed = TaskStatus::from_str(status).expect("status should parse");
            assert_eq!(parsed.to_string(), status);
        }
    }

    #[test]
    fn test_task_status_display() {
        assert_eq!(TaskStatus::Pending.to_string(), "pending");
        assert_eq!(TaskStatus::Running.to_string(), "running");
        assert_eq!(TaskStatus::BlockedHuman.to_string(), "blocked_human");
        assert_eq!(TaskStatus::Completed.to_string(), "completed");
        assert_eq!(TaskStatus::Failed.to_string(), "failed");
        assert_eq!(TaskStatus::Cancelled.to_string(), "cancelled");
        assert_eq!(TaskStatus::Suspended.to_string(), "suspended");
    }

    #[test]
    fn test_task_status_suspended_as_str() {
        assert_eq!(TaskStatus::Suspended.as_str(), "suspended");
    }

    #[test]
    fn test_task_status_suspended_from_str() {
        let parsed = TaskStatus::from_str("suspended").expect("suspended should parse");
        assert_eq!(parsed, TaskStatus::Suspended);
    }

    #[test]
    fn test_task_status_suspended_display_roundtrip() {
        let suspended = TaskStatus::Suspended;
        assert_eq!(suspended.to_string(), "suspended");
        let reparsed = TaskStatus::from_str(&suspended.to_string()).expect("should roundtrip");
        assert_eq!(reparsed, suspended);
    }

    #[test]
    fn test_capability_scope_display_roundtrip() {
        let scopes = ["invocation", "session", "workspace", "global"];
        for scope in scopes {
            let parsed = CapabilityScope::from_str(scope).expect("scope should parse");
            assert_eq!(parsed.to_string(), scope);
        }
    }

    #[test]
    fn test_is_hard_blocked_returns_true_for_blocked() {
        assert!(is_hard_blocked("write_outside_workspace"));
    }

    #[test]
    fn test_is_hard_blocked_returns_false_for_known() {
        assert!(!is_hard_blocked("shell_pipes"));
    }

    #[test]
    fn test_capability_info_lookup_returns_correct_risk() {
        let info = capability_info("git_write").expect("capability should exist");
        assert_eq!(info.risk, CapabilityRisk::Medium);
    }
}
