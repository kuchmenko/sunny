use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sunny_core::agent::{
    Agent, AgentContext, AgentCost, AgentError, AgentMessage, AgentMetadata, AgentMode,
    AgentResponse, Capability,
};
use sunny_core::tool::{FileReader, FileScanner, TextGrep, ToolError, ToolPolicy};
use sunny_mind::{
    ChatMessage, ChatRole, LlmProvider, LlmRequest, ToolCall, ToolChoice, ToolDefinition,
};
use tokio_util::sync::CancellationToken;

use crate::git_tools::{GitDiff, GitLog, GitStatus};
use crate::tool_loop::{ToolCallError, ToolCallLoop};

const MAX_TOOL_ITERATIONS: usize = 15;

pub struct ExploreAgent {
    provider: Option<Arc<dyn LlmProvider>>,
    scanner: Arc<FileScanner>,
    reader: Arc<FileReader>,
    cancel: CancellationToken,
    metadata: AgentMetadata,
}

impl ExploreAgent {
    pub fn new(provider: Option<Arc<dyn LlmProvider>>) -> Self {
        Self {
            provider,
            scanner: Arc::new(FileScanner::default()),
            reader: Arc::new(FileReader::default()),
            cancel: CancellationToken::new(),
            metadata: AgentMetadata {
                mode: AgentMode::Subagent,
                category: "exploration",
                cost: AgentCost::Free,
            },
        }
    }

    fn build_tool_policy() -> ToolPolicy {
        ToolPolicy::deny_list(&["file_write", "exec", "shell"])
    }

    fn build_tool_definitions() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "fs_scan".to_string(),
                description: "Scan directory".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }),
            },
            ToolDefinition {
                name: "fs_read".to_string(),
                description: "Read file".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }),
            },
            ToolDefinition {
                name: "text_grep".to_string(),
                description: "Search for a text pattern in a file and return matching lines"
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "pattern": { "type": "string" }
                    },
                    "required": ["path", "pattern"]
                }),
            },
            ToolDefinition {
                name: "git_log".to_string(),
                description: "Run read-only git log inspection".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "args": { "type": "string", "description": "Optional allowed git log flags" }
                    }
                }),
            },
            ToolDefinition {
                name: "git_diff".to_string(),
                description: "Run read-only git diff inspection".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "args": { "type": "string", "description": "Optional allowed git diff flags" }
                    }
                }),
            },
            ToolDefinition {
                name: "git_status".to_string(),
                description: "Run read-only git status inspection".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "args": { "type": "string", "description": "Optional allowed git status flags" }
                    }
                }),
            },
        ]
    }

    fn parse_task_input(msg: AgentMessage) -> Result<(PathBuf, String, String), AgentError> {
        match msg {
            AgentMessage::Task {
                id,
                content,
                metadata,
            } => {
                let trimmed = content.trim();
                if trimmed.is_empty() && !metadata.contains_key("_sunny.cwd") {
                    return Err(AgentError::ExecutionFailed {
                        source: Box::new(std::io::Error::other(
                            "explore query path cannot be empty",
                        )),
                    });
                }

                let root_path = metadata
                    .get("_sunny.cwd")
                    .filter(|value| !value.trim().is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(trimmed));

                let query = metadata
                    .get("_sunny.query")
                    .cloned()
                    .unwrap_or_else(|| content.clone());

                Ok((root_path, query, id))
            }
        }
    }

    fn tool_root_path(root_path: &Path) -> &Path {
        if root_path.is_dir() {
            root_path
        } else {
            root_path.parent().unwrap_or(root_path)
        }
    }

    fn contains_git_component(path: &Path) -> bool {
        path.components()
            .any(|component| component.as_os_str() == std::ffi::OsStr::new(".git"))
    }

    fn safe_truncate(text: &mut String, max_bytes: usize) {
        if text.len() <= max_bytes {
            return;
        }

        let mut safe_index = max_bytes;
        while safe_index > 0 && !text.is_char_boundary(safe_index) {
            safe_index -= 1;
        }
        text.truncate(safe_index);
    }

    fn resolve_tool_path(root_path: &Path, requested_path: &str) -> Result<PathBuf, ToolError> {
        let tool_root = Self::tool_root_path(root_path);
        let canonical_root = std::fs::canonicalize(tool_root).map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => ToolError::PathNotFound {
                path: tool_root.display().to_string(),
            },
            std::io::ErrorKind::PermissionDenied => ToolError::PermissionDenied {
                path: tool_root.display().to_string(),
            },
            _ => ToolError::ExecutionFailed {
                source: Box::new(err),
            },
        })?;

        let requested = PathBuf::from(requested_path);
        let candidate = if requested.is_absolute() {
            requested
        } else {
            canonical_root.join(requested)
        };

        if !candidate.exists() {
            return Err(ToolError::PathNotFound {
                path: candidate.display().to_string(),
            });
        }

        let canonical_candidate =
            std::fs::canonicalize(&candidate).map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => ToolError::PathNotFound {
                    path: candidate.display().to_string(),
                },
                std::io::ErrorKind::PermissionDenied => ToolError::PermissionDenied {
                    path: candidate.display().to_string(),
                },
                _ => ToolError::ExecutionFailed {
                    source: Box::new(err),
                },
            })?;

        if !canonical_candidate.starts_with(&canonical_root) {
            return Err(ToolError::SensitiveFileDenied {
                path: canonical_candidate.display().to_string(),
            });
        }

        if Self::contains_git_component(&canonical_candidate) {
            return Err(ToolError::SensitiveFileDenied {
                path: canonical_candidate.display().to_string(),
            });
        }

        Ok(canonical_candidate)
    }

    fn execute_tool_static(
        root_path: &Path,
        scanner: &FileScanner,
        reader: &FileReader,
        git_log: &GitLog,
        git_diff: &GitDiff,
        git_status: &GitStatus,
        tool_call: &ToolCall,
    ) -> Result<String, ToolError> {
        match tool_call.name.as_str() {
            "fs_scan" => {
                let args: serde_json::Value =
                    serde_json::from_str(&tool_call.arguments).map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                let path_str = args["path"]
                    .as_str()
                    .ok_or_else(|| ToolError::ExecutionFailed {
                        source: Box::new(std::io::Error::other("missing 'path' argument")),
                    })?;
                let path = Self::resolve_tool_path(root_path, path_str)?;
                let scan = scanner.scan(&path)?;
                let files: Vec<String> = scan
                    .files
                    .iter()
                    .map(|f| f.path.to_string_lossy().to_string())
                    .collect();
                serde_json::to_string(&files).map_err(|e| ToolError::ExecutionFailed {
                    source: Box::new(e),
                })
            }
            "fs_read" => {
                let args: serde_json::Value =
                    serde_json::from_str(&tool_call.arguments).map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                let path_str = args["path"]
                    .as_str()
                    .ok_or_else(|| ToolError::ExecutionFailed {
                        source: Box::new(std::io::Error::other("missing 'path' argument")),
                    })?;
                let path = Self::resolve_tool_path(root_path, path_str)?;
                let content = reader.read(&path)?;
                let mut text = content.content;
                Self::safe_truncate(&mut text, 4096);
                Ok(text)
            }
            "text_grep" => {
                let args: serde_json::Value =
                    serde_json::from_str(&tool_call.arguments).map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                let path_str = args["path"]
                    .as_str()
                    .ok_or_else(|| ToolError::ExecutionFailed {
                        source: Box::new(std::io::Error::other("missing 'path' argument")),
                    })?;
                let pattern =
                    args["pattern"]
                        .as_str()
                        .ok_or_else(|| ToolError::ExecutionFailed {
                            source: Box::new(std::io::Error::other("missing 'pattern' argument")),
                        })?;
                let path = Self::resolve_tool_path(root_path, path_str)?;
                let file_content = reader.read(&path)?;
                let grep = TextGrep::default();
                let result = grep.search(&file_content.content, pattern);
                let matches: Vec<String> = result
                    .matches
                    .iter()
                    .map(|m| format!("{}:{}", m.line_number, m.line_content))
                    .collect();
                serde_json::to_string(&matches).map_err(|e| ToolError::ExecutionFailed {
                    source: Box::new(e),
                })
            }
            "git_log" => {
                let args: serde_json::Value =
                    serde_json::from_str(&tool_call.arguments).map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                let git_args = args["args"].as_str().unwrap_or_default();
                git_log.execute(git_args, Self::tool_root_path(root_path))
            }
            "git_diff" => {
                let args: serde_json::Value =
                    serde_json::from_str(&tool_call.arguments).map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                let git_args = args["args"].as_str().unwrap_or_default();
                git_diff.execute(git_args, Self::tool_root_path(root_path))
            }
            "git_status" => {
                let args: serde_json::Value =
                    serde_json::from_str(&tool_call.arguments).map_err(|e| {
                        ToolError::ExecutionFailed {
                            source: Box::new(e),
                        }
                    })?;
                let git_args = args["args"].as_str().unwrap_or_default();
                git_status.execute(git_args, Self::tool_root_path(root_path))
            }
            _ => Err(ToolError::ExecutionFailed {
                source: Box::new(std::io::Error::other(format!(
                    "unknown tool: {}",
                    tool_call.name
                ))),
            }),
        }
    }
}

#[async_trait::async_trait]
impl Agent for ExploreAgent {
    fn name(&self) -> &str {
        "explore"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability("explore".to_string())]
    }

    async fn handle_message(
        &self,
        msg: AgentMessage,
        _ctx: &AgentContext,
    ) -> Result<AgentResponse, AgentError> {
        let (root_path, query, task_id) = Self::parse_task_input(msg)?;

        let Some(provider) = &self.provider else {
            return Err(AgentError::ExecutionFailed {
                source: Box::new(std::io::Error::other("explore agent requires LLM provider")),
            });
        };

        let request = LlmRequest {
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: format!(
                        "You are an exploration-focused codebase assistant. Use fs_scan, fs_read, text_grep, git_log, git_diff, and git_status to map repository structure and locate relevant code quickly. Prefer grep-driven discovery and targeted reads over exhaustive scanning. Explore only within: {}.",
                        root_path.display()
                    ),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: query,
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                },
            ],
            max_tokens: Some(2048),
            temperature: Some(1.0),
            tools: Some(Self::build_tool_definitions()),
            tool_choice: Some(ToolChoice::Auto),
        };

        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            Self::build_tool_policy(),
            MAX_TOOL_ITERATIONS,
            self.cancel.child_token(),
        );

        let scanner = self.scanner.clone();
        let reader = self.reader.clone();
        let root = root_path.clone();
        let git_log = Arc::new(GitLog);
        let git_diff = Arc::new(GitDiff);
        let git_status = Arc::new(GitStatus);
        let executor = Arc::new(
            move |_id: &str, name: &str, arguments: &str, _depth: usize| {
                let tool_call = ToolCall {
                    id: "exec".to_string(),
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                    execution_depth: 0,
                };
                Self::execute_tool_static(
                    &root,
                    &scanner,
                    &reader,
                    &git_log,
                    &git_diff,
                    &git_status,
                    &tool_call,
                )
            },
        );

        let result = loop_runner
            .run(request, executor, 0)
            .await
            .map_err(|err| match err {
                ToolCallError::Cancelled => AgentError::ExecutionFailed {
                    source: Box::new(std::io::Error::other(
                        "explore operation cancelled during tool_call_loop",
                    )),
                },
                _ => AgentError::ExecutionFailed {
                    source: Box::new(err),
                },
            })?;

        let mut metadata = HashMap::new();
        metadata.insert("task_id".to_string(), task_id);
        metadata.insert("agent_mode".to_string(), self.metadata.mode.to_string());
        metadata.insert("agent_cost".to_string(), self.metadata.cost.to_string());
        metadata.insert(
            "tool_call_count".to_string(),
            result.metrics.total_tool_calls.to_string(),
        );
        metadata.insert(
            "iterations".to_string(),
            result.metrics.iterations.to_string(),
        );

        Ok(AgentResponse::Success {
            content: result.response.content,
            metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::fs;
    use std::process::Command;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use tokio::sync::Mutex;

    use sunny_core::agent::{Agent, AgentContext, AgentMessage, AgentResponse, Capability};
    use sunny_mind::{
        LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage, ToolCall,
    };

    use super::ExploreAgent;

    struct ToolLoopMockProvider {
        responses: Mutex<VecDeque<LlmResponse>>,
    }

    impl ToolLoopMockProvider {
        fn new(responses: Vec<LlmResponse>) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from(responses)),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for ToolLoopMockProvider {
        fn provider_id(&self) -> &str {
            "mock-tool-loop"
        }

        fn model_id(&self) -> &str {
            "mock-model"
        }

        async fn chat(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            self.responses
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| LlmError::InvalidResponse {
                    message: "no more mock responses".to_string(),
                })
        }
    }

    fn mk_ctx() -> AgentContext {
        AgentContext {
            agent_name: "test-explore".to_string(),
        }
    }

    fn mk_msg(path: &str) -> AgentMessage {
        AgentMessage::Task {
            id: "task-1".to_string(),
            content: path.to_string(),
            metadata: HashMap::new(),
        }
    }

    fn mk_temp_dir(label: &str) -> std::path::PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "sunny_boys_explore_{label}_{}_{}",
            std::process::id(),
            ts
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn init_test_repo() -> std::path::PathBuf {
        let root = mk_temp_dir("repo");
        Command::new("git")
            .args(["init"])
            .current_dir(&root)
            .output()
            .expect("git init");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(&root)
            .output()
            .expect("git config email");
        Command::new("git")
            .args(["config", "user.name", "Test User"])
            .current_dir(&root)
            .output()
            .expect("git config name");
        fs::write(root.join("main.rs"), "fn main() {}\n").expect("write file");
        Command::new("git")
            .args(["add", "main.rs"])
            .current_dir(&root)
            .output()
            .expect("git add");
        Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(&root)
            .output()
            .expect("git commit");
        root
    }

    #[test]
    fn test_explore_agent_name_and_capabilities() {
        let agent = ExploreAgent::new(None);
        assert_eq!(agent.name(), "explore");
        assert_eq!(
            agent.capabilities(),
            vec![Capability("explore".to_string())]
        );
    }

    #[test]
    fn test_explore_agent_read_only_policy() {
        let policy = ExploreAgent::build_tool_policy();
        assert!(policy.is_allowed("fs_read"));
        assert!(policy.is_allowed("git_log"));
        assert!(!policy.is_allowed("file_write"));
        assert!(!policy.is_allowed("exec"));
        assert!(!policy.is_allowed("shell"));
    }

    #[tokio::test]
    async fn test_explore_agent_with_mock_provider() {
        let repo = init_test_repo();
        let provider = Arc::new(ToolLoopMockProvider::new(vec![
            LlmResponse {
                content: "Let me check repo status.".to_string(),
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    total_tokens: 15,
                },
                finish_reason: "tool_calls".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-model".to_string()),
                tool_calls: Some(vec![ToolCall {
                    id: "call-1".to_string(),
                    name: "git_status".to_string(),
                    arguments: serde_json::json!({ "args": "--short" }).to_string(),
                    execution_depth: 0,
                }]),
                reasoning_content: None,
            },
            LlmResponse {
                content: "Exploration complete".to_string(),
                usage: TokenUsage {
                    input_tokens: 20,
                    output_tokens: 10,
                    total_tokens: 30,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-model".to_string()),
                tool_calls: None,
                reasoning_content: None,
            },
        ]));

        let agent = ExploreAgent::new(Some(provider));
        let response = agent
            .handle_message(mk_msg(repo.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response with provider");

        match response {
            AgentResponse::Success { content, .. } => {
                assert_eq!(content, "Exploration complete");
            }
            AgentResponse::Error { code, message } => {
                panic!("expected success, got error code={code}, message={message}");
            }
        }

        fs::remove_dir_all(&repo).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn test_explore_agent_without_provider_returns_error() {
        let agent = ExploreAgent::new(None);
        let result = agent.handle_message(mk_msg("."), &mk_ctx()).await;
        assert!(result.is_err(), "missing provider should return error");
    }

    #[test]
    fn test_explore_agent_includes_git_tools() {
        let defs = ExploreAgent::build_tool_definitions();
        let names: Vec<String> = defs.into_iter().map(|d| d.name).collect();
        assert!(names.contains(&"git_log".to_string()));
        assert!(names.contains(&"git_diff".to_string()));
        assert!(names.contains(&"git_status".to_string()));
    }
}
