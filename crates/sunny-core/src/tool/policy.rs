use std::collections::HashSet;

const READONLY_TOOLS: &[&str] = &["fs_read", "fs_scan", "text_grep"];

/// Tool allow/deny policy for read-only ask flows.
///
/// The default ask policy permits only tools in the built-in read-only
/// allowlist. Callers should use [`ToolPolicy::is_allowed`] to decide whether a
/// tool may execute and [`ToolPolicy::is_mutation`] to classify unknown tools as
/// mutating by default.
#[derive(Clone)]
pub struct ToolPolicy {
    allowed_tools: HashSet<String>,
    denied_tools: HashSet<String>,
    use_deny_list: bool,
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
            use_deny_list: false,
        }
    }

    /// Build a deny-list policy that blocks specified tools.
    ///
    /// All tools not in the deny list are allowed. This is useful for
    /// restrictive policies where you want to block specific dangerous tools
    /// while allowing everything else.
    pub fn deny_list(denied: &[&str]) -> Self {
        Self {
            allowed_tools: HashSet::new(),
            denied_tools: denied.iter().map(|s| s.to_string()).collect(),
            use_deny_list: true,
        }
    }

    /// Alias for [`ToolPolicy::deny_list`].
    pub fn allow_all_except(denied: &[&str]) -> Self {
        Self::deny_list(denied)
    }

    /// Build an allow-list policy that permits only specified tools.
    ///
    /// All tools not in the allow list are blocked. This is useful for
    /// permissive policies where you want to allow only specific safe tools
    /// while blocking everything else.
    pub fn allow_list(allowed: &[&str]) -> Self {
        Self {
            allowed_tools: allowed.iter().map(|s| s.to_string()).collect(),
            denied_tools: HashSet::new(),
            use_deny_list: false,
        }
    }

    /// Return `true` when the tool name is explicitly present in the allowlist.
    pub fn is_allowed(&self, tool_name: &str) -> bool {
        if self.use_deny_list {
            // Deny-list mode: block if in deny list, allow otherwise
            !self.denied_tools.contains(tool_name)
        } else {
            // Allow-list mode: allow only if in allow list
            self.allowed_tools.contains(tool_name)
        }
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

    #[test]
    fn test_deny_list_blocks_denied_tool() {
        let policy = ToolPolicy::deny_list(&["exec", "write"]);
        assert!(!policy.is_allowed("exec"));
        assert!(!policy.is_allowed("write"));
    }

    #[test]
    fn test_deny_list_allows_non_denied_tool() {
        let policy = ToolPolicy::deny_list(&["exec"]);
        assert!(policy.is_allowed("fs_read"));
        assert!(policy.is_allowed("fs_scan"));
    }

    #[test]
    fn test_default_ask_still_works_unchanged() {
        let policy = ToolPolicy::default_ask();
        assert!(policy.is_allowed("fs_read"));
        assert!(!policy.is_allowed("exec"));
    }

    #[test]
    fn test_deny_wins_over_allow() {
        let mut policy = ToolPolicy::deny_list(&["fs_read"]);
        policy.allowed_tools.insert("fs_read".to_string());
        assert!(!policy.is_allowed("fs_read"));
    }

    #[test]
    fn test_empty_deny_list_allows_all() {
        let policy = ToolPolicy::deny_list(&[]);
        assert!(policy.is_allowed("fs_read"));
        assert!(policy.is_allowed("exec"));
        assert!(policy.is_allowed("write"));
    }

    #[test]
    fn test_allow_list_permits_listed_tool() {
        let policy = ToolPolicy::allow_list(&["fs_read", "plan_create"]);
        assert!(policy.is_allowed("fs_read"));
        assert!(policy.is_allowed("plan_create"));
    }

    #[test]
    fn test_allow_list_blocks_unlisted_tool() {
        let policy = ToolPolicy::allow_list(&["fs_read"]);
        assert!(!policy.is_allowed("fs_write"));
        assert!(!policy.is_allowed("exec"));
    }

    #[test]
    fn test_allow_list_empty_denies_all() {
        let policy = ToolPolicy::allow_list(&[]);
        assert!(!policy.is_allowed("fs_read"));
        assert!(!policy.is_allowed("fs_scan"));
        assert!(!policy.is_allowed("exec"));
    }
}
