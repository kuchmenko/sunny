use std::sync::Arc;

use chrono::{Duration, Utc};

use crate::{
    error::PlanError,
    events::{PlanEvent, ReplanTrigger},
    handoff::{HandoffBuilder, HandoffContext},
    store::PlanStore,
};

#[derive(Debug, Clone)]
pub struct ReplanRequest {
    pub plan_id: String,
    pub reason: String,
    pub tasks_to_cancel: Vec<String>,
    pub new_task_specs: Vec<NewTaskSpec>,
}

#[derive(Debug, Clone)]
pub struct NewTaskSpec {
    pub task_id: String,
    pub title: String,
    pub dep_ids: Vec<String>,
}

pub struct ReplanResult {
    pub plan_id: String,
    pub cancelled_tasks: Vec<String>,
    pub added_tasks: Vec<String>,
    pub context: HandoffContext,
}

pub struct ReplanCoordinator {
    store: Arc<PlanStore>,
}

impl ReplanCoordinator {
    pub fn new(store: Arc<PlanStore>) -> Self {
        Self { store }
    }

    pub fn prepare_replan(&self, plan_id: &str, reason: &str) -> Result<HandoffContext, PlanError> {
        let builder = HandoffBuilder::new(&self.store);
        let context = builder.build_replan_context(plan_id, reason)?;
        Ok(context)
    }

    pub fn apply_replan(&self, request: ReplanRequest) -> Result<ReplanResult, PlanError> {
        for task_id in &request.tasks_to_cancel {
            self.store
                .remove_task_from_plan(&request.plan_id, task_id)?;
        }

        for spec in &request.new_task_specs {
            self.store.add_task_to_plan(
                &request.plan_id,
                &spec.task_id,
                &spec.title,
                &spec.dep_ids,
            )?;
        }

        self.store.append_event(
            &request.plan_id,
            &PlanEvent::ReplanTriggered {
                reason: request.reason.clone(),
                trigger: ReplanTrigger::AgentRequest,
            },
            "replan_coordinator",
        )?;

        let builder = HandoffBuilder::new(&self.store);
        let context = builder.build_replan_context(&request.plan_id, &request.reason)?;
        let added_tasks = request
            .new_task_specs
            .iter()
            .map(|spec| spec.task_id.clone())
            .collect();

        Ok(ReplanResult {
            plan_id: request.plan_id,
            cancelled_tasks: request.tasks_to_cancel,
            added_tasks,
            context,
        })
    }

    pub fn has_pending_replan(&self, plan_id: &str) -> Result<bool, PlanError> {
        let events = self.store.get_events(plan_id)?;
        let cutoff = Utc::now() - Duration::hours(24);

        Ok(events
            .iter()
            .filter(|event| event.created_at >= cutoff)
            .any(|event| event.event_type == "replan_triggered"))
    }
}
