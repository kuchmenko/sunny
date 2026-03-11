use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use sunny_core::agent::{AgentMessage, Capability};
use sunny_core::orchestrator::{
    HeuristicLoopPlanner, IntakeAdvisor, Intent, IntentKind, PlanHints, PlanPolicy, PlanningIntake,
    PlanningIntakeInput, PlanningIntakeVerdict, RequestId, WorkspaceContext,
};
use sunny_mind::{LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, TokenUsage};

#[path = "../src/commands/intake_advisor.rs"]
mod intake_advisor;

use intake_advisor::LlmIntakeAdvisor;

struct MockProvider {
    response: Result<LlmResponse, LlmError>,
    call_count: Arc<AtomicUsize>,
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
        LlmError::UnsupportedAuthMode { mode } => {
            LlmError::UnsupportedAuthMode { mode: mode.clone() }
        }
    }
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
        self.call_count.fetch_add(1, Ordering::SeqCst);
        match &self.response {
            Ok(response) => Ok(response.clone()),
            Err(err) => Err(clone_llm_error(err)),
        }
    }
}

fn make_llm_response(content: &str) -> LlmResponse {
    LlmResponse {
        content: content.to_string(),
        usage: TokenUsage {
            input_tokens: 12,
            output_tokens: 8,
            total_tokens: 20,
        },
        finish_reason: "stop".to_string(),
        provider_id: ProviderId("mock".to_string()),
        model_id: ModelId("mock-model".to_string()),
        tool_calls: None,
        reasoning_content: None,
    }
}

fn make_intent(required_capability: Option<&str>, raw_input: &str) -> Intent {
    Intent {
        kind: IntentKind::Query,
        raw_input: raw_input.to_string(),
        required_capability: required_capability.map(|cap| Capability(cap.to_string())),
    }
}

fn make_task(id: &str, content: &str) -> AgentMessage {
    AgentMessage::Task {
        id: id.to_string(),
        content: content.to_string(),
        metadata: HashMap::new(),
    }
}

#[tokio::test]
async fn test_intake_advisory_pipeline_rebalances_advise_for_ambiguous_query() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider {
        response: Ok(make_llm_response(
            r#"{"suggested_capability":"advise","complexity_hint":"medium","context_tags":["routing"],"reasoning":"needs guidance"}"#,
        )),
        call_count: call_count.clone(),
    });
    let advisor: Arc<dyn IntakeAdvisor> = Arc::new(LlmIntakeAdvisor::new(provider));
    let intake = PlanningIntake::new(Some(advisor));

    let verdict = intake
        .evaluate(PlanningIntakeInput {
            intent: make_intent(Some("query"), "help me choose next steps"),
            task: make_task("task-1", "help me choose next steps"),
            request_id: RequestId::new(),
            llm_enabled: true,
            workspace_context: WorkspaceContext::default(),
        })
        .await;

    let hints = match verdict {
        PlanningIntakeVerdict::Proceed(hints) => hints,
        PlanningIntakeVerdict::Skip { reason } => panic!("expected Proceed, got Skip: {reason}"),
    };

    let planner = HeuristicLoopPlanner::new(PlanPolicy::default(), true);
    let plan = planner
        .build_plan(
            make_intent(Some("query"), "help me choose next steps"),
            make_task("task-1", "help me choose next steps"),
            RequestId::new(),
            Some(hints),
        )
        .expect("plan should build");

    assert_eq!(plan.steps.len(), 4);
    assert_eq!(
        plan.steps
            .iter()
            .find(|step| {
                step.metadata.get("_sunny.stage").map(String::as_str) == Some("plan_finalize")
            })
            .expect("finalize step")
            .required_capability,
        Some(Capability("query".to_string()))
    );
    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_intake_advisory_error_fallback() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider {
        response: Err(LlmError::Timeout { timeout_ms: 30_000 }),
        call_count: call_count.clone(),
    });
    let advisor: Arc<dyn IntakeAdvisor> = Arc::new(LlmIntakeAdvisor::new(provider));
    let intake = PlanningIntake::new(Some(advisor));

    let verdict = intake
        .evaluate(PlanningIntakeInput {
            intent: make_intent(Some("analyze"), "analyze this change"),
            task: make_task("task-2", "analyze this change"),
            request_id: RequestId::new(),
            llm_enabled: true,
            workspace_context: WorkspaceContext::default(),
        })
        .await;

    match verdict {
        PlanningIntakeVerdict::Proceed(hints) => {
            assert!(hints.suggested_capability.is_none());
            assert!(hints.complexity_hint.is_none());
            assert!(hints.context_tags.is_empty());
            assert!(hints.metadata_overrides.is_empty());
        }
        PlanningIntakeVerdict::Skip { reason } => panic!("expected Proceed, got Skip: {reason}"),
    }

    assert_eq!(call_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn test_intake_advisory_skipped_when_llm_disabled() {
    let call_count = Arc::new(AtomicUsize::new(0));
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider {
        response: Ok(make_llm_response(
            r#"{"suggested_capability":"advise","complexity_hint":"high","context_tags":["should_not_be_used"],"reasoning":"llm disabled"}"#,
        )),
        call_count: call_count.clone(),
    });
    let advisor: Arc<dyn IntakeAdvisor> = Arc::new(LlmIntakeAdvisor::new(provider));
    let intake = PlanningIntake::new(Some(advisor));

    let verdict = intake
        .evaluate(PlanningIntakeInput {
            intent: make_intent(Some("query"), "what should happen"),
            task: make_task("task-3", "what should happen"),
            request_id: RequestId::new(),
            llm_enabled: false,
            workspace_context: WorkspaceContext::default(),
        })
        .await;

    assert_eq!(call_count.load(Ordering::SeqCst), 0);
    assert!(matches!(
        verdict,
        PlanningIntakeVerdict::Proceed(PlanHints {
            suggested_capability: None,
            complexity_hint: None,
            context_tags,
            metadata_overrides,
        }) if context_tags.is_empty() && metadata_overrides.is_empty()
    ));
}

#[tokio::test]
async fn test_planner_uses_classifier_when_no_intake_hints() {
    let planner = HeuristicLoopPlanner::new(PlanPolicy::default(), false);
    let intent = make_intent(Some("query"), "inspect repository layout");
    let task = make_task("task-4", "inspect repository layout");

    let plan = planner
        .build_plan(intent, task, RequestId::new(), None)
        .expect("plan should build without hints");

    assert_eq!(plan.steps.len(), 2);
    assert_eq!(
        plan.steps
            .iter()
            .find(|step| {
                step.metadata.get("_sunny.stage").map(String::as_str) == Some("plan_finalize")
            })
            .expect("finalize step")
            .required_capability,
        Some(Capability("query".to_string()))
    );
}
