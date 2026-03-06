//! Analyze command implementation

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use clap::Args;
use tokio_util::sync::CancellationToken;

use sunny_boys::{AnalysisMode, AnalysisResult, AnalyzeAgent};
use sunny_core::agent::{AgentHandle, AgentMessage, AgentResponse, Capability};
use sunny_core::orchestrator::{AgentRegistry, OrchestratorHandle};
use sunny_mind::{KimiProvider, LlmProvider};

use crate::output::{format_json, format_pretty, format_text, AnalysisOutput, OutputMode};

#[derive(Args, Debug)]
pub struct AnalyzeArgs {
    /// Path to analyze
    pub path: PathBuf,

    #[arg(long, default_value = "pretty", value_parser = ["text", "json", "pretty"])]
    pub format: String,

    #[arg(long)]
    pub no_llm: bool,
}

pub async fn run_analyze(args: AnalyzeArgs) -> Result<(), Box<dyn std::error::Error>> {
    let provider: Option<Arc<dyn LlmProvider>> = if args.no_llm {
        None
    } else {
        match KimiProvider::from_env() {
            Ok(p) => {
                tracing::info!(
                    auth_mode = p.auth_mode(),
                    model = p.model_id(),
                    "Kimi provider initialized"
                );
                Some(Arc::new(p) as Arc<dyn LlmProvider>)
            }
            Err(e) => {
                tracing::warn!(
                    "LLM provider not available: {e}. Fallback to tool-only. \
Guidance: set KIMI_API_KEY and optionally KIMI_AUTH_MODE=api|coding_plan"
                );
                None
            }
        }
    };

    let output = execute_analyze(args, provider).await?;
    println!("{output}");
    Ok(())
}

pub(crate) async fn execute_analyze(
    args: AnalyzeArgs,
    provider: Option<Arc<dyn LlmProvider>>,
) -> Result<String, Box<dyn std::error::Error>> {
    if !args.path.exists() {
        return Err(format!("Path does not exist: {}", args.path.display()).into());
    }

    let agent = AnalyzeAgent::new(provider);
    let token = CancellationToken::new();
    let agent_handle = AgentHandle::spawn(Arc::new(agent), token.child_token());

    let mut registry = AgentRegistry::new();
    registry.register(
        "analyze".into(),
        agent_handle,
        vec![Capability("analyze".into())],
    )?;
    let orchestrator = OrchestratorHandle::spawn(registry, token.child_token());

    let msg = AgentMessage::Task {
        id: "analyze-1".to_string(),
        content: args.path.to_string_lossy().to_string(),
        metadata: HashMap::new(),
    };
    let response = orchestrator.dispatch("analyze", msg).await?;

    token.cancel();
    orchestrator
        .shutdown()
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e })?;

    match response {
        AgentResponse::Success { content, metadata } => {
            let result: AnalysisResult = serde_json::from_str(&content)?;
            let output = build_analysis_output(result, &metadata);
            let formatted = match args.format.as_str() {
                "json" => format_json(&output),
                "text" => format_text(&output),
                _ => format_pretty(&output),
            };
            Ok(formatted)
        }
        AgentResponse::Error { code, message } => {
            Err(format!("Analysis failed [{code}]: {message}").into())
        }
    }
}

fn build_analysis_output(
    result: AnalysisResult,
    _metadata: &HashMap<String, String>,
) -> AnalysisOutput {
    let mode = match result.mode {
        AnalysisMode::LlmEnriched => OutputMode::LlmEnriched,
        AnalysisMode::ToolOnlyFallback => OutputMode::ToolOnlyFallback,
    };

    let summary = format!(
        "Analyzed {} files ({} bytes)",
        result.file_count, result.total_size_bytes
    );

    AnalysisOutput {
        mode,
        summary,
        file_count: result.file_count,
        total_size_bytes: result.total_size_bytes,
        language_stats: result.language_stats,
        llm_summary: result.llm_summary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use clap::Parser;
    use sunny_mind::{
        LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage,
    };

    struct MockProvider {
        response_text: String,
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
            Ok(LlmResponse {
                content: self.response_text.clone(),
                usage: TokenUsage {
                    input_tokens: 10,
                    output_tokens: 12,
                    total_tokens: 22,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-model".to_string()),
                tool_calls: None,
            })
        }
    }

    #[derive(Parser, Debug)]
    struct TestCli {
        #[command(subcommand)]
        cmd: TestCmd,
    }

    #[derive(Parser, Debug)]
    enum TestCmd {
        Analyze(AnalyzeArgs),
    }

    fn mk_temp_dir(label: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("sunny_cli_{label}_{}_{}", std::process::id(), ts));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn test_analyze_args_default_format() {
        let args = TestCli::try_parse_from(["test", "analyze", "."]).expect("should parse");
        match args.cmd {
            TestCmd::Analyze(a) => {
                assert_eq!(a.path, PathBuf::from("."));
                assert_eq!(a.format, "pretty");
                assert!(!a.no_llm);
            }
        }
    }

    #[test]
    fn test_analyze_args_json_format() {
        let args = TestCli::try_parse_from(["test", "analyze", ".", "--format", "json"])
            .expect("should parse");
        match args.cmd {
            TestCmd::Analyze(a) => {
                assert_eq!(a.format, "json");
            }
        }
    }

    #[test]
    fn test_analyze_args_no_llm() {
        let args =
            TestCli::try_parse_from(["test", "analyze", ".", "--no-llm"]).expect("should parse");
        match args.cmd {
            TestCmd::Analyze(a) => {
                assert!(a.no_llm);
            }
        }
    }

    #[test]
    fn test_analyze_args_missing_path() {
        let result = TestCli::try_parse_from(["test", "analyze"]);
        assert!(result.is_err(), "should fail when path is missing");
    }

    #[test]
    fn test_output_format_text_contains_marker() {
        use crate::output::{format_text, AnalysisOutput, OutputMode};

        let output = AnalysisOutput {
            mode: OutputMode::LlmEnriched,
            summary: "Test summary".to_string(),
            file_count: 5,
            total_size_bytes: 1024,
            language_stats: vec![("rs".to_string(), 3), ("toml".to_string(), 2)],
            llm_summary: Some("LLM analysis".to_string()),
        };

        let formatted = format_text(&output);
        assert!(
            formatted.contains("LLM_ENRICHED") || formatted.contains("TOOL_ONLY_FALLBACK"),
            "text output should contain mode marker"
        );
    }

    #[test]
    fn test_output_format_json_valid() {
        use crate::output::{format_json, AnalysisOutput, OutputMode};

        let output = AnalysisOutput {
            mode: OutputMode::LlmEnriched,
            summary: "Test summary".to_string(),
            file_count: 5,
            total_size_bytes: 1024,
            language_stats: vec![("rs".to_string(), 3)],
            llm_summary: Some("LLM analysis".to_string()),
        };

        let formatted = format_json(&output);
        let parsed: serde_json::Value =
            serde_json::from_str(&formatted).expect("should be valid JSON");

        assert!(parsed.is_object());
        assert!(parsed["summary"].is_string());
        assert!(parsed["file_count"].is_number());
    }

    #[tokio::test]
    async fn test_run_analyze_with_mock_provider() {
        let dir = mk_temp_dir("mock_provider");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write");
        fs::write(dir.join("lib.rs"), "pub fn hello() {}\n").expect("write");

        let provider: Option<Arc<dyn LlmProvider>> = Some(Arc::new(MockProvider {
            response_text: "Well-structured Rust project".to_string(),
        }));

        let args = AnalyzeArgs {
            path: dir.clone(),
            format: "text".to_string(),
            no_llm: false,
        };

        let output = execute_analyze(args, provider)
            .await
            .expect("should succeed");

        assert!(
            output.contains("LLM_ENRICHED"),
            "output should be LLM_ENRICHED mode, got: {output}"
        );
        assert!(
            output.contains("Well-structured Rust project"),
            "output should contain LLM summary"
        );
        assert!(
            output.contains("cloud LLM provider"),
            "output should contain cloud disclosure"
        );

        fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[tokio::test]
    async fn test_run_analyze_no_llm_flag() {
        let dir = mk_temp_dir("no_llm");
        fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write");

        let args = AnalyzeArgs {
            path: dir.clone(),
            format: "text".to_string(),
            no_llm: true,
        };

        let output = execute_analyze(args, None).await.expect("should succeed");

        assert!(
            output.contains("TOOL_ONLY_FALLBACK"),
            "output should be TOOL_ONLY_FALLBACK mode, got: {output}"
        );
        assert!(
            !output.contains("cloud LLM provider"),
            "no-llm should not mention cloud provider"
        );

        fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[tokio::test]
    async fn test_run_analyze_nonexistent_path() {
        let args = AnalyzeArgs {
            path: PathBuf::from("/nonexistent/path/does/not/exist"),
            format: "text".to_string(),
            no_llm: true,
        };

        let result = execute_analyze(args, None).await;
        assert!(result.is_err(), "nonexistent path should return error");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("does not exist"),
            "error should mention path doesn't exist, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_run_analyze_json_format_produces_valid_json() {
        let dir = mk_temp_dir("json_format");
        fs::write(dir.join("app.rs"), "fn main() {}\n").expect("write");

        let args = AnalyzeArgs {
            path: dir.clone(),
            format: "json".to_string(),
            no_llm: true,
        };

        let output = execute_analyze(args, None).await.expect("should succeed");

        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");
        assert!(parsed.is_object());
        assert!(parsed["file_count"].is_number());
        assert!(parsed["mode"].is_string());
        assert_eq!(parsed["mode"].as_str(), Some("ToolOnlyFallback"));

        fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[tokio::test]
    async fn test_run_analyze_pretty_format_contains_report_header() {
        let dir = mk_temp_dir("pretty_format");
        fs::write(dir.join("lib.rs"), "pub fn a() {}\n").expect("write");

        let args = AnalyzeArgs {
            path: dir.clone(),
            format: "pretty".to_string(),
            no_llm: true,
        };

        let output = execute_analyze(args, None).await.expect("should succeed");
        assert!(
            output.contains("Code Analysis Report"),
            "pretty output should contain report header"
        );

        fs::remove_dir_all(&dir).expect("cleanup");
    }

    #[test]
    fn test_build_analysis_output_maps_modes_correctly() {
        let result = AnalysisResult {
            mode: AnalysisMode::ToolOnlyFallback,
            file_count: 3,
            truncated: false,
            total_size_bytes: 500,
            language_stats: vec![("rs".to_string(), 3)],
            file_tree: ".".to_string(),
            digest: "test digest".to_string(),
            llm_summary: None,
        };

        let output = build_analysis_output(result, &HashMap::new());
        assert_eq!(output.mode, OutputMode::ToolOnlyFallback);
        assert_eq!(output.file_count, 3);
        assert!(output.summary.contains("3 files"));
        assert!(output.llm_summary.is_none());
    }

    #[test]
    fn test_build_analysis_output_llm_enriched() {
        let result = AnalysisResult {
            mode: AnalysisMode::LlmEnriched,
            file_count: 10,
            truncated: false,
            total_size_bytes: 5000,
            language_stats: vec![("rs".to_string(), 8), ("toml".to_string(), 2)],
            file_tree: ".".to_string(),
            digest: "test digest".to_string(),
            llm_summary: Some("Great code".to_string()),
        };

        let output = build_analysis_output(result, &HashMap::new());
        assert_eq!(output.mode, OutputMode::LlmEnriched);
        assert_eq!(output.llm_summary.as_deref(), Some("Great code"));
    }
}
