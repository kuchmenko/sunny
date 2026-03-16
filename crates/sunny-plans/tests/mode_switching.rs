#![allow(clippy::arc_with_non_send_sync)]

use std::sync::Arc;

use sunny_plans::{
    events::PlanEvent,
    model::{PlanMode, PlanStatus},
    orchestrator::PlanOrchestrator,
    store::PlanStore,
};
use sunny_tasks::store::TaskStore;

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

#[test]
fn test_switch_quick_to_smart_resets_to_draft_and_keeps_work() {
    let (store, dir) = make_plan_store();
    let workspace_id = make_workspace_id(&dir);
    let store = Arc::new(store);
    let orchestrator = PlanOrchestrator::new(Arc::clone(&store));

    let plan = orchestrator
        .create_plan(&workspace_id, "mode switch", None, PlanMode::Quick, None)
        .expect("should create plan");

    store
        .add_task_to_plan(&plan.id, "done-task", "Done task", &[])
        .expect("should add task");
    store
        .append_event(
            &plan.id,
            &PlanEvent::TaskStatusChanged {
                task_id: "done-task".to_string(),
                old_status: "pending".to_string(),
                new_status: "completed".to_string(),
            },
            "test",
        )
        .expect("should append completed event");

    orchestrator
        .finalize_plan(&plan.id)
        .expect("should finalize plan");
    orchestrator
        .activate_plan(&plan.id)
        .expect("should activate plan");

    orchestrator
        .switch_to_smart(&plan.id)
        .expect("should switch to smart");

    let state = store
        .get_plan_state(&plan.id)
        .expect("should read updated state");
    assert_eq!(state.plan.mode, PlanMode::Smart);
    assert_eq!(state.plan.status, PlanStatus::Draft);
    assert!(state.task_ids.iter().any(|id| id == "done-task"));
    assert!(state
        .events
        .iter()
        .any(|event| event.event_type == "mode_switched"));
}

#[test]
fn test_switch_smart_to_quick_changes_mode() {
    let (store, dir) = make_plan_store();
    let workspace_id = make_workspace_id(&dir);
    let store = Arc::new(store);
    let orchestrator = PlanOrchestrator::new(Arc::clone(&store));

    let plan = orchestrator
        .create_plan(&workspace_id, "mode switch", None, PlanMode::Smart, None)
        .expect("should create smart plan");

    orchestrator
        .switch_to_quick(&plan.id)
        .expect("should switch to quick");

    let updated = store
        .get_plan(&plan.id)
        .expect("should fetch plan")
        .expect("plan should exist");
    assert_eq!(updated.mode, PlanMode::Quick);
}

#[test]
fn test_round_trip_quick_smart_quick_preserves_tasks() {
    let (store, dir) = make_plan_store();
    let workspace_id = make_workspace_id(&dir);
    let store = Arc::new(store);
    let orchestrator = PlanOrchestrator::new(Arc::clone(&store));

    let plan = orchestrator
        .create_plan(&workspace_id, "round trip", None, PlanMode::Quick, None)
        .expect("should create plan");

    store
        .add_task_to_plan(&plan.id, "task-a", "Task A", &[])
        .expect("should add task a");
    store
        .add_task_to_plan(&plan.id, "task-b", "Task B", &[])
        .expect("should add task b");
    store
        .add_task_to_plan(&plan.id, "task-c", "Task C", &[])
        .expect("should add task c");
    store
        .add_dependency(&plan.id, "task-c", "task-a")
        .expect("should add dependency");

    orchestrator
        .finalize_plan(&plan.id)
        .expect("should finalize plan");

    orchestrator
        .switch_to_smart(&plan.id)
        .expect("should switch to smart");
    orchestrator
        .switch_to_quick(&plan.id)
        .expect("should switch back to quick");

    let state = store
        .get_plan_state(&plan.id)
        .expect("should load final state");
    assert_eq!(state.plan.mode, PlanMode::Quick);
    assert!(state.task_ids.iter().any(|id| id == "task-a"));
    assert!(state.task_ids.iter().any(|id| id == "task-b"));
    assert!(state.task_ids.iter().any(|id| id == "task-c"));

    let mode_switch_events = state
        .events
        .iter()
        .filter(|event| event.event_type == "mode_switched")
        .count();
    assert_eq!(mode_switch_events, 2);
}
