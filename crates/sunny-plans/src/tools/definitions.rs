use serde_json;
use sunny_mind::ToolDefinition;

pub fn build_plan_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "plan_create".to_string(),
            description: "Create a new execution plan. Call this first to establish the plan structure before adding tasks, decisions, constraints, or goals.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Human-readable plan name that explains the work the plan will coordinate"
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["quick", "smart"],
                        "description": "Planning mode: use 'quick' for reactive execution or 'smart' for upfront DAG planning"
                    },
                    "description": {
                        "type": "string",
                        "description": "Optional extra context describing scope, intent, or success criteria for the plan"
                    }
                },
                "required": ["name", "mode"]
            }),
        },
        ToolDefinition {
            name: "plan_add_task".to_string(),
            description: "Add a task node to the plan DAG. Use this after plan creation to describe executable work items and optionally declare upstream dependencies.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "ID of the plan that should receive the new task"
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Stable task identifier to use for this DAG node"
                    },
                    "title": {
                        "type": "string",
                        "description": "Short task title describing the work this node performs"
                    },
                    "dep_ids": {
                        "type": "array",
                        "items": {
                            "type": "string"
                        },
                        "description": "Optional list of prerequisite task IDs that must complete before this task can start"
                    }
                },
                "required": ["plan_id", "task_id", "title"]
            }),
        },
        ToolDefinition {
            name: "plan_add_dependency".to_string(),
            description: "Add a dependency edge between two existing tasks in the same plan. Use this when the tasks already exist and you need to enforce execution order.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "ID of the plan whose DAG should be updated"
                    },
                    "from_task": {
                        "type": "string",
                        "description": "Task ID that must complete first"
                    },
                    "to_task": {
                        "type": "string",
                        "description": "Task ID that depends on from_task and starts after it finishes"
                    }
                },
                "required": ["plan_id", "from_task", "to_task"]
            }),
        },
        ToolDefinition {
            name: "plan_remove_task".to_string(),
            description: "Remove a task from the plan and choose how dependents should be handled. Use skip to cancel only this task, bridge to reconnect its neighbors, or cascade to remove dependents too.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "ID of the plan that contains the task to remove"
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Task ID to remove from the plan DAG"
                    },
                    "strategy": {
                        "type": "string",
                        "enum": ["skip", "bridge", "cascade"],
                        "description": "Removal strategy: skip cancels only this task, bridge reconnects dependencies, cascade removes downstream dependents"
                    }
                },
                "required": ["plan_id", "task_id", "strategy"]
            }),
        },
        ToolDefinition {
            name: "plan_query_state".to_string(),
            description: "Get the full current state of a plan. Use this when you need authoritative plan data before replanning, reporting progress, or making further mutations.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "ID of the plan to inspect"
                    }
                },
                "required": ["plan_id"]
            }),
        },
        ToolDefinition {
            name: "plan_finalize".to_string(),
            description: "Finalize a draft plan after all tasks and dependencies are in place. This validates the DAG and transitions the plan from Draft to Ready.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "ID of the draft plan to validate and finalize"
                    }
                },
                "required": ["plan_id"]
            }),
        },
        ToolDefinition {
            name: "plan_replan".to_string(),
            description: "Modify a plan during execution when assumptions change. Describe the requested DAG changes in the changes field and optionally list tasks that should be canceled first.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "ID of the active or draft plan to modify"
                    },
                    "changes": {
                        "type": "string",
                        "description": "JSON-formatted description of the tasks, dependencies, or structure that should change"
                    },
                    "tasks_to_cancel": {
                        "type": "array",
                        "items": {
                            "type": "string"
                        },
                        "description": "Optional list of existing task IDs that should be canceled as part of replanning"
                    }
                },
                "required": ["plan_id", "changes"]
            }),
        },
        ToolDefinition {
            name: "plan_record_decision".to_string(),
            description: "Record an architectural, scope, or requirement decision so later execution and replanning can rely on it. Use this whenever the user or agent makes a planning-relevant call.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "ID of the plan that this decision belongs to"
                    },
                    "decision": {
                        "type": "string",
                        "description": "The decision that was made, written as a concise statement"
                    },
                    "rationale": {
                        "type": "string",
                        "description": "Optional explanation for why this decision was chosen"
                    },
                    "alternatives_considered": {
                        "type": "string",
                        "description": "Optional summary of the main alternatives that were evaluated"
                    },
                    "decided_by": {
                        "type": "string",
                        "enum": ["user", "planner", "agent"],
                        "description": "Who made the decision"
                    },
                    "decision_type": {
                        "type": "string",
                        "enum": ["technology", "scope", "constraint", "tradeoff", "requirement"],
                        "description": "Optional category for the decision; use 'tradeoff' to match the current plan model enum"
                    },
                    "is_locked": {
                        "type": "boolean",
                        "description": "Whether this decision is locked and should strongly constrain future replanning"
                    }
                },
                "required": ["plan_id", "decision"]
            }),
        },
        ToolDefinition {
            name: "plan_add_constraint".to_string(),
            description: "Add an execution constraint to the plan. Use this for must-do, must-not-do, preferences, or avoid directives that should guide task generation and replanning.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "ID of the plan that the constraint should apply to"
                    },
                    "constraint_type": {
                        "type": "string",
                        "enum": ["must_do", "must_not_do", "prefer", "avoid"],
                        "description": "Constraint strength or direction"
                    },
                    "description": {
                        "type": "string",
                        "description": "The actual constraint instruction to follow during execution"
                    },
                    "source_decision_id": {
                        "type": "string",
                        "description": "Optional decision ID that introduced or justifies this constraint"
                    }
                },
                "required": ["plan_id", "constraint_type", "description"]
            }),
        },
        ToolDefinition {
            name: "plan_add_goal".to_string(),
            description: "Add a goal the plan should achieve. Use goals to capture desired outcomes that help evaluate progress and replanning tradeoffs.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "ID of the plan that this goal belongs to"
                    },
                    "description": {
                        "type": "string",
                        "description": "Goal statement describing the desired outcome"
                    },
                    "priority": {
                        "type": "string",
                        "enum": ["critical", "important", "nice_to_have"],
                        "description": "Importance of the goal relative to other outcomes"
                    }
                },
                "required": ["plan_id", "description", "priority"]
            }),
        },
        ToolDefinition {
            name: "plan_update_goal".to_string(),
            description: "Update a goal after execution changes its outcome. Use this to mark a goal as achieved or abandoned when reality changes.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "goal_id": {
                        "type": "string",
                        "description": "ID of the goal whose status should change"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["pending", "achieved", "abandoned"],
                        "description": "New status for the goal"
                    }
                },
                "required": ["goal_id", "status"]
            }),
        },
    ]
}
