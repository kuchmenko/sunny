use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sunny_core::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability};
use sunny_core::tool::{FileReader, FileScanner, TextGrep, ToolError, ToolPolicy};
use sunny_mind::{
    ChatMessage, ChatRole, LlmProvider, LlmRequest, ToolCall, ToolChoice, ToolDefinition,
};
use tokio_util::sync::CancellationToken;

use crate::timeouts::workspace_tool_loop_budget;
use crate::tool_loop::{ToolCallError, ToolCallLoop};

const MAX_CONTEXT_FILES: usize = 20;
const MAX_FILE_BYTES: usize = 2048;
const MAX_TOOL_ITERATIONS: usize = 10;
const TOOL_LOOP_READ_MAX_BYTES: usize = 4096;
const TOOL_LOOP_SCAN_MAX_FILES: usize = 400;
const TOOL_LOOP_MAX_READ_CALLS: usize = 6;
const FALLBACK_READ_CONCURRENCY: usize = 6;

fn tool_uses_read_budget(name: &str) -> bool {
    matches!(name, "fs_read" | "text_grep")
}

/// Agent that inspects a workspace by routing query requests through a bounded
/// LLM tool loop or a scanner/reader fallback when providers are unavailable.
pub struct WorkspaceReadAgent {
    provider: Option<Arc<dyn LlmProvider>>,
    scanner: Arc<FileScanner>,
    reader: Arc<FileReader>,
    cancel: CancellationToken,
}

/// Structured payload returned by [`WorkspaceReadAgent`] for query responses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodebaseResult {
    pub file_count: usize,
    pub total_size_bytes: u64,
    pub files: Vec<CodebaseFile>,
}

/// A single representative file snippet captured for codebase inspection output.
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

impl WorkspaceReadAgent {
    fn cancelled_error(operation: &str) -> AgentError {
        AgentError::ExecutionFailed {
            source: Box::new(std::io::Error::other(format!(
                "codebase operation cancelled during {operation}"
            ))),
        }
    }

    fn tool_root_path(root_path: &Path) -> &Path {
        if root_path.is_dir() {
            root_path
        } else {
            root_path.parent().unwrap_or(root_path)
        }
    }

    fn display_path(root: &Path, path: &Path) -> String {
        let relative = path.strip_prefix(root).unwrap_or(path);
        let rendered = relative.to_string_lossy();
        if rendered.is_empty() {
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string()
        } else {
            rendered.to_string()
        }
    }

    fn contains_git_component(path: &Path) -> bool {
        path.components()
            .any(|component| component.as_os_str() == std::ffi::OsStr::new(".git"))
    }

    fn safe_truncate(text: &mut String, max_bytes: usize) -> bool {
        if text.len() <= max_bytes {
            return false;
        }

        let mut safe_index = max_bytes;
        while safe_index > 0 && !text.is_char_boundary(safe_index) {
            safe_index -= 1;
        }
        text.truncate(safe_index);
        true
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

    /// Create a codebase agent using default filesystem tools and a fresh
    /// cancellation token.
    pub fn new(provider: Option<Arc<dyn LlmProvider>>) -> Self {
        Self::with_cancel(provider, CancellationToken::new())
    }

    /// Create a codebase agent that shares the supplied cancellation token with
    /// external orchestration so in-flight tool work stops promptly.
    pub fn with_cancel(provider: Option<Arc<dyn LlmProvider>>, cancel: CancellationToken) -> Self {
        Self {
            provider,
            scanner: Arc::new(FileScanner::default()),
            reader: Arc::new(FileReader::default()),
            cancel,
        }
    }

    /// Create a codebase agent with custom scanner/reader implementations.
    pub fn with_tools(
        provider: Option<Arc<dyn LlmProvider>>,
        scanner: FileScanner,
        reader: FileReader,
    ) -> Self {
        Self {
            provider,
            scanner: Arc::new(scanner),
            reader: Arc::new(reader),
            cancel: CancellationToken::new(),
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
        root_path: &Path,
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
                let path = Self::resolve_tool_path(root_path, path_str)?;
                let scan = scanner.scan(&path)?;
                let files: Vec<String> = scan
                    .files
                    .iter()
                    .take(TOOL_LOOP_SCAN_MAX_FILES)
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
                Self::safe_truncate(&mut text, TOOL_LOOP_READ_MAX_BYTES);
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
            _ => Err(ToolError::ExecutionFailed {
                source: Box::new(std::io::Error::other(format!(
                    "unknown tool: {}",
                    tool_call.name
                ))),
            }),
        }
    }

    fn is_fallback_candidate(path: &Path) -> bool {
        if Self::contains_git_component(path) {
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
            "WorkspaceReadAgent using fallback scanner+reader"
        );

        let scanner = self.scanner.clone();
        let root_path_buf = root_path.to_path_buf();
        let scan = tokio::task::spawn_blocking(move || scanner.scan(&root_path_buf))
            .await
            .map_err(|join_err| AgentError::ExecutionFailed {
                source: Box::new(std::io::Error::other(format!(
                    "fallback scan task failed: {join_err}"
                ))),
            })?
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
            if self.cancel.is_cancelled() {
                return Err(Self::cancelled_error("fallback"));
            }

            while idx < selected.len() && in_flight < FALLBACK_READ_CONCURRENCY {
                let reader = self.reader.clone();
                let root = root_path.to_path_buf();
                let path = selected[idx].clone();
                let current_idx = idx;
                join_set.spawn(async move {
                    let path_for_read = path.clone();
                    let content = tokio::task::spawn_blocking(move || reader.read(&path_for_read))
                        .await
                        .map_err(|join_err| ToolError::ExecutionFailed {
                            source: Box::new(std::io::Error::other(format!(
                                "fallback read task failed: {join_err}"
                            ))),
                        })
                        .and_then(|content| content);
                    (current_idx, root, path, content)
                });
                in_flight += 1;
                idx += 1;
            }

            let result = tokio::select! {
                _ = self.cancel.cancelled() => {
                    join_set.abort_all();
                    return Err(Self::cancelled_error("fallback"));
                }
                result = join_set.join_next(), if in_flight > 0 => result,
            };

            if let Some(result) = result {
                in_flight = in_flight.saturating_sub(1);
                match result {
                    Ok((current_idx, root, path, Ok(content))) => {
                        let mut text = content.content;
                        let truncated = Self::safe_truncate(&mut text, MAX_FILE_BYTES);
                        let rel_path = Self::display_path(&root, &path);
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
                            "WorkspaceReadAgent skipped file"
                        );
                    }
                    Err(err) => {
                        skipped_file_count = skipped_file_count.saturating_add(1);
                        tracing::warn!(
                            request_id,
                            task_id,
                            error = %err,
                            "WorkspaceReadAgent fallback read task join failed"
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
            "WorkspaceReadAgent completed (fallback)"
        );
        Ok(AgentResponse::Success { content, metadata })
    }
}

#[async_trait::async_trait]
impl Agent for WorkspaceReadAgent {
    fn name(&self) -> &str {
        "workspace-read"
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
            "WorkspaceReadAgent normalized task input"
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
            tool_loop_budget_secs = workspace_tool_loop_budget().as_secs(),
            "WorkspaceReadAgent using ToolCallLoop"
        );

        let request = LlmRequest {
        messages: vec![
            ChatMessage {
                role: ChatRole::System,
                content: format!(
                    "You are a codebase analysis assistant. Use the fs_scan, fs_read, and text_grep tools to explore the codebase at: {}. \
                     Focus on key architecture files and avoid exhaustive reads. \
                     Read at most {} files and stop when enough context is gathered. \
                     Provide a concise summary of structure and key modules.",
                    root_path.display(),
                    TOOL_LOOP_MAX_READ_CALLS
                ),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: ChatRole::User,
                content: format!(
                    "User request: {}\nAnalyze the codebase at: {}",
                    query,
                    root_path.display()
                ),
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
            ToolPolicy::default_ask(),
            MAX_TOOL_ITERATIONS,
            self.cancel.child_token(),
        );

        let scanner = self.scanner.clone();
        let reader = self.reader.clone();
        let tool_root = root_path.clone();
        let read_calls = Arc::new(AtomicUsize::new(0));
        let request_id_for_tool = request_id.clone();
        let task_id_for_tool = task_id.clone();
        let read_calls_for_tool = read_calls.clone();
        let executor = Arc::new(
            move |_id: &str, name: &str, arguments: &str, _depth: usize| {
                if tool_uses_read_budget(name) {
                    let count = read_calls_for_tool.fetch_add(1, Ordering::Relaxed) + 1;
                    if count > TOOL_LOOP_MAX_READ_CALLS {
                        return Err(ToolError::ExecutionFailed {
                        source: Box::new(std::io::Error::other(format!(
                            "read-like tool call budget exceeded for {name}: {count} > {TOOL_LOOP_MAX_READ_CALLS}"
                        ))),
                    });
                    }
                }

                tracing::info!(
                    agent = "workspace-read",
                    request_id = %request_id_for_tool,
                    task_id = %task_id_for_tool,
                    tool_name = %name,
                    "WorkspaceReadAgent dispatching tool call"
                );
                let tool_call = ToolCall {
                    id: "exec".to_string(),
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                    execution_depth: 0,
                };
                Self::execute_tool_static(&tool_root, &scanner, &reader, &tool_call)
            },
        );

        let tool_loop_budget = workspace_tool_loop_budget();
        let result =
            match tokio::time::timeout(tool_loop_budget, loop_runner.run(request, executor, 0))
                .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(ToolCallError::Cancelled)) => {
                    return Err(Self::cancelled_error("tool_call_loop"));
                }
                Ok(Err(err)) => {
                    tracing::warn!(
                        agent = %ctx.agent_name,
                        request_id,
                        task_id,
                        path = %root_path.display(),
                        error = %err,
                        "WorkspaceReadAgent ToolCallLoop failed; falling back to scanner+reader"
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
                        timeout_secs = tool_loop_budget.as_secs(),
                        "WorkspaceReadAgent ToolCallLoop timed out; falling back to scanner+reader"
                    );
                    return self
                        .run_fallback(&root_path, &request_id, &task_id, "tool_loop_timeout", ctx)
                        .await;
                }
            };

        let mut metadata = HashMap::new();
        metadata.insert("mode".to_string(), "LLM_TOOL_LOOP".to_string());
        metadata.insert(
            "tool_call_count".to_string(),
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
            "WorkspaceReadAgent completed (ToolCallLoop)"
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
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use sunny_core::tool::{FileReader, FileScanner, ToolError};
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    use sunny_core::agent::{
        Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability,
    };
    use sunny_mind::{
        LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage, ToolCall,
    };

    use super::{
        tool_uses_read_budget, CodebaseResult, WorkspaceReadAgent, TOOL_LOOP_MAX_READ_CALLS,
    };

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
                reasoning_content: None,
            })
        }
    }

    #[test]
    fn test_workspace_read_agent_name_and_capabilities() {
        let agent = WorkspaceReadAgent::new(None);
        assert_eq!(agent.name(), "workspace-read");
        assert_eq!(agent.capabilities(), vec![Capability("query".to_string())]);
    }

    #[tokio::test]
    async fn test_workspace_read_agent_handles_task() {
        let dir = mk_temp_dir("handles_task");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write file");
        fs::write(dir.join("lib.rs"), "pub fn hello() {}\n").expect("write file");

        let agent = WorkspaceReadAgent::new(None);
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
    async fn test_workspace_read_agent_empty_content_error() {
        let agent = WorkspaceReadAgent::new(None);
        let msg = AgentMessage::Task {
            id: "task-empty".to_string(),
            content: "".to_string(),
            metadata: HashMap::new(),
        };

        let result = agent.handle_message(msg, &mk_ctx()).await;
        assert!(result.is_err(), "empty content should produce an error");
    }

    #[tokio::test]
    async fn test_workspace_read_agent_with_provider() {
        let dir = mk_temp_dir("with_provider");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write file");

        let provider = Arc::new(MockProvider);
        let agent = WorkspaceReadAgent::new(Some(provider));

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
    async fn test_workspace_read_agent_fallback_without_provider() {
        let dir = mk_temp_dir("fallback");
        fs::write(dir.join("test.txt"), "test content\n").expect("write file");

        let agent = WorkspaceReadAgent::new(None);

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
    async fn test_workspace_read_agent_with_tool_loop() {
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
                reasoning_content: None,
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
                reasoning_content: None,
            },
        ]));

        let agent = WorkspaceReadAgent::new(Some(provider));

        let response = agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response with tool loop");

        match response {
            AgentResponse::Success { content, metadata } => {
                assert_eq!(content, "Found 2 Rust source files in the codebase.");
                assert_eq!(metadata.get("iterations").map(String::as_str), Some("2"));
                assert_eq!(
                    metadata.get("tool_call_count").map(String::as_str),
                    Some("1")
                );
            }
            AgentResponse::Error { code, message } => {
                panic!("expected success, got error code={code}, message={message}");
            }
        }

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[test]
    fn test_is_fallback_candidate_excludes_git_pointer_and_binary_like_files() {
        assert!(!WorkspaceReadAgent::is_fallback_candidate(Path::new(
            ".git"
        )));
        assert!(!WorkspaceReadAgent::is_fallback_candidate(Path::new(
            "assets/logo.png"
        )));
        assert!(!WorkspaceReadAgent::is_fallback_candidate(Path::new(
            "Cargo.lock"
        )));
        assert!(WorkspaceReadAgent::is_fallback_candidate(Path::new(
            "src/lib.rs"
        )));
        assert!(WorkspaceReadAgent::is_fallback_candidate(Path::new(
            "README.md"
        )));
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

        let err = WorkspaceReadAgent::execute_tool_static(&dir, &scanner, &reader, &tool_call)
            .expect_err(".git file should be blocked");
        assert!(matches!(err, ToolError::SensitiveFileDenied { .. }));

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[test]
    fn test_execute_tool_static_rejects_paths_outside_workspace_root() {
        let dir = mk_temp_dir("outside_root");
        let outside_dir = mk_temp_dir("outside_root_target");
        let outside_file = outside_dir.join("secret.txt");
        fs::write(&outside_file, "secret").expect("write secret file");

        let scanner = FileScanner::default();
        let reader = FileReader::default();
        let tool_call = ToolCall {
            id: "t2".to_string(),
            name: "fs_read".to_string(),
            arguments: serde_json::json!({ "path": outside_file }).to_string(),
            execution_depth: 0,
        };

        let err = WorkspaceReadAgent::execute_tool_static(&dir, &scanner, &reader, &tool_call)
            .expect_err("outside file should be blocked");
        assert!(matches!(err, ToolError::SensitiveFileDenied { .. }));

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
        fs::remove_dir_all(&outside_dir).expect("cleanup outside temp dir");
    }

    #[test]
    fn test_safe_truncate_respects_utf8_boundary() {
        let mut text = "cześć-file".to_string();
        let truncated = WorkspaceReadAgent::safe_truncate(&mut text, 4);

        assert!(truncated);
        assert_eq!(text, "cze");
    }

    #[test]
    fn test_resolve_tool_path_rejects_git_component() {
        let dir = mk_temp_dir("git_component_guard");
        let git_dir = dir.join(".git");
        fs::create_dir_all(&git_dir).expect("create .git dir");
        let git_config = git_dir.join("config");
        fs::write(&git_config, "[core]").expect("write git config");

        let err = WorkspaceReadAgent::resolve_tool_path(&dir, ".git/config")
            .expect_err(".git paths should be rejected");
        assert!(matches!(err, ToolError::SensitiveFileDenied { .. }));

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    struct RequestCapturingProvider {
        request_seen: Arc<Mutex<Vec<LlmRequest>>>,
        response: LlmResponse,
    }

    #[async_trait::async_trait]
    impl LlmProvider for RequestCapturingProvider {
        fn provider_id(&self) -> &str {
            "capture"
        }

        fn model_id(&self) -> &str {
            "capture-model"
        }

        async fn chat(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            self.request_seen.lock().await.push(req);
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn test_workspace_read_agent_prompt_matches_read_budget() {
        let dir = mk_temp_dir("prompt_budget");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write file");

        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(RequestCapturingProvider {
            request_seen: seen.clone(),
            response: LlmResponse {
                content: "done".to_string(),
                usage: TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    total_tokens: 2,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("capture".to_string()),
                model_id: ModelId("capture-model".to_string()),
                tool_calls: None,
                reasoning_content: None,
            },
        });
        let agent = WorkspaceReadAgent::new(Some(provider));

        agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response");

        let requests = seen.lock().await;
        let system_prompt = &requests[0].messages[0].content;
        assert!(system_prompt.contains(&format!("Read at most {} files", TOOL_LOOP_MAX_READ_CALLS)));

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    struct FailingReadBudgetProvider {
        response_sent: AtomicBool,
    }

    struct SlowProvider;

    #[async_trait::async_trait]
    impl LlmProvider for FailingReadBudgetProvider {
        fn provider_id(&self) -> &str {
            "budget"
        }

        fn model_id(&self) -> &str {
            "budget-model"
        }

        async fn chat(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            if self.response_sent.swap(true, AtomicOrdering::SeqCst) {
                return Ok(LlmResponse {
                    content: "unused".to_string(),
                    usage: TokenUsage {
                        input_tokens: 0,
                        output_tokens: 0,
                        total_tokens: 0,
                    },
                    finish_reason: "stop".to_string(),
                    provider_id: ProviderId("budget".to_string()),
                    model_id: ModelId("budget-model".to_string()),
                    tool_calls: None,
                    reasoning_content: None,
                });
            }

            let tool_calls = (0..=TOOL_LOOP_MAX_READ_CALLS)
                .map(|idx| ToolCall {
                    id: format!("call-{idx}"),
                    name: "text_grep".to_string(),
                    arguments: serde_json::json!({
                        "path": "main.rs",
                        "pattern": "fn"
                    })
                    .to_string(),
                    execution_depth: 0,
                })
                .collect();

            Ok(LlmResponse {
                content: "searching".to_string(),
                usage: TokenUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0,
                },
                finish_reason: "tool_calls".to_string(),
                provider_id: ProviderId("budget".to_string()),
                model_id: ModelId("budget-model".to_string()),
                tool_calls: Some(tool_calls),
                reasoning_content: None,
            })
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for SlowProvider {
        fn provider_id(&self) -> &str {
            "slow"
        }

        fn model_id(&self) -> &str {
            "slow-model"
        }

        async fn chat(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(LlmResponse {
                content: "done".to_string(),
                usage: TokenUsage {
                    input_tokens: 0,
                    output_tokens: 0,
                    total_tokens: 0,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("slow".to_string()),
                model_id: ModelId("slow-model".to_string()),
                tool_calls: None,
                reasoning_content: None,
            })
        }
    }

    #[tokio::test]
    async fn test_workspace_read_agent_counts_text_grep_against_read_budget() {
        let dir = mk_temp_dir("grep_budget");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write file");

        let agent = WorkspaceReadAgent::new(Some(Arc::new(FailingReadBudgetProvider {
            response_sent: AtomicBool::new(false),
        })));

        let response = agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response");

        let (result, metadata) = parse_success(response);
        assert_eq!(
            metadata.get("mode").map(String::as_str),
            Some("TOOL_ONLY_FALLBACK")
        );
        assert_eq!(
            metadata.get("fallback_reason").map(String::as_str),
            Some("tool_loop_error")
        );
        assert_eq!(result.files.len(), 1);

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[tokio::test(start_paused = true)]
    async fn test_workspace_read_agent_stops_when_cancelled() {
        let dir = mk_temp_dir("cancelled_request");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write file");

        let cancel = CancellationToken::new();
        let agent = WorkspaceReadAgent::with_cancel(Some(Arc::new(SlowProvider)), cancel.clone());
        let request_dir = dir.clone();
        let run = tokio::spawn(async move {
            agent
                .handle_message(mk_msg(request_dir.to_str().expect("path str")), &mk_ctx())
                .await
        });

        tokio::task::yield_now().await;
        cancel.cancel();

        let err = run
            .await
            .expect("join codebase task")
            .expect_err("cancellation should stop the request");
        assert!(matches!(err, AgentError::ExecutionFailed { .. }));

        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[test]
    fn test_tool_uses_read_budget_matches_supported_read_tools() {
        assert!(tool_uses_read_budget("fs_read"));
        assert!(tool_uses_read_budget("text_grep"));
        assert!(!tool_uses_read_budget("fs_scan"));
    }
}
