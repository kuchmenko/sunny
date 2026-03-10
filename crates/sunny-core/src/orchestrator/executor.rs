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

        let request_id = parse_request_id(&plan.request_id);
        let mut cancelled = false;

        for idx in 0..plan.steps.len() {
            if cancel.is_cancelled() {
                skip_remaining_steps(plan, idx)?;
                cancelled = true;
                break;
            }

            if idx >= plan.policy.max_steps as usize {
                skip_remaining_steps(plan, idx)?;
                warn!(
                    event = "orchestrator.plan.warning",
                    plan_id = %plan.plan_id,
                    warning_kind = "step_limit_exceeded",
                    steps_executed = idx,
                    max_steps = plan.policy.max_steps
                );
                info!(
                    event = EVENT_PLAN_ERROR,
                    plan_id = %plan.plan_id,
                    error_kind = "step_limit_exceeded",
                    steps_executed = idx,
                    max_steps = plan.policy.max_steps
                );

                return Err(PlanError::StepLimitExceeded {
                    executed: idx,
                    max_steps: plan.policy.max_steps,
                }
                .into());
            }

            let step = &mut plan.steps[idx];
            step.transition(StepState::Ready)?;
            step.transition(StepState::Running)?;

            loop {
                if cancel.is_cancelled() {
                    step.transition(StepState::Cancelled)?;
                    step.set_outcome(StepOutcome::Cancelled);
                    skip_remaining_steps(plan, idx + 1)?;
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
                                content: step.action.clone(),
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
                        step.set_outcome(step_outcome);
                        step.transition(StepState::Completed)?;
                        break;
                    }
                    StepOutcome::Error { .. } | StepOutcome::Timeout => {
                        if step.attempt <= plan.policy.max_retries {
                            continue;
                        }

                        step.set_outcome(step_outcome);
                        step.transition(StepState::Failed)?;
                        break;
                    }
                    StepOutcome::Cancelled => {
                        step.set_outcome(StepOutcome::Cancelled);
                        step.transition(StepState::Cancelled)?;
                        skip_remaining_steps(plan, idx + 1)?;
                        cancelled = true;
                        break;
                    }
                }
            }

            if cancelled {
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

fn skip_remaining_steps(
    plan: &mut ExecutionPlan,
    from_idx: usize,
) -> Result<(), OrchestratorError> {
    for step in plan.steps.iter_mut().skip(from_idx) {
        if matches!(step.state, StepState::Planned | StepState::Ready) {
            step.transition(StepState::Skipped)?;
            step.set_outcome(StepOutcome::Cancelled);
        }
    }

    Ok(())
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

    use super::{PlanExecutor, PlanOutcome};

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
            let AgentMessage::Task { id, .. } = msg;
            self.seen.lock().await.push(id);
            Ok(AgentResponse::Success {
                content: "ok".to_string(),
                metadata: HashMap::new(),
            })
        }
    }

    fn make_plan(plan_id: &str, request_id: &str) -> ExecutionPlan {
        let intent = IntentClassifier::new().classify("analyze this request");
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
        let intent = IntentClassifier::new().classify("analyze this request");
        ExecutionPlan::new(plan_id.to_string(), request_id.to_string(), intent, policy)
    }

    #[tokio::test]
    async fn test_executor_sequential_execution() {
        let cancel = CancellationToken::new();
        let mut registry = crate::orchestrator::AgentRegistry::new();
        let seen = Arc::new(Mutex::new(Vec::new()));

        let recorder = AgentHandle::spawn(
            Arc::new(RecordingAgent { seen: seen.clone() }),
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

        let recorder = AgentHandle::spawn(
            Arc::new(RecordingAgent { seen: seen.clone() }),
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
}
