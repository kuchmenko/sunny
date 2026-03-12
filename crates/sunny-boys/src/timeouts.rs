use std::sync::OnceLock;
use std::time::Duration;

const DEFAULT_TOOL_CALL_TIMEOUT_SECS: u64 = 90;
const DEFAULT_TOOL_PROVIDER_TIMEOUT_SECS: u64 = 90;
const DEFAULT_WORKSPACE_TOOL_LOOP_BUDGET_SECS: u64 = 300;
const DEFAULT_EXPLORE_TOOL_LOOP_BUDGET_SECS: u64 = 300;

static TOOL_CALL_TIMEOUT: OnceLock<Duration> = OnceLock::new();
static TOOL_PROVIDER_TIMEOUT: OnceLock<Duration> = OnceLock::new();
static WORKSPACE_TOOL_LOOP_BUDGET: OnceLock<Duration> = OnceLock::new();
static EXPLORE_TOOL_LOOP_BUDGET: OnceLock<Duration> = OnceLock::new();

pub(crate) fn tool_call_timeout() -> Duration {
    *TOOL_CALL_TIMEOUT.get_or_init(|| {
        duration_from_env_secs(
            "SUNNY_TOOL_CALL_TIMEOUT_SECS",
            DEFAULT_TOOL_CALL_TIMEOUT_SECS,
        )
    })
}

pub(crate) fn workspace_tool_loop_budget() -> Duration {
    *WORKSPACE_TOOL_LOOP_BUDGET.get_or_init(|| {
        duration_from_env_secs(
            "SUNNY_WORKSPACE_TOOL_LOOP_BUDGET_SECS",
            DEFAULT_WORKSPACE_TOOL_LOOP_BUDGET_SECS,
        )
    })
}

pub(crate) fn tool_provider_timeout() -> Duration {
    *TOOL_PROVIDER_TIMEOUT.get_or_init(|| {
        duration_from_env_secs(
            "SUNNY_TOOL_PROVIDER_TIMEOUT_SECS",
            DEFAULT_TOOL_PROVIDER_TIMEOUT_SECS,
        )
    })
}

pub(crate) fn explore_tool_loop_budget() -> Duration {
    *EXPLORE_TOOL_LOOP_BUDGET.get_or_init(|| {
        duration_from_env_secs(
            "SUNNY_EXPLORE_TOOL_LOOP_BUDGET_SECS",
            DEFAULT_EXPLORE_TOOL_LOOP_BUDGET_SECS,
        )
    })
}

fn duration_from_env_secs(key: &str, default_secs: u64) -> Duration {
    Duration::from_secs(
        std::env::var(key)
            .ok()
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(default_secs),
    )
}

// Helper to read a usize-like environment variable with a sane default.
// Mirrors duration_from_env_secs pattern but for usize values.
pub(crate) fn usize_from_env(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::duration_from_env_secs;

    #[test]
    fn test_duration_from_env_secs_uses_default_for_missing_key() {
        let key = "SUNNY_TEST_BOYS_TIMEOUT_MISSING_KEY";
        std::env::remove_var(key);
        assert_eq!(duration_from_env_secs(key, 55), Duration::from_secs(55));
    }

    #[test]
    fn test_duration_from_env_secs_parses_valid_values() {
        let key = format!("SUNNY_TEST_BOYS_TIMEOUT_VALID_{}", std::process::id());
        std::env::set_var(&key, "88");
        assert_eq!(duration_from_env_secs(&key, 12), Duration::from_secs(88));
        std::env::remove_var(&key);
    }

    #[test]
    fn test_duration_from_env_secs_ignores_invalid_or_zero_values() {
        let key = format!("SUNNY_TEST_BOYS_TIMEOUT_INVALID_{}", std::process::id());
        std::env::set_var(&key, "abc");
        assert_eq!(duration_from_env_secs(&key, 12), Duration::from_secs(12));
        std::env::set_var(&key, "0");
        assert_eq!(duration_from_env_secs(&key, 12), Duration::from_secs(12));
        std::env::remove_var(&key);
    }
}
