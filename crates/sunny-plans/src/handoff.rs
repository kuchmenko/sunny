use std::collections::HashMap;

use crate::error::PlanError;
use crate::events::PlanEvent;
use crate::model::{Constraint, Decision, Goal};
use crate::store::{PlanState, PlanStore};

#[derive(Debug, Clone)]
pub struct HandoffContext {
    pub structured: String,
    pub summary: String,
    pub decisions: Vec<Decision>,
    pub constraints: Vec<Constraint>,
}

pub struct HandoffBuilder<'a> {
    pub store: &'a PlanStore,
}

impl<'a> HandoffBuilder<'a> {
    pub fn new(store: &'a PlanStore) -> Self {
        Self { store }
    }

    pub fn build_quick_to_smart_context(&self, plan_id: &str) -> Result<HandoffContext, PlanError> {
        let state = self.store.get_plan_state(plan_id)?;
        let locked_decisions = locked_decisions(&state.decisions);
        let constraints = state.constraints.clone();
        let statuses = latest_task_statuses(&state)?;
        let completed = tasks_with_status(&statuses, "completed");
        let recent_events = format_recent_events(&state, 10)?;

        let structured = format!(
            "## Handoff Context: Quick -> Smart Mode\n\n\
             Plan: {} (ID: {})\n\
             Workspace: {}\n\
             Root Session: {}\n\n\
             ### Completed Tasks\n{}\n\n\
             ### Key Decisions (Locked)\n{}\n\n\
             ### Constraints\n{}\n\n\
             ### Goals\n{}\n\n\
             ### Git State\n{}\n\n\
             ### Recent Events (last 10)\n{}\n",
            state.plan.name,
            state.plan.id,
            state.plan.workspace_id,
            state.plan.root_session_id.as_deref().unwrap_or("(none)"),
            format_string_list(&completed),
            format_decision_list(&locked_decisions),
            format_constraint_list(&constraints),
            format_goal_list(&state.goals),
            "unavailable (git state is not tracked in plan store)",
            recent_events,
        );

        Ok(HandoffContext {
            structured,
            summary: String::new(),
            decisions: locked_decisions,
            constraints,
        })
    }

    pub fn build_smart_to_quick_context(&self, plan_id: &str) -> Result<HandoffContext, PlanError> {
        let state = self.store.get_plan_state(plan_id)?;
        let locked_decisions = locked_decisions(&state.decisions);
        let constraints = state.constraints.clone();
        let statuses = latest_task_statuses(&state)?;
        let completed = tasks_with_status(&statuses, "completed");
        let cancelled = tasks_with_status(&statuses, "cancelled");

        let structured = format!(
            "## Handoff Context: Smart -> Quick Mode\n\n\
             Plan: {} (ID: {})\n\
             Workspace: {}\n\
             Root Session: {}\n\n\
             ### Completed Tasks\n{}\n\n\
             ### Cancelled Tasks\ncount: {}\n{}\n\n\
             ### Key Decisions (Locked)\n{}\n\n\
             ### Constraints\n{}\n",
            state.plan.name,
            state.plan.id,
            state.plan.workspace_id,
            state.plan.root_session_id.as_deref().unwrap_or("(none)"),
            format_string_list(&completed),
            cancelled.len(),
            format_string_list(&cancelled),
            format_decision_list(&locked_decisions),
            format_constraint_list(&constraints),
        );

        Ok(HandoffContext {
            structured,
            summary: String::new(),
            decisions: locked_decisions,
            constraints,
        })
    }

    pub fn build_replan_context(
        &self,
        plan_id: &str,
        deviation_reason: &str,
    ) -> Result<HandoffContext, PlanError> {
        let state = self.store.get_plan_state(plan_id)?;
        let locked_decisions = locked_decisions(&state.decisions);
        let constraints = state.constraints.clone();
        let statuses = latest_task_statuses(&state)?;
        let running = tasks_with_status(&statuses, "running");
        let failed = tasks_with_status(&statuses, "failed");
        let summary = format_status_summary(&statuses);
        let recent_events = format_recent_events(&state, 5)?;

        let structured = format!(
            "## Replan Context\n\n\
             Plan: {} (ID: {})\n\
             Deviation Reason: {}\n\
             Mode: {}\n\n\
             ### Task Summary\n\
             Total tasks: {}\n\
             Status breakdown: {}\n\n\
             ### Running Tasks (Do Not Cancel Unless Specified)\n{}\n\n\
             ### Failed Tasks (Caused This Replan)\n{}\n\n\
             ### Key Decisions (Locked)\n{}\n\n\
             ### Constraints\n{}\n\n\
             ### Recent Events (last 5)\n{}\n",
            state.plan.name,
            state.plan.id,
            deviation_reason,
            state.plan.mode,
            state.task_ids.len(),
            summary,
            format_string_list(&running),
            format_string_list(&failed),
            format_decision_list(&locked_decisions),
            format_constraint_list(&constraints),
            recent_events,
        );

        Ok(HandoffContext {
            structured,
            summary: String::new(),
            decisions: locked_decisions,
            constraints,
        })
    }
}

fn locked_decisions(decisions: &[Decision]) -> Vec<Decision> {
    decisions
        .iter()
        .filter(|decision| decision.is_locked)
        .cloned()
        .collect()
}

fn latest_task_statuses(state: &PlanState) -> Result<HashMap<String, String>, PlanError> {
    let mut statuses = HashMap::new();

    for task_id in &state.task_ids {
        statuses.insert(task_id.clone(), String::from("pending"));
    }

    for event in &state.events {
        let parsed: PlanEvent = serde_json::from_value(event.payload.clone())?;
        match parsed {
            PlanEvent::TaskAdded { task_id, .. } => {
                statuses
                    .entry(task_id)
                    .or_insert_with(|| String::from("pending"));
            }
            PlanEvent::TaskStatusChanged {
                task_id,
                new_status,
                ..
            } => {
                statuses.insert(task_id, new_status);
            }
            _ => {}
        }
    }

    Ok(statuses)
}

fn tasks_with_status(statuses: &HashMap<String, String>, status: &str) -> Vec<String> {
    let mut ids: Vec<String> = statuses
        .iter()
        .filter_map(|(task_id, current)| {
            if current == status {
                Some(task_id.clone())
            } else {
                None
            }
        })
        .collect();
    ids.sort();
    ids
}

fn format_recent_events(state: &PlanState, limit: usize) -> Result<String, PlanError> {
    let start = state.events.len().saturating_sub(limit);
    let mut lines = Vec::new();
    for event in &state.events[start..] {
        let parsed: PlanEvent = serde_json::from_value(event.payload.clone())?;
        lines.push(format!(
            "- [{}] {}",
            event.sequence,
            event_summary(&parsed, &event.event_type)
        ));
    }
    if lines.is_empty() {
        Ok(String::from("- (none)"))
    } else {
        Ok(lines.join("\n"))
    }
}

fn event_summary(event: &PlanEvent, fallback_event_type: &str) -> String {
    match event {
        PlanEvent::TaskAdded { task_id, title, .. } => {
            format!("task_added: {} ({})", task_id, title)
        }
        PlanEvent::TaskRemoved { task_id, strategy } => {
            format!("task_removed: {} ({:?})", task_id, strategy)
        }
        PlanEvent::DependencyAdded { from_task, to_task } => {
            format!("dependency_added: {} -> {}", from_task, to_task)
        }
        PlanEvent::DependencyRemoved { from_task, to_task } => {
            format!("dependency_removed: {} -> {}", from_task, to_task)
        }
        PlanEvent::DecisionRecorded { decision, .. } => format!("decision_recorded: {}", decision),
        PlanEvent::ConstraintAdded { description, .. } => {
            format!("constraint_added: {}", description)
        }
        PlanEvent::GoalAdded { description, .. } => format!("goal_added: {}", description),
        PlanEvent::GoalStatusChanged {
            goal_id,
            new_status,
            ..
        } => format!("goal_status_changed: {} -> {}", goal_id, new_status),
        PlanEvent::PlanFinalized { validation_result } => {
            format!("plan_finalized: {}", validation_result)
        }
        PlanEvent::PlanActivated => String::from("plan_activated"),
        PlanEvent::PlanCompleted { summary } => format!("plan_completed: {}", summary),
        PlanEvent::PlanFailed { reason } => format!("plan_failed: {}", reason),
        PlanEvent::ModeSwitched { from, to } => format!("mode_switched: {} -> {}", from, to),
        PlanEvent::ReplanTriggered { reason, trigger } => {
            format!("replan_triggered: {:?} ({})", trigger, reason)
        }
        PlanEvent::TaskStatusChanged {
            task_id,
            new_status,
            ..
        } => format!("task_status_changed: {} -> {}", task_id, new_status),
    }
    .trim()
    .to_string()
    .if_empty_or_else(|| fallback_event_type.to_string())
}

fn format_status_summary(statuses: &HashMap<String, String>) -> String {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for status in statuses.values() {
        let entry = counts.entry(status.clone()).or_insert(0);
        *entry += 1;
    }

    let order = [
        "pending",
        "running",
        "blocked_human",
        "completed",
        "failed",
        "cancelled",
        "suspended",
    ];

    let mut parts = Vec::new();
    for status in order {
        if let Some(count) = counts.remove(status) {
            parts.push(format!("{}={}", status, count));
        }
    }

    let mut remaining: Vec<(String, usize)> = counts.into_iter().collect();
    remaining.sort_by(|left, right| left.0.cmp(&right.0));
    for (status, count) in remaining {
        parts.push(format!("{}={}", status, count));
    }

    if parts.is_empty() {
        String::from("(none)")
    } else {
        parts.join(", ")
    }
}

fn format_string_list(items: &[String]) -> String {
    if items.is_empty() {
        return String::from("- (none)");
    }

    items
        .iter()
        .map(|item| format!("- {}", item))
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_decision_list(items: &[Decision]) -> String {
    if items.is_empty() {
        return String::from("- (none)");
    }

    items
        .iter()
        .map(|decision| {
            if let Some(rationale) = decision.rationale.as_deref() {
                format!("- {} (rationale: {})", decision.decision, rationale)
            } else {
                format!("- {}", decision.decision)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_constraint_list(items: &[Constraint]) -> String {
    if items.is_empty() {
        return String::from("- (none)");
    }

    items
        .iter()
        .map(|constraint| {
            format!(
                "- {}: {}",
                constraint.constraint_type.as_str(),
                constraint.description
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn format_goal_list(items: &[Goal]) -> String {
    if items.is_empty() {
        return String::from("- (none)");
    }

    items
        .iter()
        .map(|goal| {
            format!(
                "- {}: {} [{}]",
                goal.priority.as_str(),
                goal.description,
                goal.status.as_str()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

trait StringFallback {
    fn if_empty_or_else(self, fallback: impl FnOnce() -> String) -> String;
}

impl StringFallback for String {
    fn if_empty_or_else(self, fallback: impl FnOnce() -> String) -> String {
        if self.is_empty() {
            fallback()
        } else {
            self
        }
    }
}
