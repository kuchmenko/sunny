use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sunny_core::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability};
use sunny_core::tool::{FileReader, FileScanner, TextGrep, ToolError, ToolPolicy};
use sunny_mind::{
    ChatMessage, ChatRole, LlmProvider, LlmRequest, ToolCall, ToolChoice, ToolDefinition,
};
use tokio_util::sync::CancellationToken;

use crate::tool_loop::ToolCallLoop;

const MAX_CONTEXT_FILES: usize = 20;
const MAX_FILE_BYTES: usize = 2048;
const MAX_TOOL_ITERATIONS: usize = 10;
const TOOL_LOOP_BUDGET_SECS: u64 = 24;
const TOOL_LOOP_READ_MAX_BYTES: usize = 4096;
const TOOL_LOOP_SCAN_MAX_FILES: usize = 400;
const TOOL_LOOP_MAX_READ_CALLS: usize = 6;
const FALLBACK_READ_CONCURRENCY: usize = 6;

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

struct TaskInput {
    root_path: PathBuf,
    query: String,
    request_id: String,
    task_id: String,
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

    fn parse_task_input(msg: AgentMessage) -> Result<TaskInput, AgentError> {
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
                            "codebase query path cannot be empty",
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

                let request_id = metadata
                    .get("_sunny.request_id")
                    .cloned()
                    .unwrap_or_else(|| "missing".to_string());

                Ok(TaskInput {
                    root_path,
                    query,
                    request_id,
                    task_id: id,
                })
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
            ToolDefinition {
                name: "text_grep".to_string(),
                description: "Search for a text pattern in a file and return matching lines"
                    .to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "File path to search in"
                        },
                        "pattern": {
                            "type": "string",
                            "description": "Text pattern to search for"
                        }
                    },
                    "required": ["path", "pattern"]
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
                    .take(TOOL_LOOP_SCAN_MAX_FILES)
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
                if path.file_name().and_then(|name| name.to_str()) == Some(".git") {
                    return Err(ToolError::SensitiveFileDenied {
                        path: path.display().to_string(),
                    });
                }
                let content = reader.read(&path)?;
                let mut text = content.content;
                if text.len() > TOOL_LOOP_READ_MAX_BYTES {
                    text.truncate(TOOL_LOOP_READ_MAX_BYTES);
                }
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
                let path = PathBuf::from(path_str);
                let file_content = reader.read(&path)?;
                let grep = TextGrep::default();
                let result = grep.search(&file_content.content, pattern);
                let matches: Vec<String> = result
                    .matches
                    .iter()
                    .map(|m| format!("{}:{}", m.line_number, m.line_content))
                    .collect();
                Ok(serde_json::to_string(&matches).unwrap_or_default())
            }
            _ => Err(ToolError::ExecutionFailed {
                source: Box::new(std::io::Error::other(format!(
                    "unknown tool: {}",
                    tool_call.name
                ))),
            }),
        }
    }

    fn is_fallback_candidate(path: &Path) -> bool {
        if path.file_name().and_then(|name| name.to_str()) == Some(".git") {
            return false;
        }

        let ext = path.extension().and_then(|e| e.to_str());
        !matches!(
            ext,
            Some("png")
                | Some("jpg")
                | Some("jpeg")
                | Some("gif")
                | Some("webp")
                | Some("ico")
                | Some("pdf")
                | Some("zip")
                | Some("wasm")
                | Some("lock")
        )
    }

    fn fallback_priority(path: &Path) -> u8 {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        if ext == "rs" {
            return 0;
        }
        if matches!(ext, "toml" | "md" | "yaml" | "yml" | "json") {
            return 1;
        }

        let hidden = path
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.starts_with('.'))
            .unwrap_or(false);
        if hidden {
            3
        } else {
            2
        }
    }

    async fn run_fallback(
        &self,
        root_path: &Path,
        request_id: &str,
        task_id: &str,
        fallback_reason: &str,
        ctx: &AgentContext,
    ) -> Result<AgentResponse, AgentError> {
        tracing::info!(
            agent = %ctx.agent_name,
            request_id,
            task_id,
            path = %root_path.display(),
            max_concurrency = FALLBACK_READ_CONCURRENCY,
            "CodebaseAgent using fallback scanner+reader"
        );

        let scan = self
            .scanner
            .scan(root_path)
            .map_err(|e| AgentError::ExecutionFailed {
                source: Box::new(e),
            })?;

        let mut scanned_files = scan.files;
        scanned_files.sort_by(|a, b| {
            Self::fallback_priority(&a.path)
                .cmp(&Self::fallback_priority(&b.path))
                .then_with(|| a.path.to_string_lossy().cmp(&b.path.to_string_lossy()))
        });

        let selected = scanned_files
            .iter()
            .filter(|f| Self::is_fallback_candidate(&f.path))
            .take(MAX_CONTEXT_FILES)
            .map(|f| f.path.clone())
            .collect::<Vec<_>>();

        let mut files: Vec<(usize, CodebaseFile)> = Vec::new();
        let mut skipped_file_count: usize = 0;
        let mut join_set = tokio::task::JoinSet::new();
        let mut in_flight = 0usize;
        let mut idx = 0usize;

        while idx < selected.len() || in_flight > 0 {
            while idx < selected.len() && in_flight < FALLBACK_READ_CONCURRENCY {
                let reader = self.reader.clone();
                let root = root_path.to_path_buf();
                let path = selected[idx].clone();
                let current_idx = idx;
                join_set.spawn(async move {
                    let content = reader.read(&path);
                    (current_idx, root, path, content)
                });
                in_flight += 1;
                idx += 1;
            }

            if let Some(result) = join_set.join_next().await {
                in_flight = in_flight.saturating_sub(1);
                match result {
                    Ok((current_idx, root, path, Ok(content))) => {
                        let mut text = content.content;
                        let truncated = text.len() > MAX_FILE_BYTES;
                        if truncated {
                            text.truncate(MAX_FILE_BYTES);
                        }
                        let rel_path = path
                            .strip_prefix(&root)
                            .unwrap_or(&path)
                            .to_string_lossy()
                            .to_string();
                        files.push((
                            current_idx,
                            CodebaseFile {
                                path: rel_path,
                                content: text,
                                truncated,
                            },
                        ));
                    }
                    Ok((_current_idx, _root, path, Err(err))) => {
                        skipped_file_count = skipped_file_count.saturating_add(1);
                        tracing::warn!(
                            request_id,
                            task_id,
                            path = %path.display(),
                            error = %err,
                            "CodebaseAgent skipped file"
                        );
                    }
                    Err(err) => {
                        skipped_file_count = skipped_file_count.saturating_add(1);
                        tracing::warn!(
                            request_id,
                            task_id,
                            error = %err,
                            "CodebaseAgent fallback read task join failed"
                        );
                    }
                }
            }
        }

        files.sort_by_key(|(order, _)| *order);
        let files = files.into_iter().map(|(_, file)| file).collect::<Vec<_>>();

        let result = CodebaseResult {
            file_count: scanned_files.len(),
            total_size_bytes: scanned_files.iter().map(|f| f.size_bytes).sum(),
            files,
        };

        let content = serde_json::to_string(&result).map_err(|e| AgentError::ExecutionFailed {
            source: Box::new(e),
        })?;

        let mut metadata = HashMap::new();
        metadata.insert("mode".to_string(), "TOOL_ONLY_FALLBACK".to_string());
        metadata.insert("fallback_reason".to_string(), fallback_reason.to_string());
        metadata.insert("file_count".to_string(), result.file_count.to_string());
        metadata.insert(
            "skipped_file_count".to_string(),
            skipped_file_count.to_string(),
        );

        tracing::info!(
            agent = %ctx.agent_name,
            request_id,
            task_id,
            file_count = result.file_count,
            "CodebaseAgent completed (fallback)"
        );
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
        let task_input = Self::parse_task_input(msg)?;
        let root_path = task_input.root_path;
        let query = task_input.query;
        let request_id = task_input.request_id;
        let task_id = task_input.task_id;

        tracing::info!(
            agent = %ctx.agent_name,
            request_id,
            task_id,
            path = %root_path.display(),
            query_len = query.len(),
            llm_enabled = self.provider.is_some(),
            "CodebaseAgent normalized task input"
        );

        let Some(provider) = &self.provider else {
            return self
                .run_fallback(&root_path, &request_id, &task_id, "no_provider", ctx)
                .await;
        };

        tracing::info!(
            agent = %ctx.agent_name,
            request_id,
            task_id,
            path = %root_path.display(),
            tool_loop_budget_secs = TOOL_LOOP_BUDGET_SECS,
            "CodebaseAgent using ToolCallLoop"
        );

        let request = LlmRequest {
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: format!(
                        "You are a codebase analysis assistant. Use the fs_scan, fs_read, and text_grep tools to explore the codebase at: {}. \
                         Focus on key architecture files and avoid exhaustive reads. \
                         Read at most 8 files and stop when enough context is gathered. \
                         Provide a concise summary of structure and key modules.",
                        root_path.display()
                    ),
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: format!(
                        "User request: {}\nAnalyze the codebase at: {}",
                        query,
                        root_path.display()
                    ),
                },
            ],
            max_tokens: Some(2048),
            temperature: Some(1.0),
            tools: Some(Self::build_tool_definitions()),
            tool_choice: Some(ToolChoice::Auto),
        };

        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            MAX_TOOL_ITERATIONS,
            CancellationToken::new(),
        );

        let scanner = self.scanner.clone();
        let reader = self.reader.clone();
        let read_calls = Arc::new(AtomicUsize::new(0));
        let request_id_for_tool = request_id.clone();
        let task_id_for_tool = task_id.clone();
        let read_calls_for_tool = read_calls.clone();
        let executor = move |_id: &str, name: &str, arguments: &str, _depth: usize| {
            if name == "fs_read" {
                let count = read_calls_for_tool.fetch_add(1, Ordering::Relaxed) + 1;
                if count > TOOL_LOOP_MAX_READ_CALLS {
                    return Err(ToolError::ExecutionFailed {
                        source: Box::new(std::io::Error::other(format!(
                            "fs_read call budget exceeded: {} > {}",
                            count, TOOL_LOOP_MAX_READ_CALLS
                        ))),
                    });
                }
            }

            tracing::info!(
                agent = "codebase",
                request_id = %request_id_for_tool,
                task_id = %task_id_for_tool,
                tool_name = %name,
                "CodebaseAgent dispatching tool call"
            );
            let tool_call = ToolCall {
                id: "exec".to_string(),
                name: name.to_string(),
                arguments: arguments.to_string(),
                execution_depth: 0,
            };
            Self::execute_tool_static(&scanner, &reader, &tool_call)
        };

        let result = match tokio::time::timeout(
            Duration::from_secs(TOOL_LOOP_BUDGET_SECS),
            loop_runner.run(request, &executor, 0),
        )
        .await
        {
            Ok(Ok(result)) => result,
            Ok(Err(err)) => {
                tracing::warn!(
                    agent = %ctx.agent_name,
                    request_id,
                    task_id,
                    path = %root_path.display(),
                    error = %err,
                    "CodebaseAgent ToolCallLoop failed; falling back to scanner+reader"
                );
                return self
                    .run_fallback(&root_path, &request_id, &task_id, "tool_loop_error", ctx)
                    .await;
            }
            Err(_) => {
                tracing::warn!(
                    agent = %ctx.agent_name,
                    request_id,
                    task_id,
                    path = %root_path.display(),
                    operation = "tool_call_loop",
                    timeout_secs = TOOL_LOOP_BUDGET_SECS,
                    "CodebaseAgent ToolCallLoop timed out; falling back to scanner+reader"
                );
                return self
                    .run_fallback(&root_path, &request_id, &task_id, "tool_loop_timeout", ctx)
                    .await;
            }
        };

        let mut metadata = HashMap::new();
        metadata.insert("mode".to_string(), "LLM_TOOL_LOOP".to_string());
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
            request_id,
            task_id,
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
    use std::collections::{HashMap, VecDeque};
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use sunny_core::tool::{FileReader, FileScanner, ToolError};
    use tokio::sync::Mutex;

    use sunny_core::agent::{Agent, AgentContext, AgentMessage, AgentResponse, Capability};
    use sunny_mind::{
        LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage, ToolCall,
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

    #[tokio::test]
    async fn test_codebase_agent_with_tool_loop() {
        let dir = mk_temp_dir("tool_loop");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write file");
        fs::write(dir.join("lib.rs"), "pub fn greet() {}\n").expect("write file");

        let provider = Arc::new(ToolLoopMockProvider::new(vec![
            LlmResponse {
                content: "Let me scan the directory.".to_string(),
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
                    name: "fs_scan".to_string(),
                    arguments: serde_json::json!({ "path": dir.to_str().unwrap() }).to_string(),
                    execution_depth: 0,
                }]),
            },
            LlmResponse {
                content: "Found 2 Rust source files in the codebase.".to_string(),
                usage: TokenUsage {
                    input_tokens: 20,
                    output_tokens: 10,
                    total_tokens: 30,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-model".to_string()),
                tool_calls: None,
            },
        ]));

        let agent = CodebaseAgent::new(Some(provider));

        let response = agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response with tool loop");

        match response {
            AgentResponse::Success { content, metadata } => {
                assert_eq!(content, "Found 2 Rust source files in the codebase.");
                assert_eq!(metadata.get("iterations").map(String::as_str), Some("2"));
                assert_eq!(metadata.get("file_count").map(String::as_str), Some("1"));
            }
            AgentResponse::Error { code, message } => {
                panic!("expected success, got error code={code}, message={message}");
            }
        }

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[test]
    fn test_is_fallback_candidate_excludes_git_pointer_and_binary_like_files() {
        assert!(!CodebaseAgent::is_fallback_candidate(Path::new(".git")));
        assert!(!CodebaseAgent::is_fallback_candidate(Path::new(
            "assets/logo.png"
        )));
        assert!(!CodebaseAgent::is_fallback_candidate(Path::new(
            "Cargo.lock"
        )));
        assert!(CodebaseAgent::is_fallback_candidate(Path::new(
            "src/lib.rs"
        )));
        assert!(CodebaseAgent::is_fallback_candidate(Path::new("README.md")));
    }

    #[test]
    fn test_execute_tool_static_fs_read_blocks_git_pointer_file() {
        let dir = mk_temp_dir("git_pointer_file");
        let git_file = dir.join(".git");
        fs::write(&git_file, "gitdir: /tmp/worktree\n").expect("write .git file");

        let scanner = FileScanner::default();
        let reader = FileReader::default();
        let tool_call = ToolCall {
            id: "t1".to_string(),
            name: "fs_read".to_string(),
            arguments: serde_json::json!({ "path": git_file }).to_string(),
            execution_depth: 0,
        };

        let err = CodebaseAgent::execute_tool_static(&scanner, &reader, &tool_call)
            .expect_err(".git file should be blocked");
        assert!(matches!(err, ToolError::SensitiveFileDenied { .. }));

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }
}
