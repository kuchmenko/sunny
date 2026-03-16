//! Plan event types for event sourcing.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::model::PlanMode;

/// Plan event enum with 15 variants for event sourcing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanEvent {
    /// Task added to plan with dependencies.
    TaskAdded {
        task_id: String,
        title: String,
        dep_ids: Vec<String>,
    },
    /// Task removed from plan with removal strategy.
    TaskRemoved {
        task_id: String,
        strategy: RemovalStrategy,
    },
    /// Dependency added between tasks.
    DependencyAdded { from_task: String, to_task: String },
    /// Dependency removed between tasks.
    DependencyRemoved { from_task: String, to_task: String },
    /// Decision recorded in plan.
    DecisionRecorded {
        decision_id: String,
        decision: String,
        rationale: Option<String>,
    },
    /// Constraint added to plan.
    ConstraintAdded {
        constraint_id: String,
        constraint_type: String,
        description: String,
    },
    /// Goal added to plan.
    GoalAdded {
        goal_id: String,
        description: String,
        priority: String,
    },
    /// Goal status changed.
    GoalStatusChanged {
        goal_id: String,
        old_status: String,
        new_status: String,
    },
    /// Plan finalized with validation result.
    PlanFinalized { validation_result: String },
    /// Plan activated for execution.
    PlanActivated,
    /// Plan completed successfully.
    PlanCompleted { summary: String },
    /// Plan failed with reason.
    PlanFailed { reason: String },
    /// Plan mode switched.
    ModeSwitched { from: PlanMode, to: PlanMode },
    /// Replan triggered with reason and trigger type.
    ReplanTriggered {
        reason: String,
        trigger: ReplanTrigger,
    },
    /// Task status changed.
    TaskStatusChanged {
        task_id: String,
        old_status: String,
        new_status: String,
    },
}

/// Strategy for removing a task from the plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalStrategy {
    /// Skip the task without affecting dependencies.
    Skip,
    /// Bridge dependencies (connect predecessors to successors).
    Bridge,
    /// Cascade removal to dependent tasks.
    Cascade,
}

/// Trigger reason for replanning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplanTrigger {
    /// Triggered by task failure.
    TaskFailure,
    /// Triggered by user request.
    UserRequest,
    /// Triggered by agent request.
    AgentRequest,
    /// Triggered by plan deviation.
    Deviation,
}

/// Stored event row from plan_events table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEvent {
    /// Database row ID.
    pub id: Option<i64>,
    /// Plan ID this event belongs to.
    pub plan_id: String,
    /// Event sequence number.
    pub sequence: i64,
    /// Event type string.
    pub event_type: String,
    /// Event payload as JSON.
    pub payload: serde_json::Value,
    /// User/agent that created this event.
    pub created_by: String,
    /// Timestamp when event was created.
    pub created_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(event: &PlanEvent) -> PlanEvent {
        let json = serde_json::to_string(event).expect("should serialize");
        serde_json::from_str(&json).expect("should deserialize")
    }

    #[test]
    fn test_task_added_roundtrip() {
        let event = PlanEvent::TaskAdded {
            task_id: "t1".into(),
            title: "Task 1".into(),
            dep_ids: vec!["t0".into()],
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_task_removed_roundtrip() {
        let event = PlanEvent::TaskRemoved {
            task_id: "t1".into(),
            strategy: RemovalStrategy::Bridge,
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_dependency_added_roundtrip() {
        let event = PlanEvent::DependencyAdded {
            from_task: "t1".into(),
            to_task: "t2".into(),
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_dependency_removed_roundtrip() {
        let event = PlanEvent::DependencyRemoved {
            from_task: "t1".into(),
            to_task: "t2".into(),
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_decision_recorded_roundtrip() {
        let event = PlanEvent::DecisionRecorded {
            decision_id: "d1".into(),
            decision: "Use async Rust".into(),
            rationale: Some("Better performance".into()),
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_decision_recorded_no_rationale_roundtrip() {
        let event = PlanEvent::DecisionRecorded {
            decision_id: "d2".into(),
            decision: "Use sync".into(),
            rationale: None,
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_constraint_added_roundtrip() {
        let event = PlanEvent::ConstraintAdded {
            constraint_id: "c1".into(),
            constraint_type: "deadline".into(),
            description: "Must complete by Friday".into(),
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_goal_added_roundtrip() {
        let event = PlanEvent::GoalAdded {
            goal_id: "g1".into(),
            description: "Implement feature X".into(),
            priority: "high".into(),
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_goal_status_changed_roundtrip() {
        let event = PlanEvent::GoalStatusChanged {
            goal_id: "g1".into(),
            old_status: "pending".into(),
            new_status: "in_progress".into(),
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_plan_finalized_roundtrip() {
        let event = PlanEvent::PlanFinalized {
            validation_result: "valid".into(),
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_plan_activated_roundtrip() {
        let event = PlanEvent::PlanActivated;
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_plan_completed_roundtrip() {
        let event = PlanEvent::PlanCompleted {
            summary: "All tasks completed".into(),
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_plan_failed_roundtrip() {
        let event = PlanEvent::PlanFailed {
            reason: "Critical task failed".into(),
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_mode_switched_roundtrip() {
        let event = PlanEvent::ModeSwitched {
            from: PlanMode::Quick,
            to: PlanMode::Smart,
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_replan_triggered_roundtrip() {
        let event = PlanEvent::ReplanTriggered {
            reason: "Task failed unexpectedly".into(),
            trigger: ReplanTrigger::TaskFailure,
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_task_status_changed_roundtrip() {
        let event = PlanEvent::TaskStatusChanged {
            task_id: "t1".into(),
            old_status: "pending".into(),
            new_status: "completed".into(),
        };
        assert_eq!(roundtrip(&event), event);
    }

    #[test]
    fn test_removal_strategy_skip_roundtrip() {
        let strategy = RemovalStrategy::Skip;
        let json = serde_json::to_string(&strategy).expect("should serialize");
        let deserialized: RemovalStrategy =
            serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(deserialized, strategy);
    }

    #[test]
    fn test_removal_strategy_bridge_roundtrip() {
        let strategy = RemovalStrategy::Bridge;
        let json = serde_json::to_string(&strategy).expect("should serialize");
        let deserialized: RemovalStrategy =
            serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(deserialized, strategy);
    }

    #[test]
    fn test_removal_strategy_cascade_roundtrip() {
        let strategy = RemovalStrategy::Cascade;
        let json = serde_json::to_string(&strategy).expect("should serialize");
        let deserialized: RemovalStrategy =
            serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(deserialized, strategy);
    }

    #[test]
    fn test_replan_trigger_task_failure_roundtrip() {
        let trigger = ReplanTrigger::TaskFailure;
        let json = serde_json::to_string(&trigger).expect("should serialize");
        let deserialized: ReplanTrigger = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(deserialized, trigger);
    }

    #[test]
    fn test_replan_trigger_user_request_roundtrip() {
        let trigger = ReplanTrigger::UserRequest;
        let json = serde_json::to_string(&trigger).expect("should serialize");
        let deserialized: ReplanTrigger = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(deserialized, trigger);
    }

    #[test]
    fn test_replan_trigger_agent_request_roundtrip() {
        let trigger = ReplanTrigger::AgentRequest;
        let json = serde_json::to_string(&trigger).expect("should serialize");
        let deserialized: ReplanTrigger = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(deserialized, trigger);
    }

    #[test]
    fn test_replan_trigger_deviation_roundtrip() {
        let trigger = ReplanTrigger::Deviation;
        let json = serde_json::to_string(&trigger).expect("should serialize");
        let deserialized: ReplanTrigger = serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(deserialized, trigger);
    }

    #[test]
    fn test_plan_event_json_structure() {
        let event = PlanEvent::TaskAdded {
            task_id: "t1".into(),
            title: "Task 1".into(),
            dep_ids: vec!["t0".into()],
        };
        let json = serde_json::to_string(&event).expect("should serialize");
        // Verify the JSON has the expected structure with "type" tag
        assert!(json.contains("\"type\":\"task_added\""));
        assert!(json.contains("\"task_id\":\"t1\""));
    }
}
