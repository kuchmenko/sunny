use chrono::{DateTime, Utc};

use crate::error::PlanError;
use crate::events::{PlanEvent, ReplanTrigger};
use crate::store::PlanStore;

#[derive(Debug, Clone)]
pub struct DeviationConfig {
    pub task_timeout_secs: u64,
    pub max_tool_calls: usize,
    pub token_budget_low: u32,
    pub token_budget_moderate: u32,
    pub token_budget_high: u32,
}

impl Default for DeviationConfig {
    fn default() -> Self {
        Self {
            task_timeout_secs: 3_600,
            max_tool_calls: 100,
            token_budget_low: 50_000,
            token_budget_moderate: 100_000,
            token_budget_high: 200_000,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskDeviationState {
    pub task_id: String,
    pub plan_id: String,
    pub started_at: DateTime<Utc>,
    pub tool_call_count: usize,
    pub token_count: u32,
}

#[derive(Debug, Clone)]
pub struct DeviationMonitor {
    config: DeviationConfig,
}

impl Default for DeviationMonitor {
    fn default() -> Self {
        Self::new(DeviationConfig::default())
    }
}

impl DeviationMonitor {
    pub fn new(config: DeviationConfig) -> Self {
        Self { config }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn default() -> Self {
        <Self as Default>::default()
    }

    pub fn check_timeout(&self, state: &TaskDeviationState) -> bool {
        let elapsed = Utc::now().signed_duration_since(state.started_at);
        elapsed.num_seconds() as u64 > self.config.task_timeout_secs
    }

    pub fn check_tool_limit(&self, state: &TaskDeviationState) -> bool {
        state.tool_call_count > self.config.max_tool_calls
    }

    pub fn check_token_budget(&self, state: &TaskDeviationState, effort: &str) -> bool {
        state.token_count > self.token_threshold(effort)
    }

    pub fn check_and_record(
        &self,
        store: &PlanStore,
        state: &TaskDeviationState,
        effort: &str,
    ) -> Result<bool, PlanError> {
        let reason = if self.check_timeout(state) {
            format!(
                "Task {} exceeded timeout of {}s",
                state.task_id, self.config.task_timeout_secs
            )
        } else if self.check_tool_limit(state) {
            format!(
                "Task {} exceeded tool call limit of {}",
                state.task_id, self.config.max_tool_calls
            )
        } else if self.check_token_budget(state, effort) {
            format!(
                "Task {} exceeded token budget for effort level {}",
                state.task_id, effort
            )
        } else {
            return Ok(false);
        };

        store.append_event(
            &state.plan_id,
            &PlanEvent::ReplanTriggered {
                reason,
                trigger: ReplanTrigger::Deviation,
            },
            "deviation_monitor",
        )?;

        Ok(true)
    }

    fn token_threshold(&self, effort: &str) -> u32 {
        match effort {
            "low" => self.config.token_budget_low,
            "moderate" => self.config.token_budget_moderate,
            "high" | "critical" => self.config.token_budget_high,
            _ => self.config.token_budget_low,
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;
    use sunny_tasks::store::TaskStore;

    use super::*;
    use crate::model::PlanMode;

    fn make_store() -> (PlanStore, TaskStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("should create temp dir");
        let db_path = dir.path().join("test.db");

        let task_db = sunny_store::Database::open(db_path.as_path()).expect("should open task db");
        let task_store = TaskStore::new(task_db);

        let plan_db = sunny_store::Database::open(db_path.as_path()).expect("should open plan db");
        let plan_store = PlanStore::new(plan_db);

        (plan_store, task_store, dir)
    }

    fn make_state(
        started_at: DateTime<Utc>,
        tool_call_count: usize,
        token_count: u32,
    ) -> TaskDeviationState {
        TaskDeviationState {
            task_id: "task-1".to_string(),
            plan_id: "plan-1".to_string(),
            started_at,
            tool_call_count,
            token_count,
        }
    }

    #[test]
    fn test_timeout_detection_returns_true_when_elapsed_exceeds_limit() {
        let monitor = DeviationMonitor::new(DeviationConfig {
            task_timeout_secs: 60,
            ..DeviationConfig::default()
        });
        let state = make_state(Utc::now() - Duration::seconds(61), 0, 0);

        assert!(monitor.check_timeout(&state));
    }

    #[test]
    fn test_tool_limit_detection_returns_true_when_count_exceeds_limit() {
        let monitor = DeviationMonitor::new(DeviationConfig {
            max_tool_calls: 3,
            ..DeviationConfig::default()
        });
        let state = make_state(Utc::now(), 4, 0);

        assert!(monitor.check_tool_limit(&state));
    }

    #[test]
    fn test_token_budget_detection_uses_effort_threshold() {
        let monitor = DeviationMonitor::new(DeviationConfig {
            token_budget_low: 10,
            token_budget_moderate: 20,
            token_budget_high: 30,
            ..DeviationConfig::default()
        });
        let state = make_state(Utc::now(), 0, 21);

        assert!(monitor.check_token_budget(&state, "moderate"));
        assert!(!monitor.check_token_budget(&state, "high"));
        assert!(monitor.check_token_budget(&state, "unknown"));
    }

    #[test]
    fn test_check_and_record_appends_replan_event_on_deviation() {
        let (store, task_store, _dir) = make_store();
        let workspace = task_store
            .find_or_create_workspace("/tmp/repo")
            .expect("should create workspace");
        let plan = store
            .create_plan(&workspace.id, "My Plan", None, PlanMode::Quick, None)
            .expect("should create plan");
        let monitor = DeviationMonitor::new(DeviationConfig {
            max_tool_calls: 2,
            ..DeviationConfig::default()
        });
        let mut state = make_state(Utc::now(), 3, 0);
        state.plan_id = plan.id.clone();

        let detected = monitor
            .check_and_record(&store, &state, "low")
            .expect("should record deviation");

        assert!(detected);

        let events = store.get_events(&plan.id).expect("should list events");
        let event = events.last().expect("should append deviation event");
        let trigger = event
            .payload
            .get("trigger")
            .and_then(serde_json::Value::as_str)
            .expect("should include trigger");
        let reason = event
            .payload
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .expect("should include reason");

        assert_eq!(event.event_type, "replan_triggered");
        assert_eq!(event.created_by, "deviation_monitor");
        assert_eq!(trigger, "deviation");
        assert!(reason.contains("tool call limit"));
    }
}
