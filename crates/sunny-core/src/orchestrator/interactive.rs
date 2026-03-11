use std::collections::HashMap;

use tokio_util::sync::CancellationToken;

use crate::agent::{AgentMessage, AgentResponse};

use super::{
    HeuristicLoopPlanner, Intent, OrchestratorError, OrchestratorHandle, PlanExecutor, PlanOutcome,
    PlanningIntake, PlanningIntakeInput, PlanningIntakeVerdict, RequestId,
};

pub struct InteractiveOrchestrator<'a> {
    orchestrator: &'a OrchestratorHandle,
    planner: HeuristicLoopPlanner,
    intake: PlanningIntake,
}

impl<'a> InteractiveOrchestrator<'a> {
    pub fn new(
        orchestrator: &'a OrchestratorHandle,
        planner: HeuristicLoopPlanner,
        intake: PlanningIntake,
    ) -> Self {
        Self {
            orchestrator,
            planner,
            intake,
        }
    }

    pub async fn execute(
        &self,
        intent: Intent,
        task: AgentMessage,
        cancel: CancellationToken,
        request_id: RequestId,
    ) -> Result<AgentResponse, OrchestratorError> {
        let intake_verdict = self
            .intake
            .evaluate(PlanningIntakeInput {
                intent: intent.clone(),
                task: task.clone(),
                request_id,
                llm_enabled: self.planner.llm_enabled(),
            })
            .await;
        let (hints, intake_verdict_label, intake_skip_reason) = match intake_verdict {
            PlanningIntakeVerdict::Proceed(hints) => (Some(hints), "proceed", None),
            PlanningIntakeVerdict::Skip { reason } => {
                tracing::info!(event = "orchestrator.intake.skipped", reason = %reason);
                (None, "skip", Some(reason))
            }
        };

        let mut plan = self.planner.build_plan(intent, task, request_id, hints)?;
        let plan_id = plan.plan_id.clone();

        let executor = PlanExecutor::new(self.orchestrator);
        let result = executor.execute(&mut plan, cancel).await?;

        match result.overall_outcome {
            PlanOutcome::Success => {
                let content = plan
                    .steps
                    .iter()
                    .rev()
                    .find_map(|step| match step.outcome.as_ref() {
                        Some(super::StepOutcome::Success { content }) => Some(content.clone()),
                        _ => None,
                    })
                    .ok_or_else(|| OrchestratorError::PlanPolicyViolation {
                        reason: "plan completed without successful step output".to_string(),
                    })?;

                let mut metadata = HashMap::new();
                metadata.insert("plan_id".to_string(), plan_id);
                metadata.insert(
                    "steps_completed".to_string(),
                    result.steps_completed.to_string(),
                );
                metadata.insert("steps_failed".to_string(), result.steps_failed.to_string());
                metadata.insert(
                    "steps_skipped".to_string(),
                    result.steps_skipped.to_string(),
                );
                metadata.insert(
                    "_sunny.intake.verdict".to_string(),
                    intake_verdict_label.to_string(),
                );
                if let Some(skip_reason) = intake_skip_reason {
                    metadata.insert("_sunny.intake.skip_reason".to_string(), skip_reason);
                }

                Ok(AgentResponse::Success { content, metadata })
            }
            PlanOutcome::Cancelled => Err(OrchestratorError::AgentUnresponsive),
            PlanOutcome::Failed => Err(OrchestratorError::PlanPolicyViolation {
                reason: "interactive plan execution failed".to_string(),
            }),
        }
    }
}
