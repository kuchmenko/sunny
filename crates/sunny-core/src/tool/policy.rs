use std::collections::HashSet;

const READONLY_TOOLS: &[&str] = &["fs_read", "fs_scan", "text_grep"];

/// Tool allow/deny policy for read-only ask flows.
///
/// The default ask policy permits only tools in the built-in read-only
/// allowlist. Callers should use [`ToolPolicy::is_allowed`] to decide whether a
/// tool may execute and [`ToolPolicy::is_mutation`] to classify unknown tools as
/// mutating by default.
pub struct ToolPolicy {
    allowed_tools: HashSet<String>,
    denied_tools: HashSet<String>,
}

impl ToolPolicy {
    /// Build the default read-only policy used by `ask`.
    ///
    /// This allowlist is populated from `READONLY_TOOLS`. Tools outside that set
    /// are not allowed by default and are treated as mutating.
    pub fn default_ask() -> Self {
        let allowed_tools = READONLY_TOOLS
            .iter()
            .map(|tool| (*tool).to_string())
            .collect();

        Self {
            allowed_tools,
            denied_tools: HashSet::new(),
        }
    }

    /// Return `true` when the tool name is explicitly present in the allowlist.
    pub fn is_allowed(&self, tool_name: &str) -> bool {
        self.allowed_tools.contains(tool_name)
    }

    /// Return `true` when the tool is not one of the known read-only tools.
    ///
    /// Unknown tools are classified as mutating so callers can deny them by
    /// default even if they are not listed in [`ToolPolicy::denied_tools`].
    pub fn is_mutation(&self, tool_name: &str) -> bool {
        !READONLY_TOOLS.contains(&tool_name)
    }

    /// Return the explicit denylist tracked by this policy.
    ///
    /// This set may be empty and is not an exhaustive list of all disallowed
    /// tools; callers should still consult [`ToolPolicy::is_allowed`] and
    /// [`ToolPolicy::is_mutation`] when enforcing policy.
    pub fn denied_tools(&self) -> &HashSet<String> {
        &self.denied_tools
    }
}

#[cfg(test)]
mod tests {
    use super::{ToolPolicy, READONLY_TOOLS};

    #[test]
    fn test_default_ask_policy_allows_read_tools() {
        let policy = ToolPolicy::default_ask();

        assert!(policy.is_allowed("fs_read"));
        assert!(policy.is_allowed("fs_scan"));
        assert!(policy.is_allowed("text_grep"));
    }

    #[test]
    fn test_default_ask_policy_denies_unknown() {
        let policy = ToolPolicy::default_ask();

        assert!(!policy.is_allowed("exec"));
        assert!(!policy.is_allowed("write"));
        assert!(!policy.is_allowed("delete"));
    }

    #[test]
    fn test_mutation_detection() {
        let policy = ToolPolicy::default_ask();

        assert!(!policy.is_mutation("fs_read"));
        assert!(policy.is_mutation("file_write"));
    }

    #[test]
    fn test_readonly_tools_pinned() {
        assert_eq!(READONLY_TOOLS.len(), 3);
    }
}
