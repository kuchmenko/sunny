use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sunny_core::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability};
use sunny_core::tool::{FileReader, FileScanner};
use sunny_mind::LlmProvider;

const MAX_CONTEXT_FILES: usize = 20;
const MAX_FILE_BYTES: usize = 2048;

pub struct CodebaseAgent {
    provider: Option<Arc<dyn LlmProvider>>,
    scanner: FileScanner,
    reader: FileReader,
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
            scanner: FileScanner::default(),
            reader: FileReader::default(),
        }
    }

    pub fn with_tools(
        provider: Option<Arc<dyn LlmProvider>>,
        scanner: FileScanner,
        reader: FileReader,
    ) -> Self {
        Self {
            provider,
            scanner,
            reader,
        }
    }

    fn parse_task_path(msg: AgentMessage) -> Result<PathBuf, AgentError> {
        match msg {
            AgentMessage::Task { content, .. } => {
                let trimmed = content.trim();
                if trimmed.is_empty() {
                    return Err(AgentError::ExecutionFailed {
                        source: Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "codebase query path cannot be empty",
                        )),
                    });
                }
                Ok(PathBuf::from(trimmed))
            }
        }
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
        let _has_llm = self.provider.is_some();
        tracing::info!(agent = %ctx.agent_name, path = %root_path.display(), "CodebaseAgent started");

        let scan = self
            .scanner
            .scan(&root_path)
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
                        .strip_prefix(&root_path)
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

        tracing::info!(agent = %ctx.agent_name, file_count = result.file_count, "CodebaseAgent completed");
        Ok(AgentResponse::Success { content, metadata })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use sunny_core::agent::{Agent, AgentContext, AgentMessage, AgentResponse, Capability};

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
}
