use crate::orchestrator::intent::PlanPolicy;
use std::str::FromStr;
use thiserror::Error;

/// Execution profile that maps to planning constraints.
///
/// Defines three predefined profiles (Low, Medium, High) that each map to specific
/// `PlanPolicy` constraints. Used for CLI configuration and runtime planning decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExecutionProfile {
    /// Minimal planning: depth=1, steps=4, retries=2
    Low,
    /// Balanced planning (default): depth=2, steps=16, retries=2
    #[default]
    Medium,
    /// Aggressive planning: depth=3, steps=32, retries=2
    High,
}

impl ExecutionProfile {
    /// Convert profile to corresponding `PlanPolicy` constraints.
    pub fn to_policy(&self) -> PlanPolicy {
        match self {
            ExecutionProfile::Low => PlanPolicy {
                max_depth: 1,
                max_steps: 4,
                max_retries: 2,
            },
            ExecutionProfile::Medium => PlanPolicy {
                max_depth: 2,
                max_steps: 16,
                max_retries: 2,
            },
            ExecutionProfile::High => PlanPolicy {
                max_depth: 3,
                max_steps: 32,
                max_retries: 2,
            },
        }
    }
}

impl FromStr for ExecutionProfile {
    type Err = ExecutionProfileParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "low" => Ok(ExecutionProfile::Low),
            "medium" => Ok(ExecutionProfile::Medium),
            "high" => Ok(ExecutionProfile::High),
            _ => Err(ExecutionProfileParseError {
                input: s.to_string(),
            }),
        }
    }
}

impl std::fmt::Display for ExecutionProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExecutionProfile::Low => write!(f, "low"),
            ExecutionProfile::Medium => write!(f, "medium"),
            ExecutionProfile::High => write!(f, "high"),
        }
    }
}

/// Error type for `ExecutionProfile` parsing failures.
#[derive(Error, Debug)]
#[error("invalid execution profile: '{input}' (expected 'low', 'medium', or 'high')")]
pub struct ExecutionProfileParseError {
    input: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_low_profile_policy() {
        let profile = ExecutionProfile::Low;
        let policy = profile.to_policy();
        assert_eq!(policy.max_depth, 1);
        assert_eq!(policy.max_steps, 4);
        assert_eq!(policy.max_retries, 2);
    }

    #[test]
    fn test_medium_profile_policy() {
        let profile = ExecutionProfile::Medium;
        let policy = profile.to_policy();
        assert_eq!(policy.max_depth, 2);
        assert_eq!(policy.max_steps, 16);
        assert_eq!(policy.max_retries, 2);
    }

    #[test]
    fn test_high_profile_policy() {
        let profile = ExecutionProfile::High;
        let policy = profile.to_policy();
        assert_eq!(policy.max_depth, 3);
        assert_eq!(policy.max_steps, 32);
        assert_eq!(policy.max_retries, 2);
    }

    #[test]
    fn test_default_profile_is_medium() {
        let default = ExecutionProfile::default();
        assert_eq!(default, ExecutionProfile::Medium);
        assert_eq!(default.to_policy().max_depth, 2);
    }

    #[test]
    fn test_profile_from_str() {
        assert_eq!(
            "low".parse::<ExecutionProfile>().unwrap(),
            ExecutionProfile::Low
        );
        assert_eq!(
            "medium".parse::<ExecutionProfile>().unwrap(),
            ExecutionProfile::Medium
        );
        assert_eq!(
            "high".parse::<ExecutionProfile>().unwrap(),
            ExecutionProfile::High
        );
    }

    #[test]
    fn test_profile_from_str_case_insensitive() {
        assert_eq!(
            "LOW".parse::<ExecutionProfile>().unwrap(),
            ExecutionProfile::Low
        );
        assert_eq!(
            "MeDiUm".parse::<ExecutionProfile>().unwrap(),
            ExecutionProfile::Medium
        );
        assert_eq!(
            "HIGH".parse::<ExecutionProfile>().unwrap(),
            ExecutionProfile::High
        );
    }

    #[test]
    fn test_profile_from_str_invalid() {
        let result = "invalid".parse::<ExecutionProfile>();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("invalid execution profile"));
    }

    #[test]
    fn test_profile_display() {
        assert_eq!(ExecutionProfile::Low.to_string(), "low");
        assert_eq!(ExecutionProfile::Medium.to_string(), "medium");
        assert_eq!(ExecutionProfile::High.to_string(), "high");
    }

    #[test]
    fn test_profile_equality() {
        assert_eq!(ExecutionProfile::Low, ExecutionProfile::Low);
        assert_ne!(ExecutionProfile::Low, ExecutionProfile::Medium);
    }

    #[test]
    fn test_profile_copy() {
        let profile = ExecutionProfile::Medium;
        let _copy = profile; // Copy trait allows this
        let _another = profile; // Can use again
        assert_eq!(profile, ExecutionProfile::Medium);
    }
}
