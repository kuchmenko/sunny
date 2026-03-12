use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use sunny_core::agent::{
    Agent, AgentContext, AgentError, AgentHandle, AgentMessage, AgentResponse, Capability,
};
use sunny_core::orchestrator::{
    AgentRegistry, ExecutionPlan, IntentClassifier, IntentKind, OrchestratorHandle, PlanExecutor,
    PlanId, PlanOutcome, PlanStep, RequestId, StepOutcome, StepState,
};

#[derive(Clone)]
struct TracingRecordingAgent {
    name: String,
    capability: Capability,
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Agent for TracingRecordingAgent {
    fn name(&self) -> &str {
        &self.name
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![self.capability.clone()]
    }

    async fn handle_message(
        &self,
        msg: AgentMessage,
        _ctx: &AgentContext,
    ) -> Result<AgentResponse, AgentError> {
        let AgentMessage::Task {
            id,
            content,
            mut metadata,
        } = msg;

        let request_id = metadata
            .get("_sunny.request_id")
            .cloned()
            .unwrap_or_else(|| "missing".to_string());

        tracing::info!(
            event = "agent.message.start",
            agent_name = %self.name,
            task_id = %id,
            request_id = %request_id
        );

        self.seen.lock().await.push(id.clone());

        tracing::info!(
            event = "agent.message.end",
            agent_name = %self.name,
            task_id = %id,
            request_id = %request_id
        );

        metadata.insert("handled_by".to_string(), self.name.clone());

        Ok(AgentResponse::Success { content, metadata })
    }
}

#[tokio::test]
#[tracing_test::traced_test]
async fn test_planner_integration_plan_route_dispatch_flow() {
    let cancellation = CancellationToken::new();

    let analyzer_seen = Arc::new(Mutex::new(Vec::new()));
    let researcher_seen = Arc::new(Mutex::new(Vec::new()));

    let analyzer = AgentHandle::spawn(
        Arc::new(TracingRecordingAgent {
            name: "analyzer".to_string(),
            capability: Capability("analyze".to_string()),
            seen: analyzer_seen.clone(),
        }),
        cancellation.child_token(),
    );
    let researcher = AgentHandle::spawn(
        Arc::new(TracingRecordingAgent {
            name: "researcher".to_string(),
            capability: Capability("query".to_string()),
            seen: researcher_seen.clone(),
        }),
        cancellation.child_token(),
    );

    let mut registry = AgentRegistry::new();
    registry
        .register(
            "analyzer".to_string(),
            analyzer,
            vec![Capability("analyze".to_string())],
        )
        .expect("analyzer should register");
    registry
        .register(
            "researcher".to_string(),
            researcher,
            vec![Capability("query".to_string())],
        )
        .expect("researcher should register");

    let orchestrator = OrchestratorHandle::spawn(registry, cancellation.child_token());

    let classifier = IntentClassifier::default();
    let intent = classifier.classify("analyze this code");

    assert_eq!(intent.kind, IntentKind::Analyze);
    assert_eq!(
        intent.required_capability,
        Some(Capability("analyze".to_string()))
    );

    let request_id = RequestId::new();
    let request_id_text = request_id.to_string();
    let plan_id = PlanId(request_id.0).to_string();

    let mut plan = ExecutionPlan::new(
        plan_id.clone(),
        request_id_text.clone(),
        intent,
        sunny_core::orchestrator::PlanPolicy::default(),
    );
    plan.add_step(PlanStep::new(
        "step-analyze".to_string(),
        "analyze this code".to_string(),
        Some(Capability("analyze".to_string())),
        5_000,
    ))
    .expect("step should be added");

    let executor = PlanExecutor::new(&orchestrator);
    let result = executor
        .execute(&mut plan, cancellation.child_token())
        .await
        .expect("plan execution should succeed");

    assert_eq!(result.overall_outcome, PlanOutcome::Success);
    assert_eq!(result.steps_completed, 1);
    assert_eq!(result.steps_failed, 0);
    assert_eq!(result.steps_skipped, 0);
    assert_eq!(result.plan_id, plan_id);

    assert_eq!(plan.steps[0].state, StepState::Completed);
    match plan.steps[0].outcome.as_ref() {
        Some(StepOutcome::Success { content }) => {
            assert_eq!(content, "analyze this code");
        }
        other => panic!("expected successful step outcome, got: {other:?}"),
    }

    assert_eq!(
        *analyzer_seen.lock().await,
        vec!["step-analyze".to_string()]
    );
    assert!(researcher_seen.lock().await.is_empty());

    logs_assert(|lines: &[&str]| {
        fn find_idx(lines: &[&str], needle: &str) -> Result<usize, String> {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .ok_or_else(|| format!("missing log line containing `{needle}`"))
        }

        let plan_start = find_idx(lines, "event=\"orchestrator.plan.start\"")?;
        let route_selected = find_idx(lines, "selected_agent=\"analyzer\"")?;
        let dispatch_start = find_idx(lines, "event=\"orchestrator.dispatch.start\"")?;
        let agent_start = find_idx(lines, "event=\"agent.message.start\"")?;
        let agent_end = find_idx(lines, "event=\"agent.message.end\"")?;
        let dispatch_success = find_idx(lines, "event=\"orchestrator.dispatch.success\"")?;
        let plan_completed = find_idx(lines, "event=\"orchestrator.plan.completed\"")?;

        if !(plan_start < route_selected
            && route_selected < dispatch_start
            && dispatch_start < agent_start
            && agent_start < agent_end
            && agent_end < dispatch_success
            && dispatch_success < plan_completed)
        {
            return Err(format!(
                "unexpected event order: plan_start={plan_start}, route_selected={route_selected}, dispatch_start={dispatch_start}, agent_start={agent_start}, agent_end={agent_end}, dispatch_success={dispatch_success}, plan_completed={plan_completed}"
            ));
        }

        let request_field = format!("request_id={request_id_text}");
        let plan_field = format!("plan_id={plan_id}");

        let request_scoped = [
            route_selected,
            dispatch_start,
            agent_start,
            agent_end,
            dispatch_success,
        ];
        for idx in request_scoped {
            let line = lines[idx];
            if !line.contains(&request_field) {
                return Err(format!(
                    "request-scoped line missing request_id `{request_id_text}`: {line}"
                ));
            }
        }

        let plan_scoped = [plan_start, plan_completed];
        for idx in plan_scoped {
            let line = lines[idx];
            if !line.contains(&plan_field) {
                return Err(format!(
                    "plan-scoped line missing plan_id `{plan_id}`: {line}"
                ));
            }
        }

        Ok(())
    });

    cancellation.cancel();
}
