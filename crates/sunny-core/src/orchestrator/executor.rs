use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::agent::{AgentMessage, AgentResponse};

use super::{
    ExecutionPlan, OrchestratorError, OrchestratorHandle, PlanError, RequestId, StepOutcome,
    StepState, EVENT_PLAN_ERROR,
};

const EVENT_PLAN_START: &str = "orchestrator.plan.start";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanOutcome {
    Success,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanResult {
    pub plan_id: String,
    pub steps_completed: usize,
    pub steps_failed: usize,
    pub steps_skipped: usize,
    pub execution_depth: u32,
    pub overall_outcome: PlanOutcome,
}

pub struct PlanExecutor<'a> {
    pub orchestrator: &'a OrchestratorHandle,
}

impl<'a> PlanExecutor<'a> {
    pub fn new(orchestrator: &'a OrchestratorHandle) -> Self {
        Self { orchestrator }
    }

    pub async fn execute(
        &self,
        plan: &mut ExecutionPlan,
        cancel: CancellationToken,
    ) -> Result<PlanResult, OrchestratorError> {
        info!(
            event = EVENT_PLAN_START,
            plan_id = %plan.plan_id,
            step_count = plan.step_count()
        );

        let execution_depth = 1u32;
        if execution_depth > plan.policy.max_depth {
            info!(
                event = EVENT_PLAN_ERROR,
                plan_id = %plan.plan_id,
                error_kind = "depth_limit_exceeded",
                execution_depth,
                max_depth = plan.policy.max_depth
            );

            return Err(PlanError::DepthLimitExceeded {
                depth: execution_depth,
                max_depth: plan.policy.max_depth,
            }
            .into());
        }

        if plan.steps.is_empty() {
            info!(
                event = "orchestrator.plan.completed",
                plan_id = %plan.plan_id,
                outcome = "success"
            );

            return Ok(PlanResult {
                plan_id: plan.plan_id.clone(),
                steps_completed: 0,
                steps_failed: 0,
                steps_skipped: 0,
                execution_depth,
                overall_outcome: PlanOutcome::Success,
            });
        }

        plan.validate_dependencies()?;

        let request_id = parse_request_id(&plan.request_id);
        let mut cancelled = false;
        let mut steps_executed = 0usize;
        let mut completed_steps: Vec<String> = Vec::new();

        while steps_executed < plan.steps.len() {
            if cancel.is_cancelled() {
                skip_unprocessed_steps(plan)?;
                cancelled = true;
                break;
            }

            if steps_executed >= plan.policy.max_steps as usize {
                skip_unprocessed_steps(plan)?;
                warn!(
                    event = "orchestrator.plan.warning",
                    plan_id = %plan.plan_id,
                    warning_kind = "step_limit_exceeded",
                    steps_executed,
                    max_steps = plan.policy.max_steps
                );
                info!(
                    event = EVENT_PLAN_ERROR,
                    plan_id = %plan.plan_id,
                    error_kind = "step_limit_exceeded",
                    steps_executed,
                    max_steps = plan.policy.max_steps
                );

                return Err(PlanError::StepLimitExceeded {
                    executed: steps_executed,
                    max_steps: plan.policy.max_steps,
                }
                .into());
            }

            let Some(step_idx) = next_ready_step_idx(plan, &completed_steps) else {
                if plan
                    .steps
                    .iter()
                    .any(|step| matches!(step.state, StepState::Planned | StepState::Ready))
                {
                    skip_unprocessed_steps(plan)?;
                    return Err(OrchestratorError::PlanPolicyViolation {
                        reason: "no executable step found; dependency graph is unsatisfied"
                            .to_string(),
                    });
                }
                break;
            };

            let composed_action = {
                let step = &plan.steps[step_idx];
                let composed = compose_evidence_action(&step.action, plan, &step.depends_on);
                if !step.depends_on.is_empty() {
                    tracing::debug!(
                        event = "orchestrator.step.evidence_composed",
                        step_id = %step.step_id,
                        dependency_count = step.depends_on.len(),
                        total_evidence_bytes = composed.len().saturating_sub(step.action.len()),
                    );
                }
                composed
            };

            let step = &mut plan.steps[step_idx];
            step.transition(StepState::Ready)?;
            step.transition(StepState::Running)?;

            let mut terminal_outcome: Option<StepOutcome> = None;
            loop {
                if cancel.is_cancelled() {
                    step.transition(StepState::Cancelled)?;
                    step.set_outcome(StepOutcome::Cancelled);
                    cancelled = true;
                    break;
                }

                step.increment_attempt();

                let step_outcome = match step.required_capability.clone() {
                    Some(capability) => {
                        let dispatch = self.orchestrator.dispatch_by_capability_with_context(
                            capability,
                            AgentMessage::Task {
                                id: step.step_id.clone(),
                                content: composed_action.clone(),
                                metadata: step.metadata.clone(),
                            },
                            request_id,
                        );

                        match timeout(std::time::Duration::from_millis(step.timeout_ms), dispatch)
                            .await
                        {
                            Err(_) => StepOutcome::Timeout,
                            Ok(Ok(AgentResponse::Success { content, .. })) => {
                                StepOutcome::Success { content }
                            }
                            Ok(Ok(AgentResponse::Error { message, .. })) => {
                                StepOutcome::Error { message }
                            }
                            Ok(Err(err)) => StepOutcome::Error {
                                message: err.to_string(),
                            },
                        }
                    }
                    None => StepOutcome::Error {
                        message: "step missing required_capability".to_string(),
                    },
                };

                match step_outcome {
                    StepOutcome::Success { .. } => {
                        terminal_outcome = Some(step_outcome);
                        break;
                    }
                    StepOutcome::Error { .. } | StepOutcome::Timeout => {
                        if step.attempt <= plan.policy.max_retries {
                            continue;
                        }
                        terminal_outcome = Some(step_outcome);
                        break;
                    }
                    StepOutcome::Cancelled => {
                        terminal_outcome = Some(StepOutcome::Cancelled);
                        break;
                    }
                }
            }

            if let Some(step_outcome) = terminal_outcome {
                match step_outcome {
                    StepOutcome::Success { .. } => {
                        step.set_outcome(step_outcome);
                        step.transition(StepState::Completed)?;
                        completed_steps.push(step.step_id.clone());
                    }
                    StepOutcome::Error { .. } | StepOutcome::Timeout => {
                        step.set_outcome(step_outcome);
                        step.transition(StepState::Failed)?;
                        skip_unprocessed_steps(plan)?;
                    }
                    StepOutcome::Cancelled => {
                        step.set_outcome(StepOutcome::Cancelled);
                        if step.state != StepState::Cancelled {
                            step.transition(StepState::Cancelled)?;
                        }
                        cancelled = true;
                        skip_unprocessed_steps(plan)?;
                    }
                }
            } else if cancelled {
                skip_unprocessed_steps(plan)?;
            }

            steps_executed += 1;

            if cancelled || plan.steps[step_idx].state == StepState::Failed {
                break;
            }
        }

        let steps_completed = plan
            .steps
            .iter()
            .filter(|step| step.state == StepState::Completed)
            .count();
        let steps_failed = plan
            .steps
            .iter()
            .filter(|step| step.state == StepState::Failed)
            .count();
        let steps_skipped = plan
            .steps
            .iter()
            .filter(|step| step.state == StepState::Skipped || step.state == StepState::Cancelled)
            .count();

        let overall_outcome = if steps_failed > 0 {
            PlanOutcome::Failed
        } else if cancelled {
            PlanOutcome::Cancelled
        } else {
            PlanOutcome::Success
        };

        let outcome = match overall_outcome {
            PlanOutcome::Success => "success",
            PlanOutcome::Failed => "error",
            PlanOutcome::Cancelled => "cancelled",
        };

        info!(
            event = "orchestrator.plan.completed",
            plan_id = %plan.plan_id,
            outcome
        );

        Ok(PlanResult {
            plan_id: plan.plan_id.clone(),
            steps_completed,
            steps_failed,
            steps_skipped,
            execution_depth,
            overall_outcome,
        })
    }
}

fn parse_request_id(raw: &str) -> RequestId {
    match Uuid::parse_str(raw) {
        Ok(id) => RequestId(id),
        Err(_) => RequestId::new(),
    }
}

fn next_ready_step_idx(plan: &ExecutionPlan, completed_steps: &[String]) -> Option<usize> {
    plan.steps.iter().position(|step| {
        matches!(step.state, StepState::Planned)
            && step.is_ready_with(completed_steps)
            && !step
                .depends_on
                .iter()
                .any(|dependency| dependency == &step.step_id)
    })
}

fn skip_unprocessed_steps(plan: &mut ExecutionPlan) -> Result<(), OrchestratorError> {
    for step in &mut plan.steps {
        if matches!(step.state, StepState::Planned | StepState::Ready) {
            step.transition(StepState::Skipped)?;
            step.set_outcome(StepOutcome::Cancelled);
        }
    }

    Ok(())
}

fn compose_evidence_action(
    original_action: &str,
    plan: &ExecutionPlan,
    depends_on: &[String],
) -> String {
    if depends_on.is_empty() {
        return original_action.to_string();
    }

    const MAX_EVIDENCE_BYTES: usize = 32768;

    let evidence_parts: Vec<(String, String, String)> = plan
        .steps
        .iter()
        .filter(|s| depends_on.contains(&s.step_id))
        .filter_map(|s| {
            if let Some(StepOutcome::Success { content }) = &s.outcome {
                let capability = s
                    .required_capability
                    .as_ref()
                    .map(|capability| capability.0.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                Some((s.step_id.clone(), capability, content.clone()))
            } else {
                None
            }
        })
        .collect();

    if evidence_parts.is_empty() {
        return original_action.to_string();
    }

    let total_raw: usize = evidence_parts.iter().map(|(_, _, c)| c.len()).sum();

    let mut composed = String::new();
    for (step_id, capability, content) in &evidence_parts {
        let truncated_content = if total_raw > MAX_EVIDENCE_BYTES {
            let section_overhead = format!(
                "<gathered_evidence>\n## Evidence from step \"{step_id}\" ({capability})\n\n</gathered_evidence>\n"
            )
            .len();
            let max_section_len = MAX_EVIDENCE_BYTES / evidence_parts.len();
            let max_content_len = max_section_len.saturating_sub(section_overhead);
            if content.len() > max_content_len {
                let marker = format!("\n[TRUNCATED - {}B -> {}B]", content.len(), max_content_len);
                let marker_len = marker.len();
                let content_slice_len = max_content_len.saturating_sub(marker_len);
                format!("{}{}", &content[..content_slice_len], marker)
            } else {
                content.clone()
            }
        } else {
            content.clone()
        };
        composed.push_str(&format!(
            "<gathered_evidence>\n## Evidence from step \"{step_id}\" ({capability})\n{truncated_content}\n</gathered_evidence>\n"
        ));
    }

    composed.push_str(original_action);
    composed
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    use crate::agent::{
        Agent, AgentContext, AgentError, AgentHandle, AgentMessage, AgentResponse, Capability,
        EchoAgent,
    };
    use crate::orchestrator::{
        ExecutionPlan, IntentClassifier, OrchestratorError, PlanError, PlanPolicy, PlanStep,
        StepState,
    };

    use super::{compose_evidence_action, PlanExecutor, PlanOutcome};

    struct FlakyAgent {
        fail_attempts: u32,
        attempts: AtomicU32,
    }

    #[async_trait::async_trait]
    impl Agent for FlakyAgent {
        fn name(&self) -> &str {
            "flaky"
        }

        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability("analyze".to_string())]
        }

        async fn handle_message(
            &self,
            _msg: AgentMessage,
            _ctx: &AgentContext,
        ) -> Result<AgentResponse, AgentError> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst) + 1;
            if attempt <= self.fail_attempts {
                return Ok(AgentResponse::Error {
                    code: "retryable".to_string(),
                    message: "transient failure".to_string(),
                });
            }

            Ok(AgentResponse::Success {
                content: format!("attempt-{attempt}"),
                metadata: HashMap::new(),
            })
        }
    }

    struct CancellingAgent {
        cancel: CancellationToken,
    }

    #[async_trait::async_trait]
    impl Agent for CancellingAgent {
        fn name(&self) -> &str {
            "cancel-once"
        }

        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability("analyze".to_string())]
        }

        async fn handle_message(
            &self,
            _msg: AgentMessage,
            _ctx: &AgentContext,
        ) -> Result<AgentResponse, AgentError> {
            self.cancel.cancel();
            Ok(AgentResponse::Success {
                content: "done".to_string(),
                metadata: HashMap::new(),
            })
        }
    }

    struct RecordingAgent {
        seen: Arc<Mutex<Vec<String>>>,
        received_content: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl Agent for RecordingAgent {
        fn name(&self) -> &str {
            "recorder"
        }

        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability("analyze".to_string())]
        }

        async fn handle_message(
            &self,
            msg: AgentMessage,
            _ctx: &AgentContext,
        ) -> Result<AgentResponse, AgentError> {
            let AgentMessage::Task { id, content, .. } = msg;
            self.seen.lock().await.push(id);
            self.received_content.lock().await.push(content);
            Ok(AgentResponse::Success {
                content: "ok".to_string(),
                metadata: HashMap::new(),
            })
        }
    }

    struct ContentCapturingAgent {
        responses: HashMap<String, AgentResponse>,
        received_by_step: Arc<Mutex<HashMap<String, String>>>,
    }

    #[async_trait::async_trait]
    impl Agent for ContentCapturingAgent {
        fn name(&self) -> &str {
            "content-capturer"
        }

        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability("analyze".to_string())]
        }

        async fn handle_message(
            &self,
            msg: AgentMessage,
            _ctx: &AgentContext,
        ) -> Result<AgentResponse, AgentError> {
            let AgentMessage::Task { id, content, .. } = msg;
            self.received_by_step
                .lock()
                .await
                .insert(id.clone(), content);
            Ok(self
                .responses
                .get(&id)
                .cloned()
                .unwrap_or(AgentResponse::Success {
                    content: "ok".to_string(),
                    metadata: HashMap::new(),
                }))
        }
    }

    fn make_plan(plan_id: &str, request_id: &str) -> ExecutionPlan {
        let intent = IntentClassifier::default().classify("analyze this request");
        ExecutionPlan::new(
            plan_id.to_string(),
            request_id.to_string(),
            intent,
            PlanPolicy {
                max_depth: 2,
                max_steps: 16,
                max_retries: 2,
            },
        )
    }

    fn make_plan_with_policy(plan_id: &str, request_id: &str, policy: PlanPolicy) -> ExecutionPlan {
        let intent = IntentClassifier::default().classify("analyze this request");
        ExecutionPlan::new(plan_id.to_string(), request_id.to_string(), intent, policy)
    }

    #[tokio::test]
    async fn test_executor_sequential_execution() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let received_content = Arc::new(Mutex::new(Vec::new()));

        let recorder = AgentHandle::spawn(
            Arc::new(RecordingAgent {
                seen: seen.clone(),
                received_content: received_content.clone(),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-a".to_string(),
                recorder,
                vec![Capability("analyze".to_string())],
            )
            .expect("register recorder");

        let orchestrator = crate::orchestrator::OrchestratorHandle::spawn_with_routing(
            registry,
            Box::new(crate::orchestrator::NameRouting),
            cancel.child_token(),
        );

        let mut plan = make_plan("plan-seq", &uuid::Uuid::new_v4().to_string());
        plan.add_step(PlanStep::new(
            "step-1".to_string(),
            "first".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step1");
        plan.add_step(PlanStep::new(
            "step-2".to_string(),
            "second".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step2");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("plan executes");

        assert_eq!(result.overall_outcome, PlanOutcome::Success);
        assert_eq!(result.steps_completed, 2);
        assert_eq!(result.steps_failed, 0);
        assert_eq!(result.steps_skipped, 0);
        assert_eq!(plan.steps[0].state, StepState::Completed);
        assert_eq!(plan.steps[1].state, StepState::Completed);
        assert_eq!(plan.steps[0].attempt, 1);
        assert_eq!(plan.steps[1].attempt, 1);
        assert_eq!(
            *seen.lock().await,
            vec!["step-1".to_string(), "step-2".to_string()]
        );

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_executor_step_failure_with_retry() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();

        let flaky = AgentHandle::spawn(
            Arc::new(FlakyAgent {
                fail_attempts: 2,
                attempts: AtomicU32::new(0),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-z".to_string(),
                flaky,
                vec![Capability("analyze".to_string())],
            )
            .expect("register flaky");

        let orchestrator =
            crate::orchestrator::OrchestratorHandle::spawn(registry, cancel.child_token());

        let mut plan = make_plan("plan-retry", &uuid::Uuid::new_v4().to_string());
        plan.add_step(PlanStep::new(
            "step-1".to_string(),
            "retry-me".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("plan executes");

        assert_eq!(result.overall_outcome, PlanOutcome::Success);
        assert_eq!(plan.steps[0].attempt, 3);
        assert_eq!(plan.steps[0].state, StepState::Completed);

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_executor_cancellation_skips_remaining_steps() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();

        let cancelling = AgentHandle::spawn(
            Arc::new(CancellingAgent {
                cancel: cancel.clone(),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-cancel".to_string(),
                cancelling,
                vec![Capability("analyze".to_string())],
            )
            .expect("register cancelling agent");

        let fallback = AgentHandle::spawn(Arc::new(EchoAgent), cancel.child_token());
        registry
            .register(
                "analyzer-fallback".to_string(),
                fallback,
                vec![Capability("analyze".to_string())],
            )
            .expect("register fallback");

        let orchestrator = crate::orchestrator::OrchestratorHandle::spawn_with_routing(
            registry,
            Box::new(crate::orchestrator::NameRouting),
            cancel.child_token(),
        );

        let mut plan = make_plan("plan-cancel", &uuid::Uuid::new_v4().to_string());
        plan.add_step(PlanStep::new(
            "step-1".to_string(),
            "trigger-cancel".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step1");
        plan.add_step(PlanStep::new(
            "step-2".to_string(),
            "must-skip".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step2");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("plan executes");

        assert_eq!(result.overall_outcome, PlanOutcome::Cancelled);
        assert_eq!(result.steps_completed, 1);
        assert_eq!(result.steps_skipped, 1);
        assert_eq!(plan.steps[0].state, StepState::Completed);
        assert_eq!(plan.steps[1].state, StepState::Skipped);
    }

    #[tokio::test]
    async fn test_executor_empty_plan_success() {
        let cancel = CancellationToken::new();
        let orchestrator = crate::orchestrator::OrchestratorHandle::spawn(
            crate::orchestrator::AgentRegistry::new(),
            cancel.child_token(),
        );

        let mut plan = make_plan("plan-empty", &uuid::Uuid::new_v4().to_string());
        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("empty plan executes");

        assert_eq!(result.overall_outcome, PlanOutcome::Success);
        assert_eq!(result.steps_completed, 0);
        assert_eq!(result.steps_failed, 0);
        assert_eq!(result.steps_skipped, 0);

        cancel.cancel();
    }

    #[tokio::test]
    #[tracing_test::traced_test]
    async fn test_executor_enforces_max_steps_limit() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let received_content = Arc::new(Mutex::new(Vec::new()));

        let recorder = AgentHandle::spawn(
            Arc::new(RecordingAgent {
                seen: seen.clone(),
                received_content: received_content.clone(),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-a".to_string(),
                recorder,
                vec![Capability("analyze".to_string())],
            )
            .expect("register recorder");

        let orchestrator = crate::orchestrator::OrchestratorHandle::spawn_with_routing(
            registry,
            Box::new(crate::orchestrator::NameRouting),
            cancel.child_token(),
        );

        let mut plan = make_plan_with_policy(
            "plan-step-limit",
            &uuid::Uuid::new_v4().to_string(),
            PlanPolicy {
                max_depth: 2,
                max_steps: 20,
                max_retries: 0,
            },
        );

        for idx in 0..20 {
            plan.add_step(PlanStep::new(
                format!("step-{idx}"),
                format!("action-{idx}"),
                Some(Capability("analyze".to_string())),
                5_000,
            ))
            .expect("add step");
        }

        plan.policy.max_steps = 3;

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor.execute(&mut plan, cancel.child_token()).await;

        match result {
            Err(OrchestratorError::Plan {
                source:
                    PlanError::StepLimitExceeded {
                        executed,
                        max_steps,
                    },
            }) => {
                assert_eq!(executed, 3);
                assert_eq!(max_steps, 3);
            }
            other => panic!("expected step limit error, got {other:?}"),
        }

        let completed = plan
            .steps
            .iter()
            .filter(|step| step.state == StepState::Completed)
            .count();
        let skipped = plan
            .steps
            .iter()
            .filter(|step| step.state == StepState::Skipped)
            .count();

        assert_eq!(completed, 3);
        assert_eq!(skipped, 17);
        assert_eq!(seen.lock().await.len(), 3);
        assert!(logs_contain("orchestrator.plan.warning"));
        assert!(logs_contain("orchestrator.plan.error"));
        assert!(logs_contain("error_kind=\"step_limit_exceeded\""));
    }

    #[tokio::test]
    async fn test_executor_respects_max_retries_zero() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();

        let flaky = AgentHandle::spawn(
            Arc::new(FlakyAgent {
                fail_attempts: 10,
                attempts: AtomicU32::new(0),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-z".to_string(),
                flaky,
                vec![Capability("analyze".to_string())],
            )
            .expect("register flaky");

        let orchestrator =
            crate::orchestrator::OrchestratorHandle::spawn(registry, cancel.child_token());

        let mut plan = make_plan_with_policy(
            "plan-no-retry",
            &uuid::Uuid::new_v4().to_string(),
            PlanPolicy {
                max_depth: 2,
                max_steps: 16,
                max_retries: 0,
            },
        );
        plan.add_step(PlanStep::new(
            "step-1".to_string(),
            "always-fail".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("execution should complete with failed step");

        assert_eq!(result.overall_outcome, PlanOutcome::Failed);
        assert_eq!(plan.steps[0].attempt, 1);
        assert_eq!(plan.steps[0].state, StepState::Failed);

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_executor_reports_execution_depth() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();
        let fallback = AgentHandle::spawn(Arc::new(EchoAgent), cancel.child_token());
        registry
            .register(
                "analyzer-a".to_string(),
                fallback,
                vec![Capability("analyze".to_string())],
            )
            .expect("register fallback");

        let orchestrator =
            crate::orchestrator::OrchestratorHandle::spawn(registry, cancel.child_token());

        let mut plan = make_plan("plan-depth-field", &uuid::Uuid::new_v4().to_string());
        plan.add_step(PlanStep::new(
            "step-1".to_string(),
            "ok".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("plan executes");

        assert_eq!(result.execution_depth, 1);

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_executor_respects_dependency_graph_readiness() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let received_content = Arc::new(Mutex::new(Vec::new()));

        let recorder = AgentHandle::spawn(
            Arc::new(RecordingAgent {
                seen: seen.clone(),
                received_content: received_content.clone(),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-graph".to_string(),
                recorder,
                vec![Capability("analyze".to_string())],
            )
            .expect("register recorder");

        let orchestrator = crate::orchestrator::OrchestratorHandle::spawn_with_routing(
            registry,
            Box::new(crate::orchestrator::NameRouting),
            cancel.child_token(),
        );

        let mut plan = make_plan("plan-deps", &uuid::Uuid::new_v4().to_string());
        plan.add_step(PlanStep::new_with_metadata(
            "step-2".to_string(),
            "second".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
            HashMap::new(),
            vec!["step-1".to_string()],
        ))
        .expect("add dependent step");
        plan.add_step(PlanStep::new(
            "step-1".to_string(),
            "first".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add root step");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("plan executes");

        assert_eq!(result.overall_outcome, PlanOutcome::Success);
        assert_eq!(
            *seen.lock().await,
            vec!["step-1".to_string(), "step-2".to_string()]
        );
    }

    #[tokio::test]
    async fn test_evidence_composition_single_dependency_success() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();
        let received_by_step = Arc::new(Mutex::new(HashMap::new()));

        let mut responses = HashMap::new();
        responses.insert(
            "step-A".to_string(),
            AgentResponse::Success {
                content: "found: foo.rs".to_string(),
                metadata: HashMap::new(),
            },
        );
        responses.insert(
            "step-B".to_string(),
            AgentResponse::Success {
                content: "processed".to_string(),
                metadata: HashMap::new(),
            },
        );

        let recorder = AgentHandle::spawn(
            Arc::new(ContentCapturingAgent {
                responses,
                received_by_step: received_by_step.clone(),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-evidence".to_string(),
                recorder,
                vec![Capability("analyze".to_string())],
            )
            .expect("register capturing agent");

        let orchestrator = crate::orchestrator::OrchestratorHandle::spawn_with_routing(
            registry,
            Box::new(crate::orchestrator::NameRouting),
            cancel.child_token(),
        );

        let mut plan = make_plan("plan-evidence-success", &uuid::Uuid::new_v4().to_string());
        plan.add_step(PlanStep::new(
            "step-A".to_string(),
            "collect evidence".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step A");
        plan.add_step(PlanStep::new_with_metadata(
            "step-B".to_string(),
            "use evidence".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
            HashMap::new(),
            vec!["step-A".to_string()],
        ))
        .expect("add step B");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("plan executes");

        assert_eq!(result.overall_outcome, PlanOutcome::Success);
        let received = received_by_step.lock().await;
        let step_b_content = received.get("step-B").expect("step-B content captured");
        assert!(step_b_content.contains("<gathered_evidence>"));
        assert!(step_b_content.contains("found: foo.rs"));

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_evidence_composition_no_dependencies() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();
        let received_by_step = Arc::new(Mutex::new(HashMap::new()));

        let mut responses = HashMap::new();
        responses.insert(
            "step-A".to_string(),
            AgentResponse::Success {
                content: "done".to_string(),
                metadata: HashMap::new(),
            },
        );

        let recorder = AgentHandle::spawn(
            Arc::new(ContentCapturingAgent {
                responses,
                received_by_step: received_by_step.clone(),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-nodeps".to_string(),
                recorder,
                vec![Capability("analyze".to_string())],
            )
            .expect("register capturing agent");

        let orchestrator = crate::orchestrator::OrchestratorHandle::spawn_with_routing(
            registry,
            Box::new(crate::orchestrator::NameRouting),
            cancel.child_token(),
        );

        let mut plan = make_plan("plan-evidence-nodeps", &uuid::Uuid::new_v4().to_string());
        let original_action = "run standalone".to_string();
        plan.add_step(PlanStep::new(
            "step-A".to_string(),
            original_action.clone(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step A");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("plan executes");

        assert_eq!(result.overall_outcome, PlanOutcome::Success);
        let received = received_by_step.lock().await;
        assert_eq!(
            received.get("step-A").expect("step-A content captured"),
            &original_action
        );

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_evidence_composition_failed_dependency_skipped() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();
        let received_by_step = Arc::new(Mutex::new(HashMap::new()));

        let mut responses = HashMap::new();
        responses.insert(
            "step-A".to_string(),
            AgentResponse::Error {
                code: "failed".to_string(),
                message: "dependency failed".to_string(),
            },
        );
        responses.insert(
            "step-B".to_string(),
            AgentResponse::Success {
                content: "done".to_string(),
                metadata: HashMap::new(),
            },
        );

        let recorder = AgentHandle::spawn(
            Arc::new(ContentCapturingAgent {
                responses,
                received_by_step: received_by_step.clone(),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-failed-dep".to_string(),
                recorder,
                vec![Capability("analyze".to_string())],
            )
            .expect("register capturing agent");

        let orchestrator = crate::orchestrator::OrchestratorHandle::spawn_with_routing(
            registry,
            Box::new(crate::orchestrator::NameRouting),
            cancel.child_token(),
        );

        let mut plan = make_plan("plan-evidence-failed", &uuid::Uuid::new_v4().to_string());
        plan.policy.max_retries = 0;
        plan.add_step(PlanStep::new(
            "step-A".to_string(),
            "collect evidence".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step A");
        let original_step_b_action = "use evidence".to_string();
        plan.add_step(PlanStep::new_with_metadata(
            "step-B".to_string(),
            original_step_b_action.clone(),
            Some(Capability("analyze".to_string())),
            5_000,
            HashMap::new(),
            vec!["step-A".to_string()],
        ))
        .expect("add step B");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("plan executes with failure");

        assert_eq!(result.overall_outcome, PlanOutcome::Failed);
        assert_eq!(plan.steps[0].state, StepState::Failed);
        assert_eq!(plan.steps[1].state, StepState::Skipped);

        let composed =
            compose_evidence_action(&original_step_b_action, &plan, &plan.steps[1].depends_on);
        assert_eq!(composed, original_step_b_action);
        assert!(!composed.contains("<gathered_evidence>"));

        let received = received_by_step.lock().await;
        assert!(received.get("step-B").is_none());

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_evidence_composition_truncation() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();
        let received_by_step = Arc::new(Mutex::new(HashMap::new()));

        let large_content = "x".repeat(40 * 1024);
        let mut responses = HashMap::new();
        responses.insert(
            "step-A".to_string(),
            AgentResponse::Success {
                content: large_content,
                metadata: HashMap::new(),
            },
        );
        responses.insert(
            "step-B".to_string(),
            AgentResponse::Success {
                content: "done".to_string(),
                metadata: HashMap::new(),
            },
        );

        let recorder = AgentHandle::spawn(
            Arc::new(ContentCapturingAgent {
                responses,
                received_by_step: received_by_step.clone(),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-truncation".to_string(),
                recorder,
                vec![Capability("analyze".to_string())],
            )
            .expect("register capturing agent");

        let orchestrator = crate::orchestrator::OrchestratorHandle::spawn_with_routing(
            registry,
            Box::new(crate::orchestrator::NameRouting),
            cancel.child_token(),
        );

        let mut plan = make_plan(
            "plan-evidence-truncation",
            &uuid::Uuid::new_v4().to_string(),
        );
        plan.add_step(PlanStep::new(
            "step-A".to_string(),
            "collect huge evidence".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step A");
        let original_action = "use huge evidence".to_string();
        plan.add_step(PlanStep::new_with_metadata(
            "step-B".to_string(),
            original_action.clone(),
            Some(Capability("analyze".to_string())),
            5_000,
            HashMap::new(),
            vec!["step-A".to_string()],
        ))
        .expect("add step B");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("plan executes");

        assert_eq!(result.overall_outcome, PlanOutcome::Success);
        let received = received_by_step.lock().await;
        let step_b_content = received.get("step-B").expect("step-B content captured");
        assert!(step_b_content.len() <= 32768 + original_action.len());
        assert!(step_b_content.contains("[TRUNCATED"));

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_multistep_plan_evidence_reaches_final_step() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();
        let received_by_step = Arc::new(Mutex::new(HashMap::new()));

        let mut responses = HashMap::new();
        responses.insert(
            "step-1".to_string(),
            AgentResponse::Success {
                content: "File A contains function foo()".to_string(),
                metadata: HashMap::new(),
            },
        );
        responses.insert(
            "step-2".to_string(),
            AgentResponse::Success {
                content: "Found 5 usages of foo() in module bar".to_string(),
                metadata: HashMap::new(),
            },
        );
        responses.insert(
            "step-3".to_string(),
            AgentResponse::Success {
                content: "analysis complete".to_string(),
                metadata: HashMap::new(),
            },
        );

        let recorder = AgentHandle::spawn(
            Arc::new(ContentCapturingAgent {
                responses,
                received_by_step: received_by_step.clone(),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-e2e".to_string(),
                recorder,
                vec![Capability("analyze".to_string())],
            )
            .expect("register agent");

        let orchestrator = crate::orchestrator::OrchestratorHandle::spawn_with_routing(
            registry,
            Box::new(crate::orchestrator::NameRouting),
            cancel.child_token(),
        );

        let mut plan = make_plan("plan-e2e-evidence", &uuid::Uuid::new_v4().to_string());
        plan.add_step(PlanStep::new(
            "step-1".to_string(),
            "collect sources".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step 1");
        plan.add_step(PlanStep::new(
            "step-2".to_string(),
            "explore usages".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step 2");
        let original_action_3 = "synthesize findings".to_string();
        plan.add_step(PlanStep::new_with_metadata(
            "step-3".to_string(),
            original_action_3.clone(),
            Some(Capability("analyze".to_string())),
            5_000,
            HashMap::new(),
            vec!["step-1".to_string(), "step-2".to_string()],
        ))
        .expect("add step 3");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("plan executes");

        assert_eq!(result.overall_outcome, PlanOutcome::Success);
        let received = received_by_step.lock().await;
        let step3_content = received.get("step-3").expect("step-3 content captured");
        assert!(
            step3_content.contains("<gathered_evidence>"),
            "evidence block missing"
        );
        assert!(
            step3_content.contains("File A contains function foo()"),
            "step-1 evidence missing"
        );
        assert!(
            step3_content.contains("Found 5 usages of foo() in module bar"),
            "step-2 evidence missing"
        );
        assert!(
            step3_content.contains(&original_action_3),
            "original action missing"
        );

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_multistep_plan_partial_failure_evidence() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();
        let received_by_step = Arc::new(Mutex::new(HashMap::new()));

        let mut responses = HashMap::new();
        responses.insert(
            "step-1".to_string(),
            AgentResponse::Success {
                content: "Found code patterns".to_string(),
                metadata: HashMap::new(),
            },
        );
        responses.insert(
            "step-2".to_string(),
            AgentResponse::Error {
                code: "fail".to_string(),
                message: "step 2 failed".to_string(),
            },
        );
        responses.insert(
            "step-3".to_string(),
            AgentResponse::Success {
                content: "done".to_string(),
                metadata: HashMap::new(),
            },
        );

        let recorder = AgentHandle::spawn(
            Arc::new(ContentCapturingAgent {
                responses,
                received_by_step: received_by_step.clone(),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-partial".to_string(),
                recorder,
                vec![Capability("analyze".to_string())],
            )
            .expect("register agent");

        let orchestrator = crate::orchestrator::OrchestratorHandle::spawn_with_routing(
            registry,
            Box::new(crate::orchestrator::NameRouting),
            cancel.child_token(),
        );

        let mut plan = make_plan("plan-partial-failure", &uuid::Uuid::new_v4().to_string());
        plan.policy.max_retries = 0;
        plan.add_step(PlanStep::new(
            "step-1".to_string(),
            "collect".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step 1");
        plan.add_step(PlanStep::new(
            "step-2".to_string(),
            "explore".to_string(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add step 2");
        let original_action_3 = "synthesize".to_string();
        plan.add_step(PlanStep::new_with_metadata(
            "step-3".to_string(),
            original_action_3.clone(),
            Some(Capability("analyze".to_string())),
            5_000,
            HashMap::new(),
            vec!["step-1".to_string(), "step-2".to_string()],
        ))
        .expect("add step 3");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("plan executes with failure");

        // Step 2 fails → skip_unprocessed_steps runs → step 3 is Skipped before dispatch.
        assert_eq!(result.overall_outcome, PlanOutcome::Failed);
        assert_eq!(
            plan.steps[0].state,
            StepState::Completed,
            "step 1 should complete"
        );
        assert_eq!(plan.steps[1].state, StepState::Failed, "step 2 should fail");
        assert_eq!(
            plan.steps[2].state,
            StepState::Skipped,
            "step 3 should be skipped"
        );

        // Verify compose_evidence_action for step 3 only includes step 1 evidence.
        let composed =
            compose_evidence_action(&original_action_3, &plan, &plan.steps[2].depends_on);
        assert!(
            composed.contains("Found code patterns"),
            "step 1 evidence should be included"
        );
        assert!(
            !composed.contains("step 2 failed"),
            "failed step 2 evidence must not be included"
        );
        assert!(
            composed.contains(&original_action_3),
            "original action must be present"
        );

        cancel.cancel();
    }

    #[tokio::test]
    async fn test_single_step_plan_no_evidence_composition() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();
        let received_by_step = Arc::new(Mutex::new(HashMap::new()));

        let mut responses = HashMap::new();
        let original_action = "standalone analysis".to_string();
        responses.insert(
            "step-solo".to_string(),
            AgentResponse::Success {
                content: "done".to_string(),
                metadata: HashMap::new(),
            },
        );

        let recorder = AgentHandle::spawn(
            Arc::new(ContentCapturingAgent {
                responses,
                received_by_step: received_by_step.clone(),
            }),
            cancel.child_token(),
        );
        registry
            .register(
                "analyzer-solo".to_string(),
                recorder,
                vec![Capability("analyze".to_string())],
            )
            .expect("register agent");

        let orchestrator = crate::orchestrator::OrchestratorHandle::spawn_with_routing(
            registry,
            Box::new(crate::orchestrator::NameRouting),
            cancel.child_token(),
        );

        let mut plan = make_plan("plan-solo", &uuid::Uuid::new_v4().to_string());
        plan.add_step(PlanStep::new(
            "step-solo".to_string(),
            original_action.clone(),
            Some(Capability("analyze".to_string())),
            5_000,
        ))
        .expect("add solo step");

        let executor = PlanExecutor::new(&orchestrator);
        let result = executor
            .execute(&mut plan, cancel.child_token())
            .await
            .expect("single step plan executes");

        assert_eq!(result.overall_outcome, PlanOutcome::Success);
        let received = received_by_step.lock().await;
        let content = received
            .get("step-solo")
            .expect("step-solo content captured");
        assert_eq!(
            content, &original_action,
            "single step must receive original action unchanged"
        );
        assert!(
            !content.contains("<gathered_evidence>"),
            "no evidence blocks for steps without dependencies"
        );

        cancel.cancel();
    }
}
