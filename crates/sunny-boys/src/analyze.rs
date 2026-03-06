use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sunny_core::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability};
use sunny_core::tool::fs_scan::ScannedFile;
use sunny_core::tool::{FileReader, FileScanner};
use sunny_mind::{ChatMessage, ChatRole, LlmError, LlmProvider, LlmRequest};

const MODE_LLM_ENRICHED: &str = "LLM_ENRICHED";
const MODE_TOOL_ONLY_FALLBACK: &str = "TOOL_ONLY_FALLBACK";
const MAX_SAMPLE_FILES: usize = 10;
const MAX_SNIPPET_BYTES: usize = 400;

pub struct AnalyzeAgent {
    provider: Option<Arc<dyn LlmProvider>>,
    scanner: FileScanner,
    reader: FileReader,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    pub mode: AnalysisMode,
    pub file_count: usize,
    pub truncated: bool,
    pub total_size_bytes: u64,
    pub language_stats: Vec<(String, usize)>,
    pub file_tree: String,
    pub digest: String,
    pub llm_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum AnalysisMode {
    LlmEnriched,
    ToolOnlyFallback,
}

impl AnalyzeAgent {
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
                            "analysis path cannot be empty",
                        )),
                    });
                }
                Ok(PathBuf::from(trimmed))
            }
        }
    }

    fn is_sensitive_path(&self, path: &Path) -> bool {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };

        if self.reader.denylist_names.iter().any(|n| n == name) {
            return true;
        }
        if self
            .reader
            .denylist_prefixes
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            return true;
        }
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let dotted = format!(".{ext}");
            if self.reader.denylist_extensions.contains(&dotted) {
                return true;
            }
        }
        false
    }

    fn build_file_tree(root: &Path, files: &[ScannedFile]) -> String {
        let mut paths: Vec<PathBuf> = files.iter().map(|f| f.path.clone()).collect();
        paths.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));

        let mut lines = Vec::with_capacity(paths.len() + 1);
        lines.push(".".to_string());

        for path in paths {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let components: Vec<String> = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().to_string())
                .collect();
            if components.is_empty() {
                continue;
            }
            let depth = components.len().saturating_sub(1);
            lines.push(format!(
                "{}{}",
                "  ".repeat(depth + 1),
                components.join("/")
            ));
        }

        lines.join("\n")
    }

    fn build_language_stats(files: &[ScannedFile]) -> Vec<(String, usize)> {
        let mut counts = BTreeMap::<String, usize>::new();
        for file in files {
            let key = file
                .extension
                .clone()
                .unwrap_or_else(|| "no_extension".to_string());
            *counts.entry(key).or_insert(0) += 1;
        }
        counts.into_iter().collect()
    }

    fn build_digest(
        file_tree: &str,
        language_stats: &[(String, usize)],
        snippets: &[(PathBuf, String)],
    ) -> String {
        let mut digest = String::new();
        digest.push_str("STRUCTURE\n");
        digest.push_str(file_tree);
        digest.push_str("\n\nLANGUAGE_STATS\n");
        for (lang, count) in language_stats {
            digest.push_str(&format!("- {lang}: {count}\n"));
        }
        digest.push_str("\nSAMPLE_SNIPPETS\n");
        for (path, snippet) in snippets {
            digest.push_str(&format!("--- {} ---\n", path.display()));
            digest.push_str(snippet);
            digest.push('\n');
        }
        digest
    }

    fn build_prompt(digest: &str) -> String {
        format!(
            "You are analyzing a codebase structure. Use ONLY the provided digest as data. \
Do not invent files or behavior. Return a concise structural summary with key modules, \
language mix, and probable architecture boundaries.\n\nDIGEST:\n{digest}"
        )
    }

    async fn enrich_with_llm(&self, digest: &str) -> Option<String> {
        let provider = self.provider.as_ref()?;
        let request = LlmRequest {
            messages: vec![
                ChatMessage {
                    role: ChatRole::System,
                    content: "Produce a concise structural codebase summary.".to_string(),
                },
                ChatMessage {
                    role: ChatRole::User,
                    content: Self::build_prompt(digest),
                },
            ],
            max_tokens: Some(600),
            temperature: Some(1.0),
        };
        match provider.chat(request).await {
            Ok(response) => {
                let content = response.content.trim();
                if content.is_empty() {
                    tracing::warn!("AnalyzeAgent LLM returned empty content, using fallback");
                    None
                } else {
                    Some(content.to_string())
                }
            }
            Err(err) => {
                match &err {
                    LlmError::AuthFailed { .. } => tracing::warn!(
                        error = %err,
                        "AnalyzeAgent LLM enrichment auth failed, using fallback. Guidance: verify key type and endpoint mode (KIMI_AUTH_MODE=api|coding_plan)."
                    ),
                    LlmError::Transport { .. } => tracing::warn!(
                        error = %err,
                        "AnalyzeAgent LLM enrichment transport failed, using fallback. Guidance: verify KIMI_BASE_URL and network reachability (api.moonshot.ai/v1, api.moonshot.cn/v1, or api-sg.moonshot.ai/v1)."
                    ),
                    _ => tracing::warn!(
                        error = %err,
                        "AnalyzeAgent LLM enrichment failed, using fallback"
                    ),
                }
                None
            }
        }
    }
}

#[async_trait::async_trait]
impl Agent for AnalyzeAgent {
    fn name(&self) -> &str {
        "analyze"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability("analyze".to_string())]
    }

    async fn handle_message(
        &self,
        msg: AgentMessage,
        ctx: &AgentContext,
    ) -> Result<AgentResponse, AgentError> {
        let root_path = Self::parse_task_path(msg)?;
        tracing::info!(agent = %ctx.agent_name, path = %root_path.display(), "AnalyzeAgent started");

        let scan = self
            .scanner
            .scan(&root_path)
            .map_err(|e| AgentError::ExecutionFailed {
                source: Box::new(e),
            })?;

        let files: Vec<ScannedFile> = scan
            .files
            .into_iter()
            .filter(|f| !self.is_sensitive_path(&f.path))
            .collect();
        let file_tree = Self::build_file_tree(&root_path, &files);
        let language_stats = Self::build_language_stats(&files);

        let mut top_files = files.iter().collect::<Vec<_>>();
        top_files.sort_by(|a, b| {
            b.size_bytes
                .cmp(&a.size_bytes)
                .then_with(|| a.path.to_string_lossy().cmp(&b.path.to_string_lossy()))
        });

        let mut snippets: Vec<(PathBuf, String)> = Vec::new();
        for file in top_files.into_iter().take(MAX_SAMPLE_FILES) {
            match self.reader.read(&file.path) {
                Ok(content) => {
                    let mut snippet = content.content;
                    if snippet.len() > MAX_SNIPPET_BYTES {
                        snippet.truncate(MAX_SNIPPET_BYTES);
                    }
                    snippets.push((content.path, snippet));
                }
                Err(err) => {
                    tracing::warn!(path = %file.path.display(), error = %err, "AnalyzeAgent read skipped file");
                }
            }
        }

        let digest = Self::build_digest(&file_tree, &language_stats, &snippets);
        let llm_summary = self.enrich_with_llm(&digest).await;

        let mode = if llm_summary.is_some() {
            AnalysisMode::LlmEnriched
        } else {
            AnalysisMode::ToolOnlyFallback
        };

        let mode_marker = if mode == AnalysisMode::LlmEnriched {
            MODE_LLM_ENRICHED
        } else {
            MODE_TOOL_ONLY_FALLBACK
        };

        let result = AnalysisResult {
            mode,
            file_count: files.len(),
            truncated: scan.truncated,
            total_size_bytes: files.iter().map(|f| f.size_bytes).sum(),
            language_stats,
            file_tree,
            digest,
            llm_summary,
        };

        let mut metadata = HashMap::new();
        metadata.insert("mode".to_string(), mode_marker.to_string());

        let content = serde_json::to_string(&result).map_err(|e| AgentError::ExecutionFailed {
            source: Box::new(e),
        })?;

        tracing::info!(agent = %ctx.agent_name, mode = mode_marker, "AnalyzeAgent completed");
        Ok(AgentResponse::Success { content, metadata })
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

    use super::{AnalysisResult, AnalyzeAgent};

    struct MockProvider {
        should_fail: bool,
        summary: String,
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        fn provider_id(&self) -> &str {
            "mock"
        }

        fn model_id(&self) -> &str {
            "mock-model"
        }

        async fn chat(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            if self.should_fail {
                return Err(LlmError::InvalidResponse {
                    message: "mock failure".to_string(),
                });
            }
            Ok(LlmResponse {
                content: self.summary.clone(),
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 12,
                    total_tokens: 22,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-model".to_string()),
            })
        }
    }

    fn mk_ctx() -> AgentContext {
        AgentContext {
            agent_name: "test-analyze".to_string(),
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
        let path =
            std::env::temp_dir().join(format!("sunny_boys_{label}_{}_{}", std::process::id(), ts));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn parse_success(response: AgentResponse) -> (AnalysisResult, HashMap<String, String>) {
        match response {
            AgentResponse::Success { content, metadata } => {
                let parsed: AnalysisResult = serde_json::from_str(&content)
                    .expect("response must be valid AnalysisResult JSON");
                (parsed, metadata)
            }
            AgentResponse::Error { code, message } => {
                panic!("expected success, got error code={code}, message={message}")
            }
        }
    }

    #[test]
    fn test_analyze_agent_name() {
        let agent = AnalyzeAgent::new(None);
        assert_eq!(agent.name(), "analyze");
    }

    #[test]
    fn test_analyze_agent_capabilities() {
        let agent = AnalyzeAgent::new(None);
        assert_eq!(
            agent.capabilities(),
            vec![Capability("analyze".to_string())]
        );
    }

    #[test]
    fn test_analyze_agent_trait_is_object_safe() {
        fn _check_object_safe(_agent: Box<dyn Agent>) {}
        let agent = AnalyzeAgent::new(None);
        _check_object_safe(Box::new(agent));
    }

    #[tokio::test]
    async fn test_analyze_pipeline_with_mock_provider_success() {
        let dir = mk_temp_dir("provider_success");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write test file");

        let provider = Arc::new(MockProvider {
            should_fail: false,
            summary: "High-level architecture summary".to_string(),
        });
        let agent = AnalyzeAgent::new(Some(provider));

        let response = agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response");

        let (result, metadata) = parse_success(response);
        assert_eq!(
            metadata.get("mode").map(String::as_str),
            Some("LLM_ENRICHED")
        );
        assert_eq!(
            result.llm_summary.as_deref(),
            Some("High-level architecture summary")
        );
        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn test_analyze_pipeline_provider_fails_fallback() {
        let dir = mk_temp_dir("provider_fail");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write test file");

        let provider = Arc::new(MockProvider {
            should_fail: true,
            summary: "unused".to_string(),
        });
        let agent = AnalyzeAgent::new(Some(provider));

        let response = agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response");

        let (result, metadata) = parse_success(response);
        assert_eq!(
            metadata.get("mode").map(String::as_str),
            Some("TOOL_ONLY_FALLBACK")
        );
        assert!(result.llm_summary.is_none());
        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn test_analyze_pipeline_no_provider_fallback() {
        let dir = mk_temp_dir("no_provider");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write test file");

        let agent = AnalyzeAgent::new(None);
        let response = agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response");

        let (result, metadata) = parse_success(response);
        assert_eq!(
            metadata.get("mode").map(String::as_str),
            Some("TOOL_ONLY_FALLBACK")
        );
        assert!(result.llm_summary.is_none());
        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn test_analyze_pipeline_scans_fixture_directory() {
        let dir = mk_temp_dir("fixture_scan");
        fs::write(dir.join("lib.rs"), "pub fn a() {}\n").expect("write file");
        fs::write(dir.join("mod.rs"), "pub fn b() {}\n").expect("write file");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write file");

        let agent = AnalyzeAgent::new(None);
        let response = agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response");

        let (result, _) = parse_success(response);
        assert_eq!(result.file_count, 3);
        assert!(result
            .language_stats
            .iter()
            .any(|(lang, count)| lang == "rs" && *count == 3));
        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn test_analyze_pipeline_skips_sensitive_files() {
        let dir = mk_temp_dir("sensitive_skip");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write file");
        fs::write(dir.join(".env"), "SECRET_KEY=abc123\n").expect("write file");

        let agent = AnalyzeAgent::new(None);
        let response = agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response");

        let (result, _) = parse_success(response);
        let json = serde_json::to_string(&result).expect("serialize result");
        assert!(
            !json.contains(".env"),
            "analysis content must not include .env"
        );
        assert_eq!(result.file_count, 1);
        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }

    #[tokio::test]
    async fn test_analyze_pipeline_handles_empty_directory() {
        let dir = mk_temp_dir("empty_dir");

        let agent = AnalyzeAgent::new(None);
        let response = agent
            .handle_message(mk_msg(dir.to_str().expect("path str")), &mk_ctx())
            .await
            .expect("agent response");

        let (result, metadata) = parse_success(response);
        assert_eq!(
            metadata.get("mode").map(String::as_str),
            Some("TOOL_ONLY_FALLBACK")
        );
        assert_eq!(result.file_count, 0);
        assert_eq!(result.total_size_bytes, 0);
        fs::remove_dir_all(&dir).expect("cleanup temp dir");
    }
}
