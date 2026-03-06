use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sunny_core::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability};
use sunny_core::tool::{FileReader, FileScanner, ToolError, ToolPolicy};
use sunny_mind::{ChatMessage, ChatRole, LlmProvider, LlmRequest, ToolCall, ToolDefinition};
use tokio_util::sync::CancellationToken;

use crate::tool_loop::{ToolCallError, ToolCallLoop};

const MAX_CONTEXT_FILES: usize = 20;
const MAX_FILE_BYTES: usize = 2048;
const MAX_TOOL_ITERATIONS: usize = 10;

pub struct CodebaseAgent {
    provider: Option<Arc<dyn LlmProvider>>,
    scanner: Arc<FileScanner>,
    reader: Arc<FileReader>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodebaseResult {
    pub file_count: usize,
    pub total_size_bytes: u64,
    pub files: Vec<CodebaseFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodebaseFile {
    pub path: String,
    pub content: String,
    pub truncated: bool,
}

impl CodebaseAgent {
    pub fn new(provider: Option<Arc<dyn LlmProvider>>) -> Self {
        Self {
            provider,
            scanner: Arc::new(FileScanner::default()),
            reader: Arc::new(FileReader::default()),
        }
    }

    pub fn with_tools(
        provider: Option<Arc<dyn LlmProvider>>,
        scanner: FileScanner,
        reader: FileReader,
    ) -> Self {
        Self {
            provider,
            scanner: Arc::new(scanner),
            reader: Arc::new(reader),
        }
    }

    fn parse_task_path(msg: AgentMessage) -> Result<PathBuf, AgentError> {
        match msg {
            AgentMessage::Task { content, .. } => {
                let trimmed = content.trim();
                if trimmed.is_empty() {
                    return Err(AgentError::ExecutionFailed {
                        source: Box::new(std::io::Error::other(
                            "codebase query path cannot be empty",
                        )),
                    });
                }
                Ok(PathBuf::from(trimmed))
            }
        }
    }

    fn build_tool_definitions() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "fs_scan".to_string(),
                description: "Scan a directory and list all files with their metadata".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Directory path to scan"
                        }
                    },
                    "required": ["path"]
                }),
            },
            ToolDefinition {
                name: "fs_read".to_string(),
                description: "Read the contents of a file".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "File path to read"
                        }
                    },
                    "required": ["path"]
                }),
            },
        ]
    }

    fn execute_tool_static(
        scanner: &FileScanner,
        reader: &FileReader,
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
                let path = PathBuf::from(path_str);
                let scan = scanner.scan(&path)?;
                let files: Vec<String> = scan
                    .files
                    .iter()
                    .map(|f| f.path.to_string_lossy().to_string())
                    .collect();
                Ok(serde_json::to_string(&files).unwrap_or_default())
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
                let path = PathBuf::from(path_str);
                let content = reader.read(&path)?;
                Ok(content.content)
            }
            _ => Err(ToolError::ExecutionFailed {
                source: Box::new(std::io::Error::other(format!(
                    "unknown tool: {}",
                    tool_call.name
                ))),
            }),
        }
    }

    async fn run_fallback(
        &self,
        root_path: &PathBuf,
        ctx: &AgentContext,
    ) -> Result<AgentResponse, AgentError> {
        tracing::info!(agent = %ctx.agent_name, path = %root_path.display(), "CodebaseAgent using fallback scanner+reader");

        let scan = self
            .scanner
            .scan(root_path)
            .map_err(|e| AgentError::ExecutionFailed {
                source: Box::new(e),
            })?;

        let mut scanned_files = scan.files;
        scanned_files.sort_by(|a, b| a.path.to_string_lossy().cmp(&b.path.to_string_lossy()));

        let mut files: Vec<CodebaseFile> = Vec::new();
        for file in scanned_files.iter().take(MAX_CONTEXT_FILES) {
            match self.reader.read(&file.path) {
                Ok(content) => {
                    let mut text = content.content;
                    let truncated = text.len() > MAX_FILE_BYTES;
                    if truncated {
                        text.truncate(MAX_FILE_BYTES);
                    }
                    let rel_path = file
                        .path
                        .strip_prefix(root_path)
                        .unwrap_or(&file.path)
                        .to_string_lossy()
                        .to_string();
                    files.push(CodebaseFile {
                        path: rel_path,
                        content: text,
                        truncated,
                    });
                }
                Err(err) => {
                    tracing::warn!(path = %file.path.display(), error = %err, "CodebaseAgent skipped file");
                }
            }
        }

        let result = CodebaseResult {
            file_count: scanned_files.len(),
            total_size_bytes: scanned_files.iter().map(|f| f.size_bytes).sum(),
            files,
        };

        let content = serde_json::to_string(&result).map_err(|e| AgentError::ExecutionFailed {
            source: Box::new(e),
        })?;

        let mut metadata = HashMap::new();
        metadata.insert("file_count".to_string(), result.file_count.to_string());

        tracing::info!(agent = %ctx.agent_name, file_count = result.file_count, "CodebaseAgent completed (fallback)");
        Ok(AgentResponse::Success { content, metadata })
    }
}

#[async_trait::async_trait]
impl Agent for CodebaseAgent {
    fn name(&self) -> &str {
        "codebase"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability("query".to_string())]
    }

    async fn handle_message(
        &self,
        msg: AgentMessage,
        ctx: &AgentContext,
    ) -> Result<AgentResponse, AgentError> {
        let root_path = Self::parse_task_path(msg)?;

        let Some(provider) = &self.provider else {
            return self.run_fallback(&root_path, ctx).await;
        };

        tracing::info!(agent = %ctx.agent_name, path = %root_path.display(), "CodebaseAgent using ToolCallLoop");

        let request = LlmRequest {
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: format!(
                        "You are a codebase analysis assistant. Use the fs_scan and fs_read tools to explore the codebase at: {}. \
                         Provide a summary of the codebase structure and key files.",
                        root_path.display()
                    ),
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: format!("Please analyze the codebase at: {}", root_path.display()),
                },
            ],
            max_tokens: Some(2048),
            temperature: Some(0.2),
            tools: Some(Self::build_tool_definitions()),
            tool_choice: None,
        };

        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            MAX_TOOL_ITERATIONS,
            CancellationToken::new(),
        );

        let scanner = self.scanner.clone();
        let reader = self.reader.clone();
        let executor = move |_id: &str, name: &str, arguments: &str| {
            let tool_call = ToolCall {
                id: "exec".to_string(),
                name: name.to_string(),
                arguments: arguments.to_string(),
                execution_depth: 0,
            };
            Self::execute_tool_static(&scanner, &reader, &tool_call)
        };

        let result = loop_runner
            .run(request, &executor)
            .await
            .map_err(|e| match e {
                ToolCallError::PolicyViolation { tool_name } => AgentError::ExecutionFailed {
                    source: Box::new(std::io::Error::other(format!(
                        "Tool {} not allowed by policy",
                        tool_name
                    ))),
                },
                ToolCallError::MaxIterationsReached { count } => AgentError::ExecutionFailed {
                    source: Box::new(std::io::Error::other(format!(
                        "Max tool iterations reached: {}",
                        count
                    ))),
                },
                ToolCallError::Llm { source } => AgentError::ExecutionFailed {
                    source: Box::new(std::io::Error::other(format!("LLM error: {}", source))),
                },
                ToolCallError::ToolExecution { source } => AgentError::ExecutionFailed {
                    source: Box::new(std::io::Error::other(format!(
                        "Tool execution error: {}",
                        source
                    ))),
                },
                ToolCallError::ToolTimeout {
                    tool_name,
                    timeout_secs,
                } => AgentError::ExecutionFailed {
                    source: Box::new(std::io::Error::other(format!(
                        "Tool {} timed out after {}s",
                        tool_name, timeout_secs
                    ))),
                },
                ToolCallError::Cancelled => AgentError::ExecutionFailed {
                    source: Box::new(std::io::Error::other("Tool call loop cancelled")),
                },
            })?;

        let mut metadata = HashMap::new();
        metadata.insert(
            "file_count".to_string(),
            result.metrics.total_tool_calls.to_string(),
        );
        metadata.insert(
            "iterations".to_string(),
            result.metrics.iterations.to_string(),
        );

        tracing::info!(
            agent = %ctx.agent_name,
            iterations = result.metrics.iterations,
            total_tool_calls = result.metrics.total_tool_calls,
            "CodebaseAgent completed (ToolCallLoop)"
        );

        Ok(AgentResponse::Success {
            content: result.response.content,
            metadata,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use sunny_core::agent::{Agent, AgentContext, AgentMessage, AgentResponse, Capability};
    use sunny_mind::{
        LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage,
    };

    use super::{CodebaseAgent, CodebaseResult};

    fn mk_ctx() -> AgentContext {
        AgentContext {
            agent_name: "test-codebase".to_string(),
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
            "sunny_boys_codebase_{label}_{}_{}",
            std::process::id(),
            ts
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn parse_success(response: AgentResponse) -> (CodebaseResult, HashMap<String, String>) {
        match response {
            AgentResponse::Success { content, metadata } => {
                let parsed: CodebaseResult = serde_json::from_str(&content)
                    .expect("response must be valid CodebaseResult JSON");
                (parsed, metadata)
            }
            AgentResponse::Error { code, message } => {
                panic!("expected success, got error code={code}, message={message}")
            }
        }
    }

    struct MockProvider;

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        fn provider_id(&self) -> &str {
            "mock"
        }

        fn model_id(&self) -> &str {
            "mock-model"
        }

        async fn chat(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                content: "Mock analysis complete".to_string(),
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    total_tokens: 15,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-model".to_string()),
                tool_calls: None,
            })
        }
    }

    #[test]
    fn test_codebase_agent_name_and_capabilities() {
        let agent = CodebaseAgent::new(None);
        assert_eq!(agent.name(), "codebase");
        assert_eq!(agent.capabilities(), vec![Capability("query".to_string())]);
    }

    #[tokio::test]
    async fn test_codebase_agent_handles_task() {
        let dir = mk_temp_dir("handles_task");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write file");
        fs::write(dir.join("lib.rs"), "pub fn hello() {}\n").expect("write file");

        let agent = CodebaseAgent::new(None);
        let response = agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response");

        let (result, metadata) = parse_success(response);
        assert_eq!(result.file_count, 2);
        assert_eq!(result.files.len(), 2);
        assert_eq!(metadata.get("file_count").map(String::as_str), Some("2"));

        let paths: Vec<&str> = result.files.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&"lib.rs"));
        assert!(paths.contains(&"main.rs"));

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn test_codebase_agent_empty_content_error() {
        let agent = CodebaseAgent::new(None);
        let msg = AgentMessage::Task {
            id: "task-empty".to_string(),
            content: "".to_string(),
            metadata: HashMap::new(),
        };

        let result = agent.handle_message(msg, &mk_ctx()).await;
        assert!(result.is_err(), "empty content should produce an error");
    }

    #[tokio::test]
    async fn test_codebase_agent_with_provider() {
        let dir = mk_temp_dir("with_provider");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write file");

        let provider = Arc::new(MockProvider);
        let agent = CodebaseAgent::new(Some(provider));

        let response = agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response with provider");

        match response {
            AgentResponse::Success { content, metadata } => {
                assert_eq!(content, "Mock analysis complete");
                assert!(metadata.contains_key("iterations"));
            }
            AgentResponse::Error { code, message } => {
                panic!("expected success, got error code={code}, message={message}");
            }
        }

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn test_codebase_agent_fallback_without_provider() {
        let dir = mk_temp_dir("fallback");
        fs::write(dir.join("test.txt"), "test content\n").expect("write file");

        let agent = CodebaseAgent::new(None);

        let response = agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response without provider");

        let (result, metadata) = parse_success(response);
        assert_eq!(result.file_count, 1);
        assert_eq!(result.files.len(), 1);
        assert_eq!(metadata.get("file_count").map(String::as_str), Some("1"));

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }
}
