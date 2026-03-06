//! Prompt command implementation

use std::collections::HashMap;
use std::sync::Arc;

use clap::Args;
use tokio_util::sync::CancellationToken;

use sunny_core::agent::{AgentHandle, Capability, EchoAgent};
use sunny_core::orchestrator::{
    AgentRegistry, ExecutionPlan, IntentClassifier, IntentKind, OrchestratorHandle, PlanExecutor,
    PlanId, PlanOutcome, PlanPolicy, PlanStep, RequestId,
};
use sunny_mind::{KimiProvider, LlmProvider};

use crate::output::{format_prompt_json, format_prompt_pretty, format_prompt_text, PromptOutput};

#[derive(Args, Debug)]
pub struct PromptArgs {
    /// The user's prompt text
    pub input: String,

    #[arg(long, default_value = "pretty", value_parser = ["text", "json", "pretty"])]
    pub format: String,

    #[arg(long)]
    pub dry_run: bool,

    #[arg(long)]
    pub no_llm: bool,
}

pub async fn run_prompt(args: PromptArgs) -> Result<(), Box<dyn std::error::Error>> {
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
                    "LLM provider not available: {e}. Prompt execution continues without LLM. \
Guidance: set KIMI_API_KEY and optionally KIMI_AUTH_MODE=api|coding_plan"
                );
                None
            }
        }
    };

    let output = execute_prompt(args, provider).await?;
    println!("{output}");
    Ok(())
}

pub(crate) async fn execute_prompt(
    args: PromptArgs,
    _provider: Option<Arc<dyn LlmProvider>>,
) -> Result<String, Box<dyn std::error::Error>> {
    use sunny_core::orchestrator::{
        EVENT_CLI_COMMAND_END, EVENT_CLI_COMMAND_START, OUTCOME_SUCCESS,
    };

    let classifier = IntentClassifier::new();
    let intent = classifier.classify(&args.input);

    let request_id = RequestId::new();
    let request_id_text = request_id.to_string();
    let plan_id = PlanId(request_id.0).to_string();

    tracing::info!(
        name: EVENT_CLI_COMMAND_START,
        request_id = %request_id_text,
        "cli.command.start"
    );

    let mut plan = ExecutionPlan::new(
        plan_id.clone(),
        request_id_text.clone(),
        intent.clone(),
        PlanPolicy::default(),
    );
    plan.add_step(PlanStep::new(
        "step-1".to_string(),
        args.input.clone(),
        intent.required_capability.clone(),
        5_000,
    ))?;

    let mut output = build_prompt_output(&request_id_text, &plan, args.dry_run, None, None);

    if args.dry_run {
        tracing::info!(
            name: EVENT_CLI_COMMAND_END,
            request_id = %request_id_text,
            outcome = "dry_run",
            "cli.command.end"
        );
    } else {
        let token = CancellationToken::new();
        let agent_handle = AgentHandle::spawn(Arc::new(EchoAgent), token.child_token());

        let mut registry = AgentRegistry::new();
        registry.register(
            "prompt-agent".into(),
            agent_handle,
            vec![
                Capability("analyze".into()),
                Capability("query".into()),
                Capability("action".into()),
            ],
        )?;

        let orchestrator = OrchestratorHandle::spawn(registry, token.child_token());
        let executor = PlanExecutor::new(&orchestrator);
        let result = executor.execute(&mut plan, token.child_token()).await?;

        let response = match plan.steps.first().and_then(|step| step.outcome.as_ref()) {
            Some(sunny_core::orchestrator::StepOutcome::Success { content }) => {
                Some(content.clone())
            }
            _ => None,
        };

        output = build_prompt_output(
            &request_id_text,
            &plan,
            args.dry_run,
            Some(result),
            response,
        );

        token.cancel();
        orchestrator
            .shutdown()
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { e })?;

        tracing::info!(
            name: EVENT_CLI_COMMAND_END,
            request_id = %request_id_text,
            outcome = OUTCOME_SUCCESS,
            "cli.command.end"
        );
    }

    let formatted = match args.format.as_str() {
        "json" => format_prompt_json(&output),
        "text" => format_prompt_text(&output),
        _ => format_prompt_pretty(&output),
    };

    Ok(formatted)
}

fn build_prompt_output(
    request_id: &str,
    plan: &ExecutionPlan,
    dry_run: bool,
    result: Option<sunny_core::orchestrator::PlanResult>,
    response: Option<String>,
) -> PromptOutput {
    let intent_kind = match plan.intent.kind {
        IntentKind::Analyze => "analyze",
        IntentKind::Query => "query",
        IntentKind::Action => "action",
    }
    .to_string();

    let required_capability = plan
        .intent
        .required_capability
        .as_ref()
        .map(|cap| cap.0.clone());

    let (outcome, steps_completed, steps_failed, steps_skipped) = match result {
        Some(result) => {
            let outcome = match result.overall_outcome {
                PlanOutcome::Success => "success",
                PlanOutcome::Failed => "failed",
                PlanOutcome::Cancelled => "cancelled",
            }
            .to_string();
            (
                outcome,
                result.steps_completed,
                result.steps_failed,
                result.steps_skipped,
            )
        }
        None => ("planned".to_string(), 0, 0, 0),
    };

    PromptOutput {
        request_id: request_id.to_string(),
        plan_id: plan.plan_id.clone(),
        intent_kind,
        required_capability,
        dry_run,
        step_count: plan.step_count(),
        steps_completed,
        steps_failed,
        steps_skipped,
        outcome,
        response,
        metadata: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::commands::analyze::{execute_analyze, AnalyzeArgs};

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
    fn test_prompt_args_defaults() {
        let args = PromptArgs {
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
    fn test_prompt_args_with_format() {
        let args = PromptArgs {
            input: "hello".to_string(),
            format: "json".to_string(),
            dry_run: false,
            no_llm: false,
        };
        assert_eq!(args.format, "json");
    }

    #[test]
    fn test_prompt_args_with_dry_run() {
        let args = PromptArgs {
            input: "hello".to_string(),
            format: "pretty".to_string(),
            dry_run: true,
            no_llm: false,
        };
        assert!(args.dry_run);
    }

    #[test]
    fn test_prompt_args_with_no_llm() {
        let args = PromptArgs {
            input: "hello".to_string(),
            format: "pretty".to_string(),
            dry_run: false,
            no_llm: true,
        };
        assert!(args.no_llm);
    }

    #[test]
    fn test_prompt_args_all_flags() {
        let args = PromptArgs {
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
    async fn test_run_prompt_stub() {
        let args = PromptArgs {
            input: "test".to_string(),
            format: "pretty".to_string(),
            dry_run: false,
            no_llm: false,
        };
        let result = run_prompt(args).await;
        assert!(result.is_ok(), "stub should succeed");
    }

    #[tokio::test]
    async fn test_execute_prompt_json_contains_plan_id() {
        let args = PromptArgs {
            input: "analyze this request".to_string(),
            format: "json".to_string(),
            dry_run: false,
            no_llm: true,
        };

        let output = execute_prompt(args, None)
            .await
            .expect("prompt should succeed");
        let plan_id = extract_json_plan_id(&output);
        assert!(
            plan_id.is_some(),
            "plan_id must be present in output: {output}"
        );
    }

    #[tokio::test]
    async fn test_sunny_analyze_path_still_works() {
        let temp_dir = mk_temp_dir("prompt_regression_analyze");
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
    async fn test_prompt_dry_run_returns_plan_without_execution() {
        let args = PromptArgs {
            input: "analyze this code".to_string(),
            format: "json".to_string(),
            dry_run: true,
            no_llm: true,
        };

        let output = execute_prompt(args, None)
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
}
