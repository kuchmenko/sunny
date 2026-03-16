use std::collections::HashMap;
use std::sync::Arc;

use sunny_mind::ToolDefinition;

use super::ToolError;

/// Unified dispatch trait for a single tool.
///
/// Each tool is one struct implementing `Tool`. Tools are registered in a
/// [`ToolRegistry`] and looked up by name at dispatch time.
///
/// # Design constraints
/// - `execute` receives raw JSON args exactly as the LLM emitted them.
/// - The return value is a plain string fed back to the model as the tool result.
/// - `Send + Sync` is required so registries can be shared across threads via `Arc`.
pub trait Tool: Send + Sync {
    /// Stable tool name used in `ToolDefinition` and for dispatch.
    fn name(&self) -> &str;

    /// JSON-schema definition forwarded to the LLM.
    fn definition(&self) -> ToolDefinition;

    /// Execute the tool with the given JSON arguments.
    ///
    /// `args` is the raw `serde_json::Value` from the model. Tools should
    /// extract fields with `.get("key").and_then(|v| v.as_str())` etc.
    fn execute(&self, args: &serde_json::Value) -> Result<String, ToolError>;
}

/// A registry of named tools that produces both `ToolDefinition` lists and
/// dispatches calls by name.
///
/// Build with [`ToolRegistry::builder`] and register tools with
/// [`ToolRegistryBuilder::register`].
///
/// # Example
/// ```rust,ignore
/// let registry = ToolRegistry::builder()
///     .register(Arc::new(FsReadTool::new(root.clone())))
///     .register(Arc::new(FsScanTool::new(root.clone())))
///     .build();
///
/// let definitions = registry.definitions();
/// let result = registry.execute("fs_read", &serde_json::json!({"path": "src/main.rs"}));
/// ```
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
    /// Preserves registration order for stable `definitions()` output.
    order: Vec<String>,
}

impl ToolRegistry {
    /// Start building a new registry.
    pub fn builder() -> ToolRegistryBuilder {
        ToolRegistryBuilder::new()
    }

    /// Return ordered `ToolDefinition`s for all registered tools.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.order
            .iter()
            .filter_map(|name| self.tools.get(name))
            .map(|t| t.definition())
            .collect()
    }

    /// Dispatch a call to the named tool.
    ///
    /// Returns `ToolError::ExecutionFailed` with an "unknown tool" message when
    /// the name is not registered, keeping the same behaviour as the old
    /// monolithic match.
    pub fn execute(&self, name: &str, args: &serde_json::Value) -> Result<String, ToolError> {
        match self.tools.get(name) {
            Some(tool) => tool.execute(args),
            None => Err(ToolError::ExecutionFailed {
                source: Box::new(std::io::Error::other(format!("unknown tool: {name}"))),
            }),
        }
    }

    /// Return `true` when a tool with the given name is registered.
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Return `true` when no tools have been registered.
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Builder for [`ToolRegistry`].
pub struct ToolRegistryBuilder {
    tools: HashMap<String, Arc<dyn Tool>>,
    order: Vec<String>,
}

impl ToolRegistryBuilder {
    fn new() -> Self {
        Self {
            tools: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// Register a tool. Later registrations with the same name silently replace earlier ones.
    pub fn register(mut self, tool: Arc<dyn Tool>) -> Self {
        let name = tool.name().to_string();
        if !self.tools.contains_key(&name) {
            self.order.push(name.clone());
        }
        self.tools.insert(name, tool);
        self
    }

    /// Consume the builder and produce a [`ToolRegistry`].
    pub fn build(self) -> ToolRegistry {
        ToolRegistry {
            tools: self.tools,
            order: self.order,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoTool {
        tool_name: &'static str,
    }

    impl Tool for EchoTool {
        fn name(&self) -> &str {
            self.tool_name
        }

        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: self.tool_name.to_string(),
                description: format!("echo tool: {}", self.tool_name),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
                group: Default::default(),
                hint: None,
            }
        }

        fn execute(&self, args: &serde_json::Value) -> Result<String, ToolError> {
            Ok(format!("{}:{args}", self.tool_name))
        }
    }

    fn echo(name: &'static str) -> Arc<dyn Tool> {
        Arc::new(EchoTool { tool_name: name })
    }

    #[test]
    fn test_registry_execute_known_tool() {
        let registry = ToolRegistry::builder().register(echo("greet")).build();

        let result = registry
            .execute("greet", &serde_json::json!({"msg": "hi"}))
            .expect("known tool should execute");
        assert!(result.starts_with("greet:"), "got: {result}");
    }

    #[test]
    fn test_registry_execute_unknown_tool_returns_error() {
        let registry = ToolRegistry::builder().build();
        let err = registry
            .execute("unknown", &serde_json::json!({}))
            .expect_err("unknown tool should error");
        assert!(
            err.to_string().contains("unknown tool"),
            "error should mention tool name, got: {err}"
        );
    }

    #[test]
    fn test_registry_definitions_order_preserved() {
        let registry = ToolRegistry::builder()
            .register(echo("alpha"))
            .register(echo("beta"))
            .register(echo("gamma"))
            .build();

        let defs = registry.definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn test_registry_len_and_contains() {
        let registry = ToolRegistry::builder()
            .register(echo("a"))
            .register(echo("b"))
            .build();

        assert_eq!(registry.len(), 2);
        assert!(!registry.is_empty());
        assert!(registry.contains("a"));
        assert!(registry.contains("b"));
        assert!(!registry.contains("c"));
    }

    #[test]
    fn test_registry_duplicate_registration_replaces() {
        let registry = ToolRegistry::builder()
            .register(echo("dup"))
            .register(echo("dup"))
            .build();

        // Only one entry, no duplicate.
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.definitions().len(), 1);
    }

    #[test]
    fn test_empty_registry_is_empty() {
        let registry = ToolRegistry::builder().build();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
        assert!(registry.definitions().is_empty());
    }
}
