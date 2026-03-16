//! Category system: Role × Effort dimensions for task classification.

use serde::{Deserialize, Serialize};
use std::fmt::{Display, Formatter};
use std::str::FromStr;

use crate::error::PlanError;

/// Agent role dimension: what kind of work the agent performs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    Executor,
    Investigator,
    Planner,
    Verifier,
}

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Executor => "executor",
            Self::Investigator => "investigator",
            Self::Planner => "planner",
            Self::Verifier => "verifier",
        }
    }
}

impl Display for Role {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Role {
    type Err = PlanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "executor" => Ok(Self::Executor),
            "investigator" => Ok(Self::Investigator),
            "planner" => Ok(Self::Planner),
            "verifier" => Ok(Self::Verifier),
            role => Err(PlanError::ValidationFailed {
                reason: format!("invalid role: {}", role),
            }),
        }
    }
}

/// Effort dimension: computational complexity and thinking budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Effort {
    Low,
    Moderate,
    High,
    Critical,
}

impl Effort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Moderate => "moderate",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    /// Returns the thinking token budget for this effort level.
    ///
    /// - Low → None (no extended thinking)
    /// - Moderate → Some(4000)
    /// - High → Some(16000)
    /// - Critical → Some(32000)
    pub fn thinking_budget(&self) -> Option<u32> {
        match self {
            Self::Low => None,
            Self::Moderate => Some(4000),
            Self::High => Some(16000),
            Self::Critical => Some(32000),
        }
    }
}

impl Display for Effort {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Effort {
    type Err = PlanError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(Self::Low),
            "moderate" => Ok(Self::Moderate),
            "high" => Ok(Self::High),
            "critical" => Ok(Self::Critical),
            effort => Err(PlanError::ValidationFailed {
                reason: format!("invalid effort: {}", effort),
            }),
        }
    }
}

/// Task category: combination of role and effort dimensions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskCategory {
    pub role: Role,
    pub effort: Effort,
}

/// Resolved category: model assignment and thinking budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedCategory {
    pub model: String,
    pub thinking_budget: Option<u32>,
}

/// Category configuration: maps role×effort to model and thinking budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryConfig {
    /// Default model for all roles/efforts.
    #[serde(default = "default_model")]
    pub default_model: String,
}

fn default_model() -> String {
    "claude-sonnet-4-6".into()
}

impl Default for CategoryConfig {
    fn default() -> Self {
        Self {
            default_model: default_model(),
        }
    }
}

impl CategoryConfig {
    /// Resolve role+effort to a model and thinking budget.
    pub fn resolve(&self, category: &TaskCategory) -> ResolvedCategory {
        ResolvedCategory {
            model: self.default_model.clone(),
            thinking_budget: category.effort.thinking_budget(),
        }
    }
}

/// Backward-compatible mapping: old category strings → Role×Effort.
///
/// Maps legacy category names to the new Role×Effort system:
/// - "quick" → Executor + Low
/// - "standard" → Executor + Moderate
/// - "deep" → Executor + High
/// - unknown → Executor + Low (default)
pub fn resolve_legacy_category(category: &str) -> TaskCategory {
    match category {
        "quick" => TaskCategory {
            role: Role::Executor,
            effort: Effort::Low,
        },
        "standard" => TaskCategory {
            role: Role::Executor,
            effort: Effort::Moderate,
        },
        "deep" => TaskCategory {
            role: Role::Executor,
            effort: Effort::High,
        },
        _ => TaskCategory {
            role: Role::Executor,
            effort: Effort::Low,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effort_thinking_budget_maps_correctly() {
        assert_eq!(Effort::Low.thinking_budget(), None);
        assert_eq!(Effort::Moderate.thinking_budget(), Some(4000));
        assert_eq!(Effort::High.thinking_budget(), Some(16000));
        assert_eq!(Effort::Critical.thinking_budget(), Some(32000));
    }

    #[test]
    fn test_legacy_category_mapping() {
        let quick = resolve_legacy_category("quick");
        assert_eq!(quick.role, Role::Executor);
        assert_eq!(quick.effort, Effort::Low);

        let standard = resolve_legacy_category("standard");
        assert_eq!(standard.role, Role::Executor);
        assert_eq!(standard.effort, Effort::Moderate);

        let deep = resolve_legacy_category("deep");
        assert_eq!(deep.role, Role::Executor);
        assert_eq!(deep.effort, Effort::High);

        let unknown = resolve_legacy_category("unknown");
        assert_eq!(unknown.role, Role::Executor);
        assert_eq!(unknown.effort, Effort::Low);
    }

    #[test]
    fn test_role_display_and_fromstr() {
        assert_eq!(Role::Executor.to_string(), "executor");
        assert_eq!(Role::Investigator.to_string(), "investigator");
        assert_eq!(Role::Planner.to_string(), "planner");
        assert_eq!(Role::Verifier.to_string(), "verifier");

        assert_eq!("executor".parse::<Role>().unwrap(), Role::Executor);
        assert_eq!("investigator".parse::<Role>().unwrap(), Role::Investigator);
        assert_eq!("planner".parse::<Role>().unwrap(), Role::Planner);
        assert_eq!("verifier".parse::<Role>().unwrap(), Role::Verifier);
    }

    #[test]
    fn test_effort_display_and_fromstr() {
        assert_eq!(Effort::Low.to_string(), "low");
        assert_eq!(Effort::Moderate.to_string(), "moderate");
        assert_eq!(Effort::High.to_string(), "high");
        assert_eq!(Effort::Critical.to_string(), "critical");

        assert_eq!("low".parse::<Effort>().unwrap(), Effort::Low);
        assert_eq!("moderate".parse::<Effort>().unwrap(), Effort::Moderate);
        assert_eq!("high".parse::<Effort>().unwrap(), Effort::High);
        assert_eq!("critical".parse::<Effort>().unwrap(), Effort::Critical);
    }

    #[test]
    fn test_category_config_resolve() {
        let config = CategoryConfig::default();
        let category = TaskCategory {
            role: Role::Executor,
            effort: Effort::High,
        };

        let resolved = config.resolve(&category);
        assert_eq!(resolved.model, "claude-sonnet-4-6");
        assert_eq!(resolved.thinking_budget, Some(16000));
    }

    #[test]
    fn test_category_config_resolve_low_effort() {
        let config = CategoryConfig::default();
        let category = TaskCategory {
            role: Role::Executor,
            effort: Effort::Low,
        };

        let resolved = config.resolve(&category);
        assert_eq!(resolved.model, "claude-sonnet-4-6");
        assert_eq!(resolved.thinking_budget, None);
    }
}
