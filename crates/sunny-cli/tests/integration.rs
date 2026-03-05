use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use sunny_boys::{AnalysisMode, AnalysisResult, AnalyzeAgent};
use sunny_core::agent::{AgentHandle, AgentMessage, AgentResponse, Capability};
use sunny_core::orchestrator::{AgentRegistry, OrchestratorHandle};
use sunny_core::tool::{FileReader, FileScanner};
use sunny_mind::{LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage};
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

pub struct MockProvider {
    response: Option<String>,
    error: Option<LlmError>,
}

impl MockProvider {
    fn with_response(response: impl Into<String>) -> Self {
        Self {
            response: Some(response.into()),
            error: None,
        }
    }

    fn with_error(error: LlmError) -> Self {
        Self {
            response: None,
            error: Some(error),
        }
    }
}

#[async_trait]
impl LlmProvider for MockProvider {
    fn provider_id(&self) -> &str {
        "mock"
    }

    fn model_id(&self) -> &str {
        "mock-model"
    }

    async fn chat(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
        match (&self.response, &self.error) {
            (Some(r), _) => Ok(LlmResponse {
                content: r.clone(),
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 15,
                    total_tokens: 25,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-model".to_string()),
            }),
            (_, Some(e)) => Err(clone_llm_error(e)),
            _ => Ok(LlmResponse {
                content: "mock summary".to_string(),
                usage: TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    total_tokens: 2,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-model".to_string()),
            }),
        }
    }
}

fn clone_llm_error(err: &LlmError) -> LlmError {
    match err {
        LlmError::AuthFailed { message } => LlmError::AuthFailed {
            message: message.clone(),
        },
        LlmError::Timeout { timeout_ms } => LlmError::Timeout {
            timeout_ms: *timeout_ms,
        },
        LlmError::RateLimited => LlmError::RateLimited,
        LlmError::InvalidResponse { message } => LlmError::InvalidResponse {
            message: message.clone(),
        },
        LlmError::Transport { source } => LlmError::Transport {
            source: Box::new(std::io::Error::other(source.to_string())),
        },
        LlmError::NotConfigured { message } => LlmError::NotConfigured {
            message: message.clone(),
        },
        LlmError::UnsupportedAuthMode { mode } => LlmError::UnsupportedAuthMode {
            mode: mode.clone(),
        },
    }
}

fn write_fixture_file(root: &Path, rel_path: &str, content: &str) {
    let full_path = root.join(rel_path);
    if let Some(parent) = full_path.parent() {
        std::fs::create_dir_all(parent).expect("create fixture parent dir");
    }
    std::fs::write(full_path, content).expect("write fixture file");
}

async fn run_pipeline(
    root: &Path,
    provider: Option<Arc<dyn LlmProvider>>,
    max_files: Option<usize>,
) -> (AnalysisResult, HashMap<String, String>) {
    let scanner = match max_files {
        Some(cap) => FileScanner {
            max_files: cap,
            ..Default::default()
        },
        None => FileScanner::default(),
    };

    let agent = AnalyzeAgent::with_tools(provider, scanner, FileReader::default());
    let token = CancellationToken::new();
    let handle = AgentHandle::spawn(Arc::new(agent), token.child_token());

    let mut registry = AgentRegistry::new();
    registry
        .register("analyze".into(), handle, vec![Capability("analyze".into())])
        .expect("register analyze agent");

    let orchestrator = OrchestratorHandle::spawn(registry, token.child_token());
    let msg = AgentMessage::Task {
        id: "integration-task".into(),
        content: root.to_string_lossy().to_string(),
        metadata: HashMap::new(),
    };

    let response = orchestrator
        .dispatch("analyze", msg)
        .await
        .expect("orchestrator dispatch succeeds");

    token.cancel();
    orchestrator
        .shutdown()
        .await
        .expect("orchestrator shutdown");

    match response {
        AgentResponse::Success { content, metadata } => {
            let result: AnalysisResult =
                serde_json::from_str(&content).expect("parse analysis result JSON");
            (result, metadata)
        }
        AgentResponse::Error { code, message } => {
            panic!("expected success response, got {code}: {message}")
        }
    }
}

#[tokio::test]
async fn test_e2e_analyze_tool_only_on_fixture() {
    let dir = tempdir().expect("create temp dir");
    write_fixture_file(dir.path(), "src/main.rs", "fn main() {}\n");
    write_fixture_file(dir.path(), "src/lib.rs", "pub fn lib() {}\n");
    write_fixture_file(dir.path(), "src/mod_a.rs", "pub fn a() {}\n");
    write_fixture_file(dir.path(), "src/mod_b.rs", "pub fn b() {}\n");
    write_fixture_file(dir.path(), "tests/basic.rs", "#[test] fn t() {}\n");

    let (result, metadata) = run_pipeline(dir.path(), None, None).await;

    assert_eq!(result.mode, AnalysisMode::ToolOnlyFallback);
    assert_eq!(
        metadata.get("mode").map(String::as_str),
        Some("TOOL_ONLY_FALLBACK")
    );
    assert_eq!(result.file_count, 5);
    assert!(result
        .language_stats
        .iter()
        .any(|(ext, count)| ext == "rs" && *count == 5));
    assert!(result.file_tree.contains("src/main.rs"));
    assert!(result.digest.contains("LANGUAGE_STATS"));
    assert!(result.digest.contains("- rs: 5"));
}

#[tokio::test]
async fn test_e2e_analyze_with_mock_provider() {
    let dir = tempdir().expect("create temp dir");
    write_fixture_file(dir.path(), "main.rs", "fn main() {}\n");

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_response("summary text"));
    let (result, metadata) = run_pipeline(dir.path(), Some(provider), None).await;

    assert_eq!(result.mode, AnalysisMode::LlmEnriched);
    assert_eq!(
        metadata.get("mode").map(String::as_str),
        Some("LLM_ENRICHED")
    );
    assert_eq!(result.llm_summary.as_deref(), Some("summary text"));
}

#[tokio::test]
async fn test_e2e_analyze_provider_failure_falls_back() {
    let dir = tempdir().expect("create temp dir");
    write_fixture_file(dir.path(), "main.rs", "fn main() {}\n");

    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::with_error(LlmError::Timeout {
        timeout_ms: 100,
    }));
    let (result, metadata) = run_pipeline(dir.path(), Some(provider), None).await;

    assert_eq!(result.mode, AnalysisMode::ToolOnlyFallback);
    assert_eq!(
        metadata.get("mode").map(String::as_str),
        Some("TOOL_ONLY_FALLBACK")
    );
    assert!(result.llm_summary.is_none());
}

#[tokio::test]
async fn test_e2e_analyze_empty_directory() {
    let dir = tempdir().expect("create temp dir");
    let (result, metadata) = run_pipeline(dir.path(), None, None).await;

    assert_eq!(result.mode, AnalysisMode::ToolOnlyFallback);
    assert_eq!(
        metadata.get("mode").map(String::as_str),
        Some("TOOL_ONLY_FALLBACK")
    );
    assert_eq!(result.file_count, 0);
    assert_eq!(result.total_size_bytes, 0);
    assert_eq!(result.file_tree, ".");
}

#[tokio::test]
async fn test_e2e_analyze_respects_denylist() {
    let dir = tempdir().expect("create temp dir");
    write_fixture_file(dir.path(), "src/main.rs", "fn main() {}\n");
    write_fixture_file(dir.path(), ".env", "SECRET=hidden\n");
    write_fixture_file(dir.path(), "secrets.key", "PRIVATE KEY\n");

    let (result, _) = run_pipeline(dir.path(), None, None).await;

    assert_eq!(result.file_count, 1);
    assert!(!result.file_tree.contains(".env"));
    assert!(!result.file_tree.contains("secrets.key"));
    assert!(!result.digest.contains(".env"));
    assert!(!result.digest.contains("secrets.key"));
}

#[tokio::test]
async fn test_e2e_analyze_large_directory_caps() {
    let dir = tempdir().expect("create temp dir");
    for i in 0..100 {
        write_fixture_file(dir.path(), &format!("src/file_{i}.rs"), "pub fn f() {}\n");
    }

    let (result, _) = run_pipeline(dir.path(), None, Some(10)).await;

    assert_eq!(result.file_count, 10);
    assert!(result.truncated);
}

#[tokio::test]
async fn test_e2e_json_output_valid() {
    let dir = tempdir().expect("create temp dir");
    write_fixture_file(dir.path(), "src/lib.rs", "pub fn lib() {}\n");

    let (result, _) = run_pipeline(dir.path(), None, None).await;
    let json = serde_json::to_string_pretty(&result).expect("serialize result to json");

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON output");
    assert!(parsed.is_object());
    assert!(parsed.get("file_count").is_some());
    assert!(parsed.get("mode").is_some());
}

#[tokio::test]
async fn test_e2e_orchestrator_dispatch_to_analyze_agent() {
    let dir = tempdir().expect("create temp dir");
    write_fixture_file(dir.path(), "src/main.rs", "fn main() {}\n");

    let token = CancellationToken::new();
    let agent = AnalyzeAgent::new(None);
    let handle = AgentHandle::spawn(Arc::new(agent), token.child_token());

    let mut registry = AgentRegistry::new();
    registry
        .register("analyze".into(), handle, vec![Capability("analyze".into())])
        .expect("register analyze agent");

    let orchestrator = OrchestratorHandle::spawn(registry, token.child_token());
    let msg = AgentMessage::Task {
        id: "dispatch-test".into(),
        content: dir.path().to_string_lossy().to_string(),
        metadata: HashMap::new(),
    };

    let response = orchestrator
        .dispatch("analyze", msg)
        .await
        .expect("dispatch analyze");

    token.cancel();
    orchestrator
        .shutdown()
        .await
        .expect("orchestrator shutdown");

    match response {
        AgentResponse::Success { content, metadata } => {
            let parsed: AnalysisResult =
                serde_json::from_str(&content).expect("analysis result JSON");
            assert!(parsed.file_count >= 1);
            assert!(metadata.contains_key("mode"));
        }
        AgentResponse::Error { code, message } => {
            panic!("dispatch returned error {code}: {message}")
        }
    }
}
