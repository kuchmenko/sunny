/// RestartPolicy defines how an agent should be restarted on failure.
/// This is scaffolding for future supervisor behavior — no actual restart logic yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RestartPolicy {
    /// Never restart (default). Current behavior.
    #[default]
    Never,
    /// Restart on failure, up to max_retries.
    OnFailure { max_retries: u32 },
    /// Always restart unless explicitly stopped.
    Always { max_retries: u32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_restart_policy_default_is_never() {
        let policy = RestartPolicy::default();
        assert_eq!(policy, RestartPolicy::Never);
    }

    #[test]
    fn test_restart_policy_on_failure_variant() {
        let policy = RestartPolicy::OnFailure { max_retries: 3 };
        assert_eq!(policy, RestartPolicy::OnFailure { max_retries: 3 });
    }

    #[test]
    fn test_restart_policy_always_variant() {
        let policy = RestartPolicy::Always { max_retries: 5 };
        assert_eq!(policy, RestartPolicy::Always { max_retries: 5 });
    }
}
