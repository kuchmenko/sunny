//! Plan domain types.

use std::fmt::{Display, Formatter};
use std::str::FromStr;

use crate::error::PlanError;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlanMode {
    Quick,
    Smart,
}

impl PlanMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Quick => "quick",
            Self::Smart => "smart",
        }
    }
}

impl Display for PlanMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PlanMode {
    type Err = PlanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "quick" => Ok(Self::Quick),
            "smart" => Ok(Self::Smart),
            mode => Err(PlanError::InvalidStatus {
                status: mode.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlanStatus {
    Draft,
    Ready,
    Active,
    Completed,
    Failed,
}

impl PlanStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Ready => "ready",
            Self::Active => "active",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

impl Display for PlanStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PlanStatus {
    type Err = PlanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "draft" => Ok(Self::Draft),
            "ready" => Ok(Self::Ready),
            "active" => Ok(Self::Active),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            status => Err(PlanError::InvalidStatus {
                status: status.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Plan {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub mode: PlanMode,
    pub status: PlanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_session_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DecisionAuthor {
    User,
    Planner,
    Agent,
}

impl DecisionAuthor {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Planner => "planner",
            Self::Agent => "agent",
        }
    }
}

impl Display for DecisionAuthor {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DecisionAuthor {
    type Err = PlanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "user" => Ok(Self::User),
            "planner" => Ok(Self::Planner),
            "agent" => Ok(Self::Agent),
            author => Err(PlanError::InvalidStatus {
                status: author.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DecisionType {
    Technology,
    Scope,
    Constraint,
    TradeOff,
    Requirement,
}

impl DecisionType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Technology => "technology",
            Self::Scope => "scope",
            Self::Constraint => "constraint",
            Self::TradeOff => "tradeoff",
            Self::Requirement => "requirement",
        }
    }
}

impl Display for DecisionType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for DecisionType {
    type Err = PlanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "technology" => Ok(Self::Technology),
            "scope" => Ok(Self::Scope),
            "constraint" => Ok(Self::Constraint),
            "tradeoff" => Ok(Self::TradeOff),
            "requirement" => Ok(Self::Requirement),
            dtype => Err(PlanError::InvalidStatus {
                status: dtype.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Decision {
    pub id: String,
    pub plan_id: String,
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternatives_considered: Option<String>,
    pub decided_by: DecisionAuthor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_type: Option<DecisionType>,
    pub is_locked: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub superseded_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ConstraintType {
    MustDo,
    MustNotDo,
    Prefer,
    Avoid,
}

impl ConstraintType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MustDo => "must_do",
            Self::MustNotDo => "must_not_do",
            Self::Prefer => "prefer",
            Self::Avoid => "avoid",
        }
    }
}

impl Display for ConstraintType {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ConstraintType {
    type Err = PlanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "must_do" => Ok(Self::MustDo),
            "must_not_do" => Ok(Self::MustNotDo),
            "prefer" => Ok(Self::Prefer),
            "avoid" => Ok(Self::Avoid),
            ctype => Err(PlanError::InvalidStatus {
                status: ctype.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Constraint {
    pub id: String,
    pub plan_id: String,
    pub constraint_type: ConstraintType,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_decision_id: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GoalPriority {
    Critical,
    Important,
    NiceToHave,
}

impl GoalPriority {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Important => "important",
            Self::NiceToHave => "nice_to_have",
        }
    }
}

impl Display for GoalPriority {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for GoalPriority {
    type Err = PlanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "critical" => Ok(Self::Critical),
            "important" => Ok(Self::Important),
            "nice_to_have" => Ok(Self::NiceToHave),
            priority => Err(PlanError::InvalidStatus {
                status: priority.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GoalStatus {
    Pending,
    Achieved,
    Abandoned,
}

impl GoalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Achieved => "achieved",
            Self::Abandoned => "abandoned",
        }
    }
}

impl Display for GoalStatus {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for GoalStatus {
    type Err = PlanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "achieved" => Ok(Self::Achieved),
            "abandoned" => Ok(Self::Abandoned),
            status => Err(PlanError::InvalidStatus {
                status: status.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Goal {
    pub id: String,
    pub plan_id: String,
    pub description: String,
    pub priority: GoalPriority,
    pub status: GoalStatus,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_plan_mode_roundtrip() {
        let modes = ["quick", "smart"];
        for mode in modes {
            let parsed = PlanMode::from_str(mode).expect("mode should parse");
            assert_eq!(parsed.to_string(), mode);
        }
    }

    #[test]
    fn test_plan_status_roundtrip() {
        let statuses = ["draft", "ready", "active", "completed", "failed"];
        for status in statuses {
            let parsed = PlanStatus::from_str(status).expect("status should parse");
            assert_eq!(parsed.to_string(), status);
        }
    }

    #[test]
    fn test_decision_author_roundtrip() {
        let authors = ["user", "planner", "agent"];
        for author in authors {
            let parsed = DecisionAuthor::from_str(author).expect("author should parse");
            assert_eq!(parsed.to_string(), author);
        }
    }

    #[test]
    fn test_decision_type_roundtrip() {
        let types = [
            "technology",
            "scope",
            "constraint",
            "tradeoff",
            "requirement",
        ];
        for dtype in types {
            let parsed = DecisionType::from_str(dtype).expect("type should parse");
            assert_eq!(parsed.to_string(), dtype);
        }
    }

    #[test]
    fn test_constraint_type_roundtrip() {
        let types = ["must_do", "must_not_do", "prefer", "avoid"];
        for ctype in types {
            let parsed = ConstraintType::from_str(ctype).expect("type should parse");
            assert_eq!(parsed.to_string(), ctype);
        }
    }

    #[test]
    fn test_goal_priority_roundtrip() {
        let priorities = ["critical", "important", "nice_to_have"];
        for priority in priorities {
            let parsed = GoalPriority::from_str(priority).expect("priority should parse");
            assert_eq!(parsed.to_string(), priority);
        }
    }

    #[test]
    fn test_goal_status_roundtrip() {
        let statuses = ["pending", "achieved", "abandoned"];
        for status in statuses {
            let parsed = GoalStatus::from_str(status).expect("status should parse");
            assert_eq!(parsed.to_string(), status);
        }
    }
}
