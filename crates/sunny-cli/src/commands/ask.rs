//! Ask command implementation

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::Args;
use tokio_util::sync::CancellationToken;

use sunny_boys::build_boys_registry;
use sunny_core::agent::{AgentMessage, AgentResponse, Capability};
use sunny_core::orchestrator::{
    HeuristicLoopPlanner, IntentClassifier, IntentKind, InteractiveOrchestrator, OrchestratorError,
    OrchestratorHandle, PlanPolicy, PlanningIntake, RequestId,
};
use sunny_mind::{KimiProvider, LlmProvider};

use crate::output::{
    format_prompt_json, format_prompt_pretty, format_prompt_text, PromptIssue, PromptOutput,
};

struct AskExecution {
    formatted: String,
    success: bool,
}

#[derive(Args, Debug)]
pub struct AskArgs {
    /// The user's ask text
    pub input: String,

    #[arg(long, default_value = "pretty", value_parser = ["text", "json", "pretty"])]
    pub format: String,

    #[arg(long)]
    pub dry_run: bool,

    #[arg(long)]
    pub no_llm: bool,
}

pub async fn run_ask(args: AskArgs) -> Result<(), Box<dyn std::error::Error>> {
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
                    "LLM provider not available: {e}. Ask execution continues without LLM. \
Guidance: set KIMI_API_KEY and optionally KIMI_AUTH_MODE=api|coding_plan"
                );
                None
            }
        }
    };

    let execution = execute_ask_internal(args, provider).await?;
    println!("{}", execution.formatted);
    if !execution.success {
        return Err(std::io::Error::other("ask command failed").into());
    }
    Ok(())
}

fn format_ask_error(
    format: &str,
    request_id: &str,
    intent_kind: &str,
    required_capability: &str,
    dry_run: bool,
    metadata: HashMap<String, String>,
    error: PromptIssue,
) -> AskExecution {
    AskExecution {
        formatted: render_error_output(
            format,
            request_id,
            intent_kind,
            required_capability,
            dry_run,
            metadata,
            error,
        ),
        success: false,
    }
}

fn resolve_query_root(input: &str, cwd: &Path) -> PathBuf {
    let trimmed = input.trim();
    let candidate = PathBuf::from(trimmed);
    let resolved = if candidate.is_absolute() {
        candidate
    } else {
        cwd.join(candidate)
    };

    if resolved.exists() {
        resolved
    } else {
        cwd.to_path_buf()
    }
}

fn issue(level: &str, code: &str, message: impl Into<String>, hint: Option<&str>) -> PromptIssue {
    PromptIssue {
        level: level.to_string(),
        code: code.to_string(),
        message: message.into(),
        hint: hint.map(str::to_string),
    }
}

fn insert_provider_metadata(
    metadata: &mut HashMap<String, String>,
    provider: Option<&Arc<dyn LlmProvider>>,
    no_llm: bool,
) {
    metadata.insert(
        "_sunny.provider.id".to_string(),
        provider
            .map(|provider| provider.provider_id().to_string())
            .unwrap_or_else(|| "none".to_string()),
    );
    metadata.insert(
        "_sunny.provider.model".to_string(),
        provider
            .map(|provider| provider.model_id().to_string())
            .unwrap_or_else(|| "none".to_string()),
    );
    metadata.insert(
        "_sunny.provider.mode".to_string(),
        if no_llm {
            "no_llm".to_string()
        } else {
            "llm_enabled".to_string()
        },
    );

    if provider.is_none() && !no_llm {
        metadata.insert("_sunny.degradation.active".to_string(), "true".to_string());
    }
}

fn map_orchestrator_error(err: &OrchestratorError) -> PromptIssue {
    match err {
        OrchestratorError::AgentUnresponsive => issue(
            "error",
            "agent_timeout",
            "Agent did not respond within timeout window",
            Some("Retry, or use --no-llm to bypass provider/tool loop"),
        ),
        OrchestratorError::AgentNotFound { name } => issue(
            "error",
            "agent_not_found",
            format!("Agent not found: {name}"),
            Some("Verify registry/capability wiring for ask route"),
        ),
        OrchestratorError::DispatchFailed { source } => issue(
            "error",
            "agent_execution_failed",
            source.to_string(),
            Some("Inspect logs with request_id for root cause"),
        ),
        OrchestratorError::ShuttingDown => issue(
            "error",
            "orchestrator_shutting_down",
            "Orchestrator is shutting down",
            Some("Retry the command after runtime initialization"),
        ),
        OrchestratorError::InvalidStepTransition { from, to } => issue(
            "error",
            "invalid_step_transition",
            format!("Invalid step transition: {from:?} -> {to:?}"),
            Some("Inspect orchestration state transitions and retry"),
        ),
        OrchestratorError::PlanPolicyViolation { reason } => issue(
            "error",
            "plan_policy_violation",
            reason.clone(),
            Some("Adjust input scope or orchestration policy limits"),
        ),
        OrchestratorError::Plan { source } => issue(
            "error",
            "plan_error",
            source.to_string(),
            Some("Inspect planner constraints and retry"),
        ),
    }
}

fn warnings_from_metadata(metadata: &HashMap<String, String>) -> Vec<PromptIssue> {
    let mut warnings = Vec::new();

    if metadata.get("mode").map(String::as_str) == Some("TOOL_ONLY_FALLBACK") {
        let reason = metadata
            .get("fallback_reason")
            .cloned()
            .unwrap_or_else(|| "unknown".to_string());
        let hint = if reason == "tool_loop_read_budget_exceeded" {
            "Tool loop hit read budget; narrow the question scope or rerun with --no-llm"
        } else {
            "Provider/tool loop was unavailable or timed out; output may be less focused"
        };
        warnings.push(issue(
            "warning",
            "fallback_mode",
            format!("Result generated via fallback execution path ({reason})"),
            Some(hint),
        ));

        if let Some(detail) = metadata.get("fallback_detail") {
            warnings.push(issue(
                "warning",
                "fallback_detail",
                format!("Fallback detail: {detail}"),
                Some("Inspect this detail and request_id logs for exact failing tool call"),
            ));
        }
    }

    if let Some(skipped_raw) = metadata.get("skipped_file_count") {
        match skipped_raw.parse::<usize>() {
            Ok(skipped) => {
                if skipped > 0 {
                    warnings.push(issue(
                        "warning",
                        "files_skipped",
                        format!("Skipped {skipped} file(s) due to read constraints"),
                        Some("Check logs for file paths and error kinds"),
                    ));
                }
            }
            Err(_) => warnings.push(issue(
                "warning",
                "invalid_skipped_file_count",
                format!("Invalid skipped_file_count metadata: '{skipped_raw}'"),
                Some("Check fallback metadata serialization in the responding agent"),
            )),
        }
    }

    warnings
}

#[derive(serde::Deserialize)]
struct QueryResultFile {
    path: String,
    truncated: bool,
}

#[derive(serde::Deserialize)]
struct QueryResultEnvelope {
    file_count: usize,
    total_size_bytes: u64,
    files: Vec<QueryResultFile>,
}

fn summarize_query_response(raw: &str) -> Option<String> {
    let parsed: QueryResultEnvelope = serde_json::from_str(raw).ok()?;
    let truncated_files = parsed.files.iter().filter(|f| f.truncated).count();

    let listed = parsed
        .files
        .iter()
        .take(8)
        .map(|f| format!("- {}", f.path))
        .collect::<Vec<_>>()
        .join("\n");

    let mut out = format!(
        "Scanned {} files ({} bytes). Included {} file snippets{}.",
        parsed.file_count,
        parsed.total_size_bytes,
        parsed.files.len(),
        if truncated_files > 0 {
            format!(", {} snippet(s) truncated", truncated_files)
        } else {
            String::new()
        }
    );

    if !listed.is_empty() {
        out.push_str("\n\nRepresentative files:\n");
        out.push_str(&listed);
    }

    Some(out)
}

fn render_error_output(
    format: &str,
    request_id: &str,
    intent_kind: &str,
    required_capability: &str,
    dry_run: bool,
    metadata: HashMap<String, String>,
    error: PromptIssue,
) -> String {
    let output = build_ask_output(AskOutputParams {
        request_id,
        intent_kind,
        required_capability,
        dry_run,
        outcome: "error",
        response: None,
        metadata,
        warnings: Vec::new(),
        error: Some(error),
    });
    format_output(format, &output)
}

#[cfg(test)]
pub(crate) async fn execute_ask(
    args: AskArgs,
    provider: Option<Arc<dyn LlmProvider>>,
) -> Result<String, Box<dyn std::error::Error>> {
    Ok(execute_ask_internal(args, provider).await?.formatted)
}

async fn execute_ask_internal(
    args: AskArgs,
    provider: Option<Arc<dyn LlmProvider>>,
) -> Result<AskExecution, Box<dyn std::error::Error>> {
    use sunny_core::orchestrator::{
        EVENT_CLI_COMMAND_END, EVENT_CLI_COMMAND_START, OUTCOME_ERROR, OUTCOME_SUCCESS,
    };

    let request_id = RequestId::new();
    let request_id_text = request_id.to_string();

    if args.input.trim().is_empty() {
        let mut metadata = HashMap::new();
        metadata.insert("failure_stage".to_string(), "input_validation".to_string());

        tracing::info!(
            name: EVENT_CLI_COMMAND_END,
            request_id = %request_id_text,
            outcome = OUTCOME_ERROR,
            "cli.command.end"
        );

        return Ok(format_ask_error(
            &args.format,
            &request_id_text,
            "invalid",
            "unknown",
            args.dry_run,
            metadata,
            issue(
                "error",
                "blank_input",
                "Ask input cannot be blank",
                Some("Provide a question, action, or path to inspect"),
            ),
        ));
    }

    let classifier = IntentClassifier::new();
    let intent = classifier.classify(&args.input);

    let intent_kind = match intent.kind {
        IntentKind::Analyze => "analyze",
        IntentKind::Query => "query",
        IntentKind::Action => "action",
    }
    .to_string();

    let required_capability = intent
        .required_capability
        .clone()
        .unwrap_or_else(|| Capability("query".into()));

    tracing::info!(
        name: EVENT_CLI_COMMAND_START,
        request_id = %request_id_text,
        intent_kind = %intent_kind,
        capability = %required_capability.0,
        "cli.command.start"
    );

    if args.dry_run {
        let output = build_ask_output(AskOutputParams {
            request_id: &request_id_text,
            intent_kind: &intent_kind,
            required_capability: &required_capability.0,
            dry_run: args.dry_run,
            outcome: "planned",
            response: None,
            metadata: HashMap::new(),
            warnings: Vec::new(),
            error: None,
        });

        tracing::info!(
            name: EVENT_CLI_COMMAND_END,
            request_id = %request_id_text,
            outcome = "dry_run",
            "cli.command.end"
        );

        return Ok(AskExecution {
            formatted: format_output(&args.format, &output),
            success: true,
        });
    }

    let llm_enabled = provider.is_some();
    let cancel = CancellationToken::new();
    let registry = match build_boys_registry(provider.clone(), &cancel) {
        Ok(registry) => registry,
        Err(err) => {
            let mut metadata = HashMap::new();
            metadata.insert("failure_stage".to_string(), "registry_setup".to_string());
            metadata.insert("error".to_string(), err.to_string());

            tracing::info!(
                name: EVENT_CLI_COMMAND_END,
                request_id = %request_id_text,
                outcome = OUTCOME_ERROR,
                "cli.command.end"
            );
            return Ok(format_ask_error(
                &args.format,
                &request_id_text,
                &intent_kind,
                &required_capability.0,
                args.dry_run,
                metadata,
                issue(
                    "error",
                    "registry_setup_failed",
                    "Failed to initialize ask agent registry",
                    Some("Inspect startup logs for provider/agent wiring issues"),
                ),
            ));
        }
    };

    let orchestrator = OrchestratorHandle::spawn(registry, cancel.child_token());

    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => {
            let mut metadata = HashMap::new();
            metadata.insert("failure_stage".to_string(), "cwd_resolution".to_string());
            metadata.insert("error".to_string(), err.to_string());
            tracing::info!(
                name: EVENT_CLI_COMMAND_END,
                request_id = %request_id_text,
                outcome = OUTCOME_ERROR,
                "cli.command.end"
            );
            return Ok(format_ask_error(
                &args.format,
                &request_id_text,
                &intent_kind,
                &required_capability.0,
                args.dry_run,
                metadata,
                issue(
                    "error",
                    "cwd_unavailable",
                    "Unable to resolve current working directory",
                    Some("Run ask from an existing workspace directory"),
                ),
            ));
        }
    };
    let query_root = if required_capability.0 == "query" {
        resolve_query_root(&args.input, &cwd)
    } else {
        cwd.clone()
    };

    let mut metadata = HashMap::new();
    metadata.insert(
        "_sunny.cwd".to_string(),
        query_root.to_string_lossy().to_string(),
    );
    metadata.insert("_sunny.query".to_string(), args.input.clone());
    metadata.insert("_sunny.request_id".to_string(), request_id_text.clone());

    let task_content = if required_capability.0 == "query" {
        query_root.to_string_lossy().to_string()
    } else {
        args.input.clone()
    };

    let task_msg = AgentMessage::Task {
        id: request_id_text.clone(),
        content: task_content,
        metadata,
    };

    tracing::info!(
        request_id = %request_id_text,
        intent_kind = %intent_kind,
        capability = %required_capability.0,
        cwd = %cwd.display(),
        query_len = args.input.len(),
        "ask.task.dispatch"
    );

    let planner = HeuristicLoopPlanner::new(
        PlanPolicy {
            max_depth: 3,
            max_steps: 16,
            max_retries: 1,
        },
        llm_enabled,
    );
    let interactive = InteractiveOrchestrator::new(&orchestrator, planner, PlanningIntake::new());

    let dispatch_result = interactive
        .execute(intent, task_msg, cancel.child_token(), request_id)
        .await;

    if let Err(err) = orchestrator.shutdown().await {
        tracing::warn!(request_id = %request_id_text, error = %err, "ask orchestrator shutdown failed");
    }

    let response = match dispatch_result {
        Ok(response) => response,
        Err(err) => {
            cancel.cancel();
            let mut metadata = HashMap::new();
            metadata.insert("failure_stage".to_string(), "agent_dispatch".to_string());
            metadata.insert("error".to_string(), err.to_string());
            if matches!(err, OrchestratorError::AgentUnresponsive) {
                metadata.insert("timeout_source".to_string(), "orchestrator".to_string());
            }

            tracing::info!(
                name: EVENT_CLI_COMMAND_END,
                request_id = %request_id_text,
                outcome = OUTCOME_ERROR,
                "cli.command.end"
            );
            return Ok(format_ask_error(
                &args.format,
                &request_id_text,
                &intent_kind,
                &required_capability.0,
                args.dry_run,
                metadata,
                map_orchestrator_error(&err),
            ));
        }
    };
    cancel.cancel();

    let (outcome, response_content, response_metadata, warnings, error) = match response {
        AgentResponse::Success {
            content,
            mut metadata,
        } => {
            let intake_verdict_label = metadata
                .get("_sunny.intake.verdict")
                .map(String::as_str)
                .unwrap_or("unknown");
            tracing::info!(
                event = "ask.intake.verdict",
                verdict = %intake_verdict_label,
                request_id = %request_id_text
            );

            let summary = if required_capability.0 == "query" {
                summarize_query_response(&content)
            } else {
                None
            };

            insert_provider_metadata(&mut metadata, provider.as_ref(), args.no_llm);

            if summary.is_some() {
                metadata.insert(
                    "response_format".to_string(),
                    "query_summary_from_structured_payload".to_string(),
                );
            }

            let warnings = warnings_from_metadata(&metadata);
            (
                "success".to_string(),
                Some(summary.unwrap_or(content)),
                metadata,
                warnings,
                None,
            )
        }
        AgentResponse::Error { code, message } => {
            tracing::warn!(
                request_id = %request_id_text,
                error_code = %code,
                error_message = %message,
                "agent returned error response"
            );
            let mut metadata = HashMap::new();
            metadata.insert("failure_stage".to_string(), "agent_response".to_string());
            metadata.insert("error_code".to_string(), code.clone());
            metadata.insert("error_message".to_string(), message.clone());
            (
                "error".to_string(),
                None,
                metadata,
                Vec::new(),
                Some(issue(
                    "error",
                    &code,
                    message,
                    Some("Inspect logs for the request_id and retry if transient"),
                )),
            )
        }
    };

    let output = build_ask_output(AskOutputParams {
        request_id: &request_id_text,
        intent_kind: &intent_kind,
        required_capability: &required_capability.0,
        dry_run: args.dry_run,
        outcome: &outcome,
        response: response_content,
        metadata: response_metadata,
        warnings,
        error,
    });

    let final_outcome = if outcome == "success" {
        OUTCOME_SUCCESS
    } else {
        OUTCOME_ERROR
    };
    tracing::info!(
        name: EVENT_CLI_COMMAND_END,
        request_id = %request_id_text,
        outcome = final_outcome,
        "cli.command.end"
    );

    Ok(AskExecution {
        formatted: format_output(&args.format, &output),
        success: outcome == "success",
    })
}

struct AskOutputParams<'a> {
    request_id: &'a str,
    intent_kind: &'a str,
    required_capability: &'a str,
    dry_run: bool,
    outcome: &'a str,
    response: Option<String>,
    metadata: HashMap<String, String>,
    warnings: Vec<PromptIssue>,
    error: Option<PromptIssue>,
}

fn build_ask_output(params: AskOutputParams<'_>) -> PromptOutput {
    let (steps_completed, steps_failed) = match params.outcome {
        "success" => (1, 0),
        "planned" => (0, 0),
        _ => (0, 1),
    };

    PromptOutput {
        request_id: params.request_id.to_string(),
        plan_id: params.request_id.to_string(),
        intent_kind: params.intent_kind.to_string(),
        required_capability: Some(params.required_capability.to_string()),
        dry_run: params.dry_run,
        step_count: 1,
        steps_completed,
        steps_failed,
        steps_skipped: 0,
        outcome: params.outcome.to_string(),
        response: params.response,
        warnings: params.warnings,
        error: params.error,
        metadata: params.metadata,
    }
}

fn format_output(format: &str, output: &PromptOutput) -> String {
    match format {
        "json" => format_prompt_json(output),
        "text" => format_prompt_text(output),
        _ => format_prompt_pretty(output),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::commands::analyze::{execute_analyze, AnalyzeArgs};
    use sunny_mind::{
        LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage,
    };

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
                content: "LLM review feedback".to_string(),
                usage: TokenUsage {
                    input_tokens: 5,
                    output_tokens: 7,
                    total_tokens: 12,
                },
                finish_reason: "stop".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-model".to_string()),
                tool_calls: None,
                reasoning_content: None,
            })
        }
    }

    fn extract_json_plan_id(output: &str) -> Option<String> {
        let parsed: serde_json::Value = serde_json::from_str(output).ok()?;
        parsed
            .get("plan_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
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
    fn test_ask_args_defaults() {
        let args = AskArgs {
            input: "hello".to_string(),
            format: "pretty".to_string(),
            dry_run: false,
            no_llm: false,
        };
        assert_eq!(args.input, "hello");
        assert_eq!(args.format, "pretty");
        assert!(!args.dry_run);
        assert!(!args.no_llm);
    }

    #[test]
    fn test_ask_args_with_format() {
        let args = AskArgs {
            input: "hello".to_string(),
            format: "json".to_string(),
            dry_run: false,
            no_llm: false,
        };
        assert_eq!(args.format, "json");
    }

    #[test]
    fn test_ask_args_with_dry_run() {
        let args = AskArgs {
            input: "hello".to_string(),
            format: "pretty".to_string(),
            dry_run: true,
            no_llm: false,
        };
        assert!(args.dry_run);
    }

    #[test]
    fn test_ask_args_with_no_llm() {
        let args = AskArgs {
            input: "hello".to_string(),
            format: "pretty".to_string(),
            dry_run: false,
            no_llm: true,
        };
        assert!(args.no_llm);
    }

    #[test]
    fn test_ask_args_all_flags() {
        let args = AskArgs {
            input: "hello world".to_string(),
            format: "text".to_string(),
            dry_run: true,
            no_llm: true,
        };
        assert_eq!(args.input, "hello world");
        assert_eq!(args.format, "text");
        assert!(args.dry_run);
        assert!(args.no_llm);
    }

    #[tokio::test]
    async fn test_run_ask_stub() {
        let args = AskArgs {
            input: "test".to_string(),
            format: "pretty".to_string(),
            dry_run: true, // Use dry_run to avoid actual agent execution
            no_llm: false,
        };
        let result = run_ask(args).await;
        assert!(result.is_ok(), "stub should succeed");
    }

    #[tokio::test]
    async fn test_execute_ask_json_contains_plan_id() {
        let args = AskArgs {
            input: "analyze this request".to_string(),
            format: "json".to_string(),
            dry_run: false,
            no_llm: true,
        };

        let output = execute_ask(args, None).await.expect("ask should succeed");
        let plan_id = extract_json_plan_id(&output);
        assert!(
            plan_id.is_some(),
            "plan_id must be present in output: {output}"
        );
    }

    #[tokio::test]
    async fn test_ask_metadata_includes_provider_info() {
        let args = AskArgs {
            input: "review this code snippet".to_string(),
            format: "json".to_string(),
            dry_run: false,
            no_llm: false,
        };

        let output = execute_ask(args, Some(Arc::new(MockProvider)))
            .await
            .expect("ask should succeed with provider metadata");
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");

        assert_eq!(parsed["metadata"]["_sunny.provider.id"], "mock");
        assert_eq!(parsed["metadata"]["_sunny.provider.model"], "mock-model");
        assert_eq!(parsed["metadata"]["_sunny.provider.mode"], "llm_enabled");
        assert!(
            parsed["metadata"]["_sunny.degradation.active"].is_null(),
            "degradation marker should be absent when provider is available"
        );
    }

    #[tokio::test]
    async fn test_ask_output_includes_intake_verdict() {
        let args = AskArgs {
            input: "analyze this request".to_string(),
            format: "json".to_string(),
            dry_run: false,
            no_llm: true,
        };

        let output = execute_ask(args, None)
            .await
            .expect("ask should succeed with intake metadata");
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");

        assert_eq!(parsed["metadata"]["_sunny.intake.verdict"], "proceed");
        assert_eq!(parsed["metadata"]["_sunny.provider.mode"], "no_llm");
    }

    #[tokio::test]
    async fn test_ask_metadata_degradation_marker() {
        let args = AskArgs {
            input: "review this code snippet".to_string(),
            format: "json".to_string(),
            dry_run: false,
            no_llm: false,
        };

        let output = execute_ask(args, None)
            .await
            .expect("ask should succeed without configured provider");
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");

        assert_eq!(parsed["metadata"]["_sunny.provider.id"], "none");
        assert_eq!(parsed["metadata"]["_sunny.provider.model"], "none");
        assert_eq!(parsed["metadata"]["_sunny.provider.mode"], "llm_enabled");
        assert_eq!(parsed["metadata"]["_sunny.degradation.active"], "true");
    }

    #[tokio::test]
    async fn test_sunny_analyze_path_still_works() {
        let temp_dir = mk_temp_dir("ask_regression_analyze");
        fs::write(temp_dir.join("main.rs"), "fn main() {}\n").expect("write sample file");

        let args = AnalyzeArgs {
            path: PathBuf::from(&temp_dir),
            format: "text".to_string(),
            no_llm: true,
        };

        let output = execute_analyze(args, None)
            .await
            .expect("analyze should keep working");
        assert!(
            output.contains("TOOL_ONLY_FALLBACK"),
            "expected analyze output marker, got: {output}"
        );

        fs::remove_dir_all(&temp_dir).expect("remove temp dir");
    }

    #[tokio::test]
    async fn test_ask_dry_run_returns_plan_without_execution() {
        let args = AskArgs {
            input: "analyze this code".to_string(),
            format: "json".to_string(),
            dry_run: true,
            no_llm: true,
        };

        let output = execute_ask(args, None)
            .await
            .expect("dry-run should succeed");

        assert!(
            output.contains("\"dry_run\": true"),
            "dry_run flag should be true"
        );
        assert!(output.contains("\"plan_id\""), "plan_id should be present");
        assert!(
            output.contains("\"intent_kind\""),
            "intent_kind should be present"
        );
    }

    #[tokio::test]
    async fn test_ask_dry_run_no_intake_metadata() {
        let args = AskArgs {
            input: "test".to_string(),
            format: "json".to_string(),
            dry_run: true,
            no_llm: true,
        };

        let output = execute_ask(args, None)
            .await
            .expect("dry-run should succeed without intake metadata");
        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");

        let metadata = parsed["metadata"]
            .as_object()
            .expect("metadata should be an object");
        assert!(
            !metadata.contains_key("_sunny.intake.verdict"),
            "dry-run should not include intake verdict metadata"
        );
        assert!(
            !metadata.contains_key("_sunny.intake.skip_reason"),
            "dry-run should not include intake skip metadata"
        );
    }

    #[tokio::test]
    async fn test_ask_provider_path_no_llm() {
        // Verifies that when no_llm=true, provider is None and execution still succeeds
        // Uses dry_run to avoid actual agent execution
        let args = AskArgs {
            input: "test without llm".to_string(),
            format: "json".to_string(),
            dry_run: true,
            no_llm: true,
        };

        // Execute with no_llm=true - provider should be None internally
        let output = execute_ask(args, None)
            .await
            .expect("ask should succeed without provider");

        // Verify output is valid JSON with expected fields
        assert!(
            output.contains("\"request_id\""),
            "request_id should be present"
        );
        assert!(output.contains("\"plan_id\""), "plan_id should be present");
        assert!(output.contains("\"outcome\""), "outcome should be present");
    }

    #[tokio::test]
    async fn test_execute_ask_without_provider_injection() {
        let args = AskArgs {
            input: "test with direct none injection".to_string(),
            format: "json".to_string(),
            dry_run: true,
            no_llm: false,
        };

        let output = execute_ask(args, None)
            .await
            .expect("ask should succeed even with missing env");

        // Verify output is valid
        assert!(
            output.contains("\"request_id\""),
            "request_id should be present"
        );
        assert!(output.contains("\"plan_id\""), "plan_id should be present");
    }

    #[tokio::test]
    async fn test_execute_ask_with_real_agents_analyze() {
        let args = AskArgs {
            input: "review this code snippet".to_string(),
            format: "json".to_string(),
            dry_run: false,
            no_llm: true,
        };

        let output = execute_ask(args, None)
            .await
            .expect("ask with real agents should succeed");

        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");

        assert_eq!(parsed["intent_kind"], "analyze");
        assert_eq!(parsed["required_capability"], "analyze");
        assert_eq!(parsed["outcome"], "success");
        assert_eq!(parsed["steps_completed"], 1);
        assert!(parsed["response"].is_string(), "response should be present");
        assert!(
            parsed["response"]
                .as_str()
                .is_some_and(|r| r.contains("REVIEW FEEDBACK")),
            "response should contain ReviewAgent output"
        );
    }

    #[tokio::test]
    async fn test_execute_ask_query_uses_workspace_context() {
        let args = AskArgs {
            input: "Inspect current codebase".to_string(),
            format: "json".to_string(),
            dry_run: false,
            no_llm: true,
        };

        let output = execute_ask(args, None)
            .await
            .expect("query ask should succeed using current workspace path");

        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");

        assert_eq!(parsed["intent_kind"], "query");
        assert_eq!(parsed["required_capability"], "query");
        assert_eq!(parsed["outcome"], "success");
        assert!(
            parsed["response"].is_string(),
            "response should be present for query ask"
        );
    }

    #[tokio::test]
    async fn test_execute_ask_with_real_agents_action() {
        let args = AskArgs {
            input: "create a deployment plan".to_string(),
            format: "json".to_string(),
            dry_run: false,
            no_llm: true,
        };

        let output = execute_ask(args, None)
            .await
            .expect("action ask should succeed");

        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");

        assert_eq!(parsed["intent_kind"], "action");
        assert_eq!(parsed["required_capability"], "action");
        assert_eq!(parsed["outcome"], "success");
        assert!(
            parsed["response"]
                .as_str()
                .is_some_and(|r| r.contains("CRITIQUE REPORT")),
            "response should contain CritiqueAgent output"
        );
    }

    #[tokio::test]
    async fn test_execute_ask_dry_run_skips_agent_dispatch() {
        let args = AskArgs {
            input: "analyze something".to_string(),
            format: "json".to_string(),
            dry_run: true,
            no_llm: true,
        };

        let output = execute_ask(args, None)
            .await
            .expect("dry-run should succeed");

        let parsed: serde_json::Value =
            serde_json::from_str(&output).expect("output should be valid JSON");

        assert_eq!(parsed["outcome"], "planned");
        assert_eq!(parsed["steps_completed"], 0);
        assert!(
            parsed["response"].is_null(),
            "dry-run should have no response"
        );
    }

    #[test]
    fn test_render_error_output_contains_structured_error_fields() {
        let mut metadata = HashMap::new();
        metadata.insert("failure_stage".to_string(), "agent_dispatch".to_string());

        let rendered = render_error_output(
            "json",
            "req-err",
            "query",
            "query",
            false,
            metadata,
            issue(
                "error",
                "agent_timeout",
                "Agent did not respond within timeout window",
                Some("Retry with --no-llm"),
            ),
        );

        let parsed: serde_json::Value =
            serde_json::from_str(&rendered).expect("output should be valid JSON");

        assert_eq!(parsed["outcome"], "error");
        assert_eq!(parsed["error"]["code"], "agent_timeout");
        assert_eq!(
            parsed["error"]["message"],
            "Agent did not respond within timeout window"
        );
        assert_eq!(parsed["metadata"]["failure_stage"], "agent_dispatch");
    }

    #[test]
    fn test_warnings_from_metadata_for_fallback_and_skips() {
        let mut metadata = HashMap::new();
        metadata.insert("mode".to_string(), "TOOL_ONLY_FALLBACK".to_string());
        metadata.insert(
            "fallback_reason".to_string(),
            "tool_loop_timeout".to_string(),
        );
        metadata.insert("skipped_file_count".to_string(), "2".to_string());

        let warnings = warnings_from_metadata(&metadata);
        assert_eq!(warnings.len(), 2);
        assert_eq!(warnings[0].code, "fallback_mode");
        assert_eq!(warnings[1].code, "files_skipped");
    }

    #[test]
    fn test_warnings_from_metadata_reports_invalid_skipped_file_count() {
        let mut metadata = HashMap::new();
        metadata.insert("skipped_file_count".to_string(), "many".to_string());

        let warnings = warnings_from_metadata(&metadata);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, "invalid_skipped_file_count");
    }

    #[test]
    fn test_summarize_query_response_renders_compact_output() {
        let raw = r#"{"file_count":12,"total_size_bytes":1024,"files":[{"path":"a.rs","content":"x","truncated":false},{"path":"b.rs","content":"y","truncated":true}]}"#;
        let summary = summarize_query_response(raw).expect("should parse structured payload");
        assert!(summary.contains("Scanned 12 files (1024 bytes)."));
        assert!(summary.contains("Representative files"));
        assert!(summary.contains("- a.rs"));
        assert!(summary.contains("- b.rs"));
    }

    #[test]
    fn test_summarize_query_response_returns_none_for_non_json() {
        assert!(summarize_query_response("plain text response").is_none());
    }
}
