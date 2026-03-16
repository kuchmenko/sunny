#![allow(clippy::arc_with_non_send_sync)]

use std::sync::Arc;

use sunny_plans::{
    error::PlanError,
    model::{ConstraintType, DecisionAuthor, GoalPriority, GoalStatus, PlanMode, PlanStatus},
    orchestrator::PlanOrchestrator,
    store::PlanStore,
};
use sunny_tasks::store::TaskStore;
use uuid::Uuid;

fn make_plan_store() -> (PlanStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("should create temp dir");
    let task_db =
        sunny_store::Database::open(dir.path().join("test.db").as_path()).expect("should open db");
    let _ = TaskStore::new(task_db);
    let db =
        sunny_store::Database::open(dir.path().join("test.db").as_path()).expect("should open db");
    (PlanStore::new(db), dir)
}

fn make_workspace_id(dir: &tempfile::TempDir) -> String {
    let db =
        sunny_store::Database::open(dir.path().join("test.db").as_path()).expect("should open db");
    let task_store = TaskStore::new(db);
    task_store
        .find_or_create_workspace("/tmp/repo")
        .expect("should create workspace")
        .id
}

fn assert_event_order(event_types: &[String], expected_order: &[&str]) {
    let mut cursor = 0usize;
    for expected in expected_order {
        let rel_pos = event_types[cursor..]
            .iter()
            .position(|actual| actual == expected)
            .unwrap_or_else(|| panic!("missing event type in order: {expected}"));
        cursor += rel_pos + 1;
    }
}

#[test]
fn test_plan_full_lifecycle_with_event_log_order() {
    let (store, dir) = make_plan_store();
    let workspace_id = make_workspace_id(&dir);
    let store = Arc::new(store);
    let orchestrator = PlanOrchestrator::new(Arc::clone(&store));

    let plan = orchestrator
        .create_plan(&workspace_id, "test plan", None, PlanMode::Quick, None)
        .expect("should create plan");

    let task_a = Uuid::new_v4().to_string();
    let task_b = Uuid::new_v4().to_string();
    let task_c = Uuid::new_v4().to_string();

    store
        .add_task_to_plan(&plan.id, &task_a, "Task A", &[])
        .expect("should add task A");
    store
        .add_task_to_plan(&plan.id, &task_b, "Task B", &[])
        .expect("should add task B");
    store
        .add_task_to_plan(&plan.id, &task_c, "Task C", &[])
        .expect("should add task C");

    store
        .add_dependency(&plan.id, &task_a, &task_b)
        .expect("should add dependency A->B");
    store
        .add_dependency(&plan.id, &task_b, &task_c)
        .expect("should add dependency B->C");

    store
        .add_decision(
            &plan.id,
            "Use rusqlite for storage",
            Some("Match existing task storage pattern"),
            DecisionAuthor::User,
            None,
            true,
        )
        .expect("should add decision");

    store
        .add_constraint(&plan.id, ConstraintType::MustDo, "Follow AGENTS.md", None)
        .expect("should add constraint");

    let goal = store
        .add_goal(&plan.id, "Plan lifecycle works", GoalPriority::Critical)
        .expect("should add goal");

    orchestrator
        .finalize_plan(&plan.id)
        .expect("should finalize plan");
    orchestrator
        .activate_plan(&plan.id)
        .expect("should activate plan");

    store
        .update_goal_status(&goal.id, GoalStatus::Achieved)
        .expect("should update goal status");

    orchestrator
        .complete_plan(&plan.id, "done")
        .expect("should complete plan");

    let state = store
        .get_plan_state(&plan.id)
        .expect("should load plan state");

    assert_eq!(state.plan.status, PlanStatus::Completed);
    assert_eq!(state.decisions.len(), 1);
    assert_eq!(state.constraints.len(), 1);
    assert!(state.events.len() >= 11);

    let event_types: Vec<String> = state
        .events
        .iter()
        .map(|event| event.event_type.clone())
        .collect();
    assert_event_order(
        &event_types,
        &[
            "task_added",
            "task_added",
            "task_added",
            "dependency_added",
            "dependency_added",
            "decision_recorded",
            "constraint_added",
            "goal_added",
            "plan_finalized",
            "plan_activated",
            "goal_status_changed",
            "plan_completed",
        ],
    );
}

#[test]
fn test_plan_invalid_transitions_rejected_by_orchestrator() {
    let (store, dir) = make_plan_store();
    let workspace_id = make_workspace_id(&dir);
    let store = Arc::new(store);
    let orchestrator = PlanOrchestrator::new(Arc::clone(&store));

    let plan = orchestrator
        .create_plan(&workspace_id, "test plan", None, PlanMode::Quick, None)
        .expect("should create plan");

    let activate_err = orchestrator
        .activate_plan(&plan.id)
        .expect_err("draft->active should be rejected");
    assert!(matches!(activate_err, PlanError::InvalidStatus { .. }));

    orchestrator
        .finalize_plan(&plan.id)
        .expect("should finalize plan");

    store
        .add_decision(
            &plan.id,
            "Decisions can be added while ready",
            Some("Planning context can still evolve"),
            DecisionAuthor::Planner,
            None,
            true,
        )
        .expect("should add decision in ready status");

    let complete_err = orchestrator
        .complete_plan(&plan.id, "done")
        .expect_err("ready->completed should be rejected");
    assert!(matches!(complete_err, PlanError::InvalidStatus { .. }));

    orchestrator
        .activate_plan(&plan.id)
        .expect("should activate plan");
    let finalize_err = orchestrator
        .finalize_plan(&plan.id)
        .expect_err("active->ready via finalize should be rejected");
    assert!(matches!(finalize_err, PlanError::InvalidStatus { .. }));
}

#[test]
fn test_plan_dependency_cycle_detection() {
    let (store, dir) = make_plan_store();
    let workspace_id = make_workspace_id(&dir);
    let plan = store
        .create_plan(&workspace_id, "cycle test", None, PlanMode::Quick, None)
        .expect("should create plan");

    let task_x = Uuid::new_v4().to_string();
    let task_y = Uuid::new_v4().to_string();

    store
        .add_task_to_plan(&plan.id, &task_x, "Task X", &[])
        .expect("should add task X");
    store
        .add_task_to_plan(&plan.id, &task_y, "Task Y", &[])
        .expect("should add task Y");

    store
        .add_dependency(&plan.id, &task_x, &task_y)
        .expect("x->y should be valid");

    let err = store
        .add_dependency(&plan.id, &task_y, &task_x)
        .expect_err("y->x should create cycle");
    assert!(matches!(err, PlanError::CycleDetected));
}
