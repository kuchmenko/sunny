//! Agent metadata annotations for routing and capability hints.
//!
//! These types are NOT part of the [`crate::agent::Agent`] trait (that would be a breaking
//! change). Instead, they are stored separately and used for routing decisions
//! and observability.

use std::fmt::Display;

/// Metadata annotation for an agent.
///
/// Used for routing hints and observability. NOT part of the Agent trait
/// to maintain backward compatibility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentMetadata {
    /// Execution mode: primary orchestrator or subagent
    pub mode: AgentMode,
    /// Category for grouping (e.g., "exploration", "advisor", "specialist")
    pub category: &'static str,
    /// Relative cost tier for routing decisions
    pub cost: AgentCost,
}

/// Agent execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentMode {
    /// Primary orchestrator agent
    Primary,
    /// Subagent for delegated tasks
    Subagent,
}

/// Relative cost tier for agent selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentCost {
    /// No LLM calls or minimal computation
    Free,
    /// Single LLM call or lightweight processing
    Cheap,
    /// Multiple LLM calls or heavy computation
    Expensive,
}

impl Default for AgentMetadata {
    fn default() -> Self {
        Self {
            mode: AgentMode::Subagent,
            category: "general",
            cost: AgentCost::Free,
        }
    }
}

impl Display for AgentMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?} {} {:?}", self.mode, self.category, self.cost)
    }
}

impl Display for AgentMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentMode::Primary => write!(f, "primary"),
            AgentMode::Subagent => write!(f, "subagent"),
        }
    }
}

impl Display for AgentCost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentCost::Free => write!(f, "free"),
            AgentCost::Cheap => write!(f, "cheap"),
            AgentCost::Expensive => write!(f, "expensive"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metadata_default() {
        let meta = AgentMetadata::default();
        assert_eq!(meta.mode, AgentMode::Subagent);
        assert_eq!(meta.category, "general");
        assert_eq!(meta.cost, AgentCost::Free);
    }

    #[test]
    fn test_metadata_display() {
        let meta = AgentMetadata {
            mode: AgentMode::Primary,
            category: "advisor",
            cost: AgentCost::Expensive,
        };
        let s = format!("{}", meta);
        assert!(s.contains("Primary"));
        assert!(s.contains("advisor"));
        assert!(s.contains("Expensive"));
    }
}
