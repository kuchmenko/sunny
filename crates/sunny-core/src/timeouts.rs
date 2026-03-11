use std::sync::OnceLock;
use std::time::Duration;

const DEFAULT_AGENT_SEND_TIMEOUT_SECS: u64 = 180;
const DEFAULT_AGENT_REPLY_TIMEOUT_SECS: u64 = 180;
const DEFAULT_ORCHESTRATOR_SEND_TIMEOUT_SECS: u64 = 180;
const DEFAULT_ORCHESTRATOR_REPLY_TIMEOUT_SECS: u64 = 180;
const DEFAULT_PLAN_CONTEXT_TIMEOUT_MS: u64 = 90_000;
const DEFAULT_PLAN_STAGE_TIMEOUT_MS: u64 = 180_000;

static AGENT_SEND_TIMEOUT: OnceLock<Duration> = OnceLock::new();
static AGENT_REPLY_TIMEOUT: OnceLock<Duration> = OnceLock::new();
static ORCHESTRATOR_SEND_TIMEOUT: OnceLock<Duration> = OnceLock::new();
static ORCHESTRATOR_REPLY_TIMEOUT: OnceLock<Duration> = OnceLock::new();
static PLAN_CONTEXT_TIMEOUT_MS: OnceLock<u64> = OnceLock::new();
static PLAN_STAGE_TIMEOUT_MS: OnceLock<u64> = OnceLock::new();

pub(crate) fn agent_send_timeout() -> Duration {
    *AGENT_SEND_TIMEOUT.get_or_init(|| {
        duration_from_env_secs(
            "SUNNY_AGENT_SEND_TIMEOUT_SECS",
            DEFAULT_AGENT_SEND_TIMEOUT_SECS,
        )
    })
}

pub(crate) fn agent_reply_timeout() -> Duration {
    *AGENT_REPLY_TIMEOUT.get_or_init(|| {
        duration_from_env_secs(
            "SUNNY_AGENT_REPLY_TIMEOUT_SECS",
            DEFAULT_AGENT_REPLY_TIMEOUT_SECS,
        )
    })
}

pub(crate) fn orchestrator_send_timeout() -> Duration {
    *ORCHESTRATOR_SEND_TIMEOUT.get_or_init(|| {
        duration_from_env_secs(
            "SUNNY_ORCHESTRATOR_SEND_TIMEOUT_SECS",
            DEFAULT_ORCHESTRATOR_SEND_TIMEOUT_SECS,
        )
    })
}

pub(crate) fn orchestrator_reply_timeout() -> Duration {
    *ORCHESTRATOR_REPLY_TIMEOUT.get_or_init(|| {
        duration_from_env_secs(
            "SUNNY_ORCHESTRATOR_REPLY_TIMEOUT_SECS",
            DEFAULT_ORCHESTRATOR_REPLY_TIMEOUT_SECS,
        )
    })
}

pub(crate) fn plan_context_timeout_ms() -> u64 {
    *PLAN_CONTEXT_TIMEOUT_MS.get_or_init(|| {
        u64_from_env(
            "SUNNY_PLAN_CONTEXT_TIMEOUT_MS",
            DEFAULT_PLAN_CONTEXT_TIMEOUT_MS,
        )
    })
}

pub(crate) fn plan_stage_timeout_ms() -> u64 {
    *PLAN_STAGE_TIMEOUT_MS
        .get_or_init(|| u64_from_env("SUNNY_PLAN_STAGE_TIMEOUT_MS", DEFAULT_PLAN_STAGE_TIMEOUT_MS))
}

fn duration_from_env_secs(key: &str, default_secs: u64) -> Duration {
    Duration::from_secs(u64_from_env(key, default_secs))
}

fn u64_from_env(key: &str, default_value: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default_value)
}

#[cfg(test)]
mod tests {
    use super::u64_from_env;

    #[test]
    fn test_u64_from_env_uses_default_for_missing_key() {
        let key = "SUNNY_TEST_TIMEOUT_MISSING_KEY";
        std::env::remove_var(key);
        assert_eq!(u64_from_env(key, 42), 42);
    }

    #[test]
    fn test_u64_from_env_parses_positive_values() {
        let key = format!("SUNNY_TEST_TIMEOUT_POSITIVE_{}", std::process::id());
        std::env::set_var(&key, "123");
        assert_eq!(u64_from_env(&key, 42), 123);
        std::env::remove_var(&key);
    }

    #[test]
    fn test_u64_from_env_ignores_invalid_or_zero_values() {
        let key = format!("SUNNY_TEST_TIMEOUT_INVALID_{}", std::process::id());
        std::env::set_var(&key, "not-a-number");
        assert_eq!(u64_from_env(&key, 77), 77);
        std::env::set_var(&key, "0");
        assert_eq!(u64_from_env(&key, 77), 77);
        std::env::remove_var(&key);
    }
}
