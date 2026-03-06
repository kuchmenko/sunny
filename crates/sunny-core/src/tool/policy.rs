use std::collections::HashSet;

const READONLY_TOOLS: &[&str] = &["fs_read", "fs_scan", "text_grep"];

pub struct ToolPolicy {
    allowed_tools: HashSet<String>,
    denied_tools: HashSet<String>,
}

impl ToolPolicy {
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

    pub fn is_allowed(&self, tool_name: &str) -> bool {
        self.allowed_tools.contains(tool_name)
    }

    pub fn is_mutation(&self, tool_name: &str) -> bool {
        !READONLY_TOOLS.contains(&tool_name)
    }

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
