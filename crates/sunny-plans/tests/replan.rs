#![allow(clippy::arc_with_non_send_sync)]

use std::sync::Arc;

use serde_json::json;
use sunny_plans::{
    model::PlanMode,
    orchestrator::PlanOrchestrator,
    replan::{NewTaskSpec, ReplanCoordinator, ReplanRequest},
    store::PlanStore,
    tools::handlers::handle_task_request_replan,
};
use sunny_tasks::store::TaskStore;

fn make_plan_store() -> (Arc<PlanStore>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("should create temp dir");
    let task_db =
        sunny_store::Database::open(dir.path().join("test.db").as_path()).expect("should open db");
    let _ = TaskStore::new(task_db);
    let db =
        sunny_store::Database::open(dir.path().join("test.db").as_path()).expect("should open db");
    (Arc::new(PlanStore::new(db)), dir)
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
fn test_task_request_replan_creates_agent_request_event_and_pending_state() {
    let (store, dir) = make_plan_store();
    let workspace_id = make_workspace_id(&dir);
    let orchestrator = PlanOrchestrator::new(Arc::clone(&store));
    let coordinator = ReplanCoordinator::new(Arc::clone(&store));

    let plan = orchestrator
        .create_plan(&workspace_id, "replan test", None, PlanMode::Quick, None)
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

    orchestrator
        .finalize_plan(&plan.id)
        .expect("should finalize plan");
    orchestrator
        .activate_plan(&plan.id)
        .expect("should activate plan");

    let args = json!({
        "plan_id": plan.id,
        "reason": "blocking issue discovered"
    });
    let result = handle_task_request_replan(&store, &args).expect("handler should succeed");
    let response: serde_json::Value =
        serde_json::from_str(&result).expect("response should be json");

    assert_eq!(response["status"], "replan_requested");
    assert!(coordinator
        .has_pending_replan(
            response["plan_id"]
                .as_str()
                .expect("plan_id should be string")
        )
        .expect("pending replan should be checked"));

    let events = store
        .get_events(
            response["plan_id"]
                .as_str()
                .expect("plan_id should be string"),
        )
        .expect("should get events");
    let replan_event = events
        .iter()
        .find(|event| event.event_type == "replan_triggered")
        .expect("replan event should exist");

    assert_eq!(replan_event.payload["type"], "replan_triggered");
    assert_eq!(replan_event.payload["trigger"], "agent_request");
}

#[test]
fn test_apply_replan_cancels_requested_tasks() {
    let (store, dir) = make_plan_store();
    let workspace_id = make_workspace_id(&dir);
    let orchestrator = PlanOrchestrator::new(Arc::clone(&store));
    let coordinator = ReplanCoordinator::new(Arc::clone(&store));

    let plan = orchestrator
        .create_plan(&workspace_id, "cancel test", None, PlanMode::Quick, None)
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

    let result = coordinator
        .apply_replan(ReplanRequest {
            plan_id: plan.id.clone(),
            reason: "drop task b".to_string(),
            tasks_to_cancel: vec!["task-b".to_string()],
            new_task_specs: vec![],
        })
        .expect("replan should succeed");

    let state = store.get_plan_state(&plan.id).expect("should load state");

    assert!(!state.task_ids.contains(&"task-b".to_string()));
    assert_eq!(result.cancelled_tasks, vec!["task-b".to_string()]);
}

#[test]
fn test_apply_replan_adds_new_tasks() {
    let (store, dir) = make_plan_store();
    let workspace_id = make_workspace_id(&dir);
    let orchestrator = PlanOrchestrator::new(Arc::clone(&store));
    let coordinator = ReplanCoordinator::new(Arc::clone(&store));

    let plan = orchestrator
        .create_plan(&workspace_id, "add task test", None, PlanMode::Quick, None)
        .expect("should create plan");

    store
        .add_task_to_plan(&plan.id, "task-a", "Task A", &[])
        .expect("should add task a");

    coordinator
        .apply_replan(ReplanRequest {
            plan_id: plan.id.clone(),
            reason: "add follow-up task".to_string(),
            tasks_to_cancel: vec![],
            new_task_specs: vec![NewTaskSpec {
                task_id: "task-d".to_string(),
                title: "Task D".to_string(),
                dep_ids: vec!["task-a".to_string()],
            }],
        })
        .expect("replan should succeed");

    let state = store.get_plan_state(&plan.id).expect("should load state");
    assert!(state.task_ids.contains(&"task-d".to_string()));
}
