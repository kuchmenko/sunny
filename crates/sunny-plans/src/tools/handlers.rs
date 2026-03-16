//! Plan tool handlers for executing plan-building operations.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::str::FromStr;

use serde_json::{json, Value};

use crate::error::PlanError;
use crate::events::{PlanEvent, RemovalStrategy, ReplanTrigger};
use crate::model::{
    ConstraintType, DecisionAuthor, DecisionType, GoalPriority, GoalStatus, PlanMode, PlanStatus,
};
use crate::store::{PlanState, PlanStore};

pub fn handle_plan_create(store: &PlanStore, args: &Value) -> Result<String, PlanError> {
    respond(|| {
        let name = required_str(args, "name")?;
        let mode = parse_mode(args)?;
        let description = args.get("description").and_then(Value::as_str);
        let workspace_id = args
            .get("workspace_id")
            .and_then(Value::as_str)
            .unwrap_or("default");
        let root_session_id = args.get("root_session_id").and_then(Value::as_str);

        let plan = store.create_plan(workspace_id, name, description, mode, root_session_id)?;

        Ok(json!({
            "plan_id": plan.id,
            "workspace_id": plan.workspace_id,
            "name": plan.name,
            "description": plan.description,
            "mode": plan.mode.to_string(),
            "status": plan.status.to_string(),
            "root_session_id": plan.root_session_id
        }))
    })
}

pub fn handle_plan_add_task(store: &PlanStore, args: &Value) -> Result<String, PlanError> {
    respond(|| {
        let plan_id = required_str(args, "plan_id")?;
        let task_id = required_str(args, "task_id")?;
        let title = required_str(args, "title")?;
        let dep_ids = optional_string_array(args, "dep_ids")?;

        store.add_task_to_plan(plan_id, task_id, title, &dep_ids)?;

        Ok(json!({
            "plan_id": plan_id,
            "task_id": task_id,
            "title": title,
            "dep_ids": dep_ids,
            "status": "added"
        }))
    })
}

pub fn handle_plan_add_dependency(store: &PlanStore, args: &Value) -> Result<String, PlanError> {
    respond(|| {
        let plan_id = required_str(args, "plan_id")?;
        let from_task = required_str(args, "from_task")?;
        let to_task = required_str(args, "to_task")?;

        store.add_dependency(plan_id, from_task, to_task)?;

        Ok(json!({
            "plan_id": plan_id,
            "from_task": from_task,
            "to_task": to_task,
            "status": "added"
        }))
    })
}

pub fn handle_plan_remove_task(store: &PlanStore, args: &Value) -> Result<String, PlanError> {
    respond(|| {
        let plan_id = required_str(args, "plan_id")?;
        let task_id = required_str(args, "task_id")?;
        let strategy = parse_removal_strategy(args)?;

        let state = store.get_plan_state(plan_id)?;
        let graph = build_graph(&state);

        match strategy {
            RemovalStrategy::Skip => {
                store.remove_task_from_plan(plan_id, task_id)?;
                Ok(json!({
                    "plan_id": plan_id,
                    "task_id": task_id,
                    "strategy": "skip",
                    "status": "removed"
                }))
            }
            RemovalStrategy::Bridge => {
                let predecessors = graph
                    .deps
                    .get(task_id)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .collect::<Vec<_>>();
                let successors = graph
                    .tasks
                    .iter()
                    .filter(|candidate| graph.depends_on(candidate, task_id))
                    .cloned()
                    .collect::<Vec<_>>();

                for successor in &successors {
                    for predecessor in &predecessors {
                        if successor != predecessor
                            && successor.as_str() != task_id
                            && predecessor.as_str() != task_id
                        {
                            store.add_dependency(plan_id, successor, predecessor)?;
                        }
                    }
                }

                store.remove_task_from_plan(plan_id, task_id)?;
                store.append_event(
                    plan_id,
                    &PlanEvent::TaskRemoved {
                        task_id: task_id.to_string(),
                        strategy: RemovalStrategy::Bridge,
                    },
                    "system",
                )?;

                Ok(json!({
                    "plan_id": plan_id,
                    "task_id": task_id,
                    "strategy": "bridge",
                    "bridged_predecessors": predecessors,
                    "bridged_successors": successors,
                    "status": "removed"
                }))
            }
            RemovalStrategy::Cascade => {
                let to_remove = collect_dependents(task_id, &graph);
                let mut removed_ids = to_remove.into_iter().collect::<Vec<_>>();
                removed_ids.sort();

                for id in &removed_ids {
                    store.remove_task_from_plan(plan_id, id)?;
                }

                store.append_event(
                    plan_id,
                    &PlanEvent::TaskRemoved {
                        task_id: task_id.to_string(),
                        strategy: RemovalStrategy::Cascade,
                    },
                    "system",
                )?;

                Ok(json!({
                    "plan_id": plan_id,
                    "task_id": task_id,
                    "strategy": "cascade",
                    "removed_task_ids": removed_ids,
                    "status": "removed"
                }))
            }
        }
    })
}

pub fn handle_plan_query_state(store: &PlanStore, args: &Value) -> Result<String, PlanError> {
    respond(|| {
        let plan_id = required_str(args, "plan_id")?;
        let state = store.get_plan_state(plan_id)?;
        Ok(json!({
            "plan": state.plan,
            "task_ids": state.task_ids,
            "decisions": state.decisions,
            "constraints": state.constraints,
            "goals": state.goals,
            "events": state.events
        }))
    })
}

pub fn handle_plan_finalize(store: &PlanStore, args: &Value) -> Result<String, PlanError> {
    respond(|| {
        let plan_id = required_str(args, "plan_id")?;
        let plan = store
            .get_plan(plan_id)?
            .ok_or_else(|| PlanError::NotFound {
                id: plan_id.to_string(),
            })?;

        match plan.status {
            PlanStatus::Ready => {
                // Idempotent: re-validate DAG, return success without new event
                let state = store.get_plan_state(plan_id)?;
                validate_dag(&state)?;
                return Ok(json!({
                    "plan_id": plan_id,
                    "status": "ready"
                }));
            }
            PlanStatus::Draft => {
                // Fall through to existing Draft→Ready logic
            }
            _ => {
                return Ok(json!({
                    "error": "invalid_status",
                    "message": format!("plan must be in Draft or Ready status, got {}", plan.status)
                }));
            }
        }

        let state = store.get_plan_state(plan_id)?;
        validate_dag(&state)?;

        store.update_plan_status(plan_id, PlanStatus::Ready)?;
        store.append_event(
            plan_id,
            &PlanEvent::PlanFinalized {
                validation_result: "ok".to_string(),
            },
            "system",
        )?;

        Ok(json!({
            "plan_id": plan_id,
            "status": "ready"
        }))
    })
}

pub fn handle_plan_replan(store: &PlanStore, args: &Value) -> Result<String, PlanError> {
    respond(|| {
        let plan_id = required_str(args, "plan_id")?;
        let changes = required_str(args, "changes")?;
        let tasks_to_cancel = optional_string_array(args, "tasks_to_cancel")?;

        for task_id in &tasks_to_cancel {
            store.remove_task_from_plan(plan_id, task_id)?;
        }

        store.append_event(
            plan_id,
            &PlanEvent::ReplanTriggered {
                reason: changes.to_string(),
                trigger: ReplanTrigger::UserRequest,
            },
            "system",
        )?;

        Ok(json!({
            "plan_id": plan_id,
            "status": "replan_triggered",
            "changes": changes,
            "tasks_canceled": tasks_to_cancel
        }))
    })
}

pub fn handle_plan_record_decision(store: &PlanStore, args: &Value) -> Result<String, PlanError> {
    respond(|| {
        let plan_id = required_str(args, "plan_id")?;
        let decision = required_str(args, "decision")?;
        let rationale = args.get("rationale").and_then(Value::as_str);
        let decided_by = parse_decision_author(args)?;
        let decision_type = parse_optional_decision_type(args)?;
        let is_locked = args
            .get("is_locked")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let recorded = store.add_decision(
            plan_id,
            decision,
            rationale,
            decided_by,
            decision_type,
            is_locked,
        )?;

        Ok(json!({
            "plan_id": plan_id,
            "decision_id": recorded.id,
            "decision": recorded.decision,
            "rationale": recorded.rationale,
            "decided_by": recorded.decided_by.to_string(),
            "decision_type": recorded.decision_type.map(|value| value.to_string()),
            "is_locked": recorded.is_locked,
            "status": "recorded"
        }))
    })
}

pub fn handle_plan_add_constraint(store: &PlanStore, args: &Value) -> Result<String, PlanError> {
    respond(|| {
        let plan_id = required_str(args, "plan_id")?;
        let constraint_type = parse_constraint_type(args)?;
        let description = required_str(args, "description")?;
        let source_decision_id = args.get("source_decision_id").and_then(Value::as_str);

        let constraint =
            store.add_constraint(plan_id, constraint_type, description, source_decision_id)?;

        Ok(json!({
            "plan_id": plan_id,
            "constraint_id": constraint.id,
            "constraint_type": constraint.constraint_type.to_string(),
            "description": constraint.description,
            "source_decision_id": constraint.source_decision_id,
            "status": "added"
        }))
    })
}

pub fn handle_plan_add_goal(store: &PlanStore, args: &Value) -> Result<String, PlanError> {
    respond(|| {
        let plan_id = required_str(args, "plan_id")?;
        let description = required_str(args, "description")?;
        let priority = parse_goal_priority(args)?;

        let goal = store.add_goal(plan_id, description, priority)?;

        Ok(json!({
            "plan_id": plan_id,
            "goal_id": goal.id,
            "description": goal.description,
            "priority": goal.priority.to_string(),
            "status": goal.status.to_string()
        }))
    })
}

pub fn handle_plan_update_goal(store: &PlanStore, args: &Value) -> Result<String, PlanError> {
    respond(|| {
        let goal_id = required_str(args, "goal_id")?;
        let status = parse_goal_status(args)?;

        store.update_goal_status(goal_id, status.clone())?;

        Ok(json!({
            "goal_id": goal_id,
            "status": status.to_string(),
            "updated": true
        }))
    })
}

pub fn handle_task_request_replan(store: &PlanStore, args: &Value) -> Result<String, PlanError> {
    respond(|| {
        let plan_id = required_str(args, "plan_id")?;
        let reason = required_str(args, "reason")?;

        store.append_event(
            plan_id,
            &PlanEvent::ReplanTriggered {
                reason: reason.to_string(),
                trigger: ReplanTrigger::AgentRequest,
            },
            "agent",
        )?;

        Ok(json!({
            "status": "replan_requested",
            "plan_id": plan_id,
            "reason": reason
        }))
    })
}

#[derive(Debug, Clone)]
struct DependencyGraph {
    tasks: BTreeSet<String>,
    deps: HashMap<String, HashSet<String>>,
}

impl DependencyGraph {
    fn depends_on(&self, task: &str, dependency: &str) -> bool {
        self.deps
            .get(task)
            .is_some_and(|dependencies| dependencies.contains(dependency))
    }
}

fn respond<F>(op: F) -> Result<String, PlanError>
where
    F: FnOnce() -> Result<Value, PlanError>,
{
    match op() {
        Ok(value) => Ok(value.to_string()),
        Err(err) => Ok(error_response(err)),
    }
}

fn required_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, PlanError> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| PlanError::ValidationFailed {
            reason: format!("missing '{key}'"),
        })
}

fn optional_string_array(args: &Value, key: &str) -> Result<Vec<String>, PlanError> {
    match args.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToString::to_string)
                    .ok_or_else(|| PlanError::ValidationFailed {
                        reason: format!("'{key}' must contain only strings"),
                    })
            })
            .collect(),
        Some(_) => Err(PlanError::ValidationFailed {
            reason: format!("'{key}' must be an array of strings"),
        }),
    }
}

fn parse_mode(args: &Value) -> Result<PlanMode, PlanError> {
    let mode = required_str(args, "mode")?;
    PlanMode::from_str(mode).map_err(|_| PlanError::ValidationFailed {
        reason: format!("invalid mode '{mode}'"),
    })
}

fn parse_removal_strategy(args: &Value) -> Result<RemovalStrategy, PlanError> {
    let strategy = required_str(args, "strategy")?;
    match strategy {
        "skip" => Ok(RemovalStrategy::Skip),
        "bridge" => Ok(RemovalStrategy::Bridge),
        "cascade" => Ok(RemovalStrategy::Cascade),
        _ => Err(PlanError::ValidationFailed {
            reason: format!("invalid strategy '{strategy}'"),
        }),
    }
}

fn parse_decision_author(args: &Value) -> Result<DecisionAuthor, PlanError> {
    match args.get("decided_by").and_then(Value::as_str) {
        Some(value) => DecisionAuthor::from_str(value).map_err(|_| PlanError::ValidationFailed {
            reason: format!("invalid decided_by '{value}'"),
        }),
        None => Ok(DecisionAuthor::Planner),
    }
}

fn parse_optional_decision_type(args: &Value) -> Result<Option<DecisionType>, PlanError> {
    match args.get("decision_type").and_then(Value::as_str) {
        Some(value) => {
            DecisionType::from_str(value)
                .map(Some)
                .map_err(|_| PlanError::ValidationFailed {
                    reason: format!("invalid decision_type '{value}'"),
                })
        }
        None => Ok(None),
    }
}

fn parse_constraint_type(args: &Value) -> Result<ConstraintType, PlanError> {
    let value = required_str(args, "constraint_type")?;
    ConstraintType::from_str(value).map_err(|_| PlanError::ValidationFailed {
        reason: format!("invalid constraint_type '{value}'"),
    })
}

fn parse_goal_priority(args: &Value) -> Result<GoalPriority, PlanError> {
    let value = required_str(args, "priority")?;
    GoalPriority::from_str(value).map_err(|_| PlanError::ValidationFailed {
        reason: format!("invalid priority '{value}'"),
    })
}

fn parse_goal_status(args: &Value) -> Result<GoalStatus, PlanError> {
    let value = required_str(args, "status")?;
    GoalStatus::from_str(value).map_err(|_| PlanError::ValidationFailed {
        reason: format!("invalid status '{value}'"),
    })
}

fn build_graph(state: &PlanState) -> DependencyGraph {
    let mut tasks = state.task_ids.iter().cloned().collect::<BTreeSet<_>>();
    let mut deps: HashMap<String, HashSet<String>> = HashMap::new();

    for task_id in &state.task_ids {
        deps.entry(task_id.clone()).or_default();
    }

    for event in &state.events {
        let parsed = serde_json::from_value::<PlanEvent>(event.payload.clone());
        let Ok(plan_event) = parsed else {
            continue;
        };

        match plan_event {
            PlanEvent::TaskAdded {
                task_id, dep_ids, ..
            } => {
                tasks.insert(task_id.clone());
                deps.entry(task_id.clone()).or_default().extend(dep_ids);
            }
            PlanEvent::TaskRemoved { task_id, .. } => {
                tasks.remove(&task_id);
                deps.remove(&task_id);
                for values in deps.values_mut() {
                    values.remove(&task_id);
                }
            }
            PlanEvent::DependencyAdded { from_task, to_task } => {
                deps.entry(from_task).or_default().insert(to_task);
            }
            PlanEvent::DependencyRemoved { from_task, to_task } => {
                if let Some(values) = deps.get_mut(&from_task) {
                    values.remove(&to_task);
                }
            }
            _ => {}
        }
    }

    let task_ids = state.task_ids.iter().cloned().collect::<BTreeSet<_>>();
    tasks = task_ids;
    deps.retain(|task_id, _| tasks.contains(task_id));

    for values in deps.values_mut() {
        values.retain(|dep_id| !dep_id.is_empty());
    }

    DependencyGraph { tasks, deps }
}

fn collect_dependents(root_task_id: &str, graph: &DependencyGraph) -> BTreeSet<String> {
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();
    for (task_id, dependencies) in &graph.deps {
        for dependency in dependencies {
            dependents
                .entry(dependency.clone())
                .or_default()
                .push(task_id.clone());
        }
    }

    let mut visited = BTreeSet::new();
    let mut queue = VecDeque::new();
    visited.insert(root_task_id.to_string());
    queue.push_back(root_task_id.to_string());

    while let Some(current) = queue.pop_front() {
        if let Some(children) = dependents.get(&current) {
            for child in children {
                if visited.insert(child.clone()) {
                    queue.push_back(child.clone());
                }
            }
        }
    }

    visited
}

pub fn validate_dag(state: &PlanState) -> Result<(), PlanError> {
    let graph = build_graph(state);

    let mut orphan_references = Vec::new();
    for (task_id, dependencies) in &graph.deps {
        if !graph.tasks.contains(task_id) {
            orphan_references.push(format!("task '{task_id}' is not in plan"));
        }

        for dependency in dependencies {
            if !graph.tasks.contains(dependency) {
                orphan_references.push(format!(
                    "task '{task_id}' references missing dependency '{dependency}'"
                ));
            }
        }
    }

    if !orphan_references.is_empty() {
        return Err(PlanError::ValidationFailed {
            reason: format!("orphan tasks detected: {}", orphan_references.join(", ")),
        });
    }

    if has_cycle(&graph) {
        return Err(PlanError::ValidationFailed {
            reason: "plan dependency graph contains a cycle".to_string(),
        });
    }

    Ok(())
}

fn has_cycle(graph: &DependencyGraph) -> bool {
    fn visit(
        node: &str,
        graph: &DependencyGraph,
        visiting: &mut HashSet<String>,
        visited: &mut HashSet<String>,
    ) -> bool {
        if visiting.contains(node) {
            return true;
        }

        if visited.contains(node) {
            return false;
        }

        visiting.insert(node.to_string());

        if let Some(dependencies) = graph.deps.get(node) {
            for dependency in dependencies {
                if !graph.tasks.contains(dependency) {
                    continue;
                }
                if visit(dependency, graph, visiting, visited) {
                    return true;
                }
            }
        }

        visiting.remove(node);
        visited.insert(node.to_string());
        false
    }

    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();

    for node in &graph.tasks {
        if visit(node, graph, &mut visiting, &mut visited) {
            return true;
        }
    }

    false
}

fn error_response(error: PlanError) -> String {
    let (error_type, message) = match error {
        PlanError::NotFound { id } => ("not_found", format!("plan or entity not found: {id}")),
        PlanError::InvalidStatus { status } => {
            ("invalid_status", format!("invalid status: {status}"))
        }
        PlanError::CycleDetected => ("cycle_detected", "dependency cycle detected".to_string()),
        PlanError::ValidationFailed { reason } => ("validation_error", reason),
        PlanError::StoreError(err) => ("store_error", err.to_string()),
        PlanError::AlreadyExists { id } => {
            ("already_exists", format!("entity already exists: {id}"))
        }
        PlanError::Serialization(err) => ("serialization_error", err.to_string()),
    };

    json!({
        "error": error_type,
        "message": message
    })
    .to_string()
}
