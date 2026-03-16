use std::sync::Arc;

use crate::error::PlanError;
use crate::events::PlanEvent;
use crate::handoff::{HandoffBuilder, HandoffContext};
use crate::model::{Plan, PlanMode, PlanStatus};
use crate::store::PlanStore;

pub struct PlanOrchestrator {
    store: Arc<PlanStore>,
}

impl PlanOrchestrator {
    pub fn new(store: Arc<PlanStore>) -> Self {
        Self { store }
    }

    pub fn create_plan(
        &self,
        workspace_id: &str,
        name: &str,
        description: Option<&str>,
        mode: PlanMode,
        root_session_id: Option<&str>,
    ) -> Result<Plan, PlanError> {
        self.store
            .create_plan(workspace_id, name, description, mode, root_session_id)
    }

    pub fn finalize_plan(&self, plan_id: &str) -> Result<Plan, PlanError> {
        let plan = self.require_plan(plan_id)?;

        // Allow finalize on Draft or Ready (idempotent)
        if !matches!(plan.status, PlanStatus::Draft | PlanStatus::Ready) {
            return Err(PlanError::InvalidStatus {
                status: format!("expected Draft or Ready, got {}", plan.status),
            });
        }

        // If already Ready, re-validate and return without new event
        if plan.status == PlanStatus::Ready {
            let state = self.store.get_plan_state(plan_id)?;
            // Re-validate DAG
            crate::tools::validate_dag(&state)?;
            return self.require_plan(plan_id);
        }

        // Draft -> Ready transition
        self.store.update_plan_status(plan_id, PlanStatus::Ready)?;
        self.store.append_event(
            plan_id,
            &PlanEvent::PlanFinalized {
                validation_result: "ok".to_string(),
            },
            "orchestrator",
        )?;

        self.require_plan(plan_id)
    }

    pub fn activate_plan(&self, plan_id: &str) -> Result<Plan, PlanError> {
        let plan = self.require_plan(plan_id)?;
        ensure_status(&plan, PlanStatus::Ready)?;

        self.store.update_plan_status(plan_id, PlanStatus::Active)?;
        self.store
            .append_event(plan_id, &PlanEvent::PlanActivated, "orchestrator")?;

        self.require_plan(plan_id)
    }

    pub fn complete_plan(&self, plan_id: &str, summary: &str) -> Result<Plan, PlanError> {
        let plan = self.require_plan(plan_id)?;
        ensure_status(&plan, PlanStatus::Active)?;

        self.store
            .update_plan_status(plan_id, PlanStatus::Completed)?;
        self.store.append_event(
            plan_id,
            &PlanEvent::PlanCompleted {
                summary: summary.to_string(),
            },
            "orchestrator",
        )?;

        self.require_plan(plan_id)
    }

    pub fn fail_plan(&self, plan_id: &str, reason: &str) -> Result<Plan, PlanError> {
        let plan = self.require_plan(plan_id)?;
        ensure_status(&plan, PlanStatus::Active)?;

        self.store.update_plan_status(plan_id, PlanStatus::Failed)?;
        self.store.append_event(
            plan_id,
            &PlanEvent::PlanFailed {
                reason: reason.to_string(),
            },
            "orchestrator",
        )?;

        self.require_plan(plan_id)
    }

    pub fn switch_to_smart(&self, plan_id: &str) -> Result<HandoffContext, PlanError> {
        let plan = self.require_plan(plan_id)?;
        if matches!(plan.status, PlanStatus::Completed | PlanStatus::Failed) {
            return Err(PlanError::InvalidStatus {
                status: format!(
                    "switch_to_smart requires non-terminal status, got {}",
                    plan.status
                ),
            });
        }

        let builder = HandoffBuilder::new(&self.store);
        let context = builder.build_quick_to_smart_context(plan_id)?;

        self.store.update_plan_mode(plan_id, PlanMode::Smart)?;
        self.store.update_plan_status(plan_id, PlanStatus::Draft)?;
        self.store.append_event(
            plan_id,
            &PlanEvent::ModeSwitched {
                from: plan.mode,
                to: PlanMode::Smart,
            },
            "orchestrator",
        )?;

        Ok(context)
    }

    pub fn switch_to_quick(&self, plan_id: &str) -> Result<HandoffContext, PlanError> {
        let plan = self.require_plan(plan_id)?;
        if matches!(plan.status, PlanStatus::Completed | PlanStatus::Failed) {
            return Err(PlanError::InvalidStatus {
                status: format!(
                    "switch_to_quick requires non-terminal status, got {}",
                    plan.status
                ),
            });
        }

        let builder = HandoffBuilder::new(&self.store);
        let context = builder.build_smart_to_quick_context(plan_id)?;

        self.store.update_plan_mode(plan_id, PlanMode::Quick)?;
        self.store.append_event(
            plan_id,
            &PlanEvent::ModeSwitched {
                from: plan.mode,
                to: PlanMode::Quick,
            },
            "orchestrator",
        )?;

        Ok(context)
    }

    pub fn ensure_plan(&self, workspace_id: &str, session_id: &str) -> Result<Plan, PlanError> {
        let plans = self.store.list_plans(workspace_id)?;
        if let Some(plan) = plans
            .into_iter()
            .find(|plan| plan.status == PlanStatus::Active)
        {
            return Ok(plan);
        }

        self.create_plan(
            workspace_id,
            "Auto Plan",
            None,
            PlanMode::Quick,
            Some(session_id),
        )
    }

    fn require_plan(&self, plan_id: &str) -> Result<Plan, PlanError> {
        self.store
            .get_plan(plan_id)?
            .ok_or_else(|| PlanError::NotFound {
                id: plan_id.to_string(),
            })
    }
}

fn ensure_status(plan: &Plan, expected: PlanStatus) -> Result<(), PlanError> {
    if plan.status == expected {
        return Ok(());
    }

    Err(PlanError::InvalidStatus {
        status: format!("expected {}, got {}", expected, plan.status),
    })
}
