use sunny_core::orchestrator::events::*;

#[test]
fn test_event_chain_completeness() {
    assert!(!EVENT_PLAN_CREATED.is_empty());
    assert!(!EVENT_PLAN_COMPLETED.is_empty());
    assert!(!EVENT_PLAN_ERROR.is_empty());

    assert!(!EVENT_ROUTE_RESOLVED.is_empty());
    assert!(!EVENT_ROUTE_FAILED.is_empty());

    assert!(!EVENT_AGENT_MESSAGE_START.is_empty());
    assert!(!EVENT_AGENT_MESSAGE_END.is_empty());
    assert!(!EVENT_AGENT_MESSAGE_ERROR.is_empty());

    assert!(!EVENT_TOOL_EXEC_START.is_empty());
    assert!(!EVENT_TOOL_EXEC_END.is_empty());
    assert!(!EVENT_TOOL_EXEC_ERROR.is_empty());
    assert!(!EVENT_TOOL_EXEC_DEPTH.is_empty());
    assert!(!EVENT_TOOL_EXEC_TIMEOUT.is_empty());
    assert!(!EVENT_TOOL_CANCELLED.is_empty());
}

#[test]
fn test_event_chain_order() {
    let expected_chain = [
        "orchestrator.plan.created",
        "orchestrator.route.resolved",
        "agent.message.start",
        "agent.message.end",
        "orchestrator.plan.completed",
    ];
    let actual_chain = [
        EVENT_PLAN_CREATED,
        EVENT_ROUTE_RESOLVED,
        EVENT_AGENT_MESSAGE_START,
        EVENT_AGENT_MESSAGE_END,
        EVENT_PLAN_COMPLETED,
    ];

    assert_eq!(actual_chain, expected_chain);
}

#[test]
fn test_route_event_canonical_values() {
    assert_eq!(EVENT_ROUTE_RESOLVED, "orchestrator.route.resolved");
    assert_eq!(EVENT_ROUTE_FAILED, "orchestrator.route.failed");
}

#[test]
fn test_outcome_constants() {
    assert_eq!(OUTCOME_SUCCESS, "success");
    assert_eq!(OUTCOME_ERROR, "error");
    assert_eq!(OUTCOME_TIMEOUT, "timeout");
    assert_eq!(OUTCOME_CANCELLED, "cancelled");

    for outcome in [
        OUTCOME_SUCCESS,
        OUTCOME_ERROR,
        OUTCOME_TIMEOUT,
        OUTCOME_CANCELLED,
    ] {
        assert!(
            outcome.chars().all(|c| c.is_ascii_lowercase()),
            "Outcome '{}' must be lowercase",
            outcome
        );
        assert!(
            !outcome.contains('_'),
            "Outcome '{}' must not contain underscores",
            outcome
        );
    }
}

#[test]
fn test_event_categorization() {
    assert!(EVENT_PLAN_CREATED.starts_with("orchestrator."));
    assert!(EVENT_PLAN_COMPLETED.starts_with("orchestrator."));
    assert!(EVENT_ROUTE_RESOLVED.starts_with("orchestrator."));
    assert!(EVENT_DISPATCH_START.starts_with("orchestrator."));

    assert!(EVENT_AGENT_MESSAGE_START.starts_with("agent."));
    assert!(EVENT_AGENT_MESSAGE_END.starts_with("agent."));

    assert!(EVENT_TOOL_EXEC_START.starts_with("tool."));
    assert!(EVENT_TOOL_EXEC_END.starts_with("tool."));
}

#[test]
fn test_tool_execution_depth_tracking() {
    assert!(!EVENT_TOOL_EXEC_DEPTH.is_empty());
    assert_eq!(EVENT_TOOL_EXEC_DEPTH, "tool.exec.depth");
}

#[test]
fn test_correlation_field_conventions() {
    let correlation_fields = ["request_id", "plan_id", "agent_id", "task_id"];

    for field in &correlation_fields {
        assert!(
            !field.contains('.'),
            "Correlation field '{}' should not contain dots",
            field
        );
        assert!(
            field.contains('_'),
            "Correlation field '{}' should use underscores",
            field
        );
    }
}

#[test]
fn test_event_pairs_balanced() {
    let pairs = [
        (EVENT_AGENT_MESSAGE_START, EVENT_AGENT_MESSAGE_END),
        (EVENT_TOOL_EXEC_START, EVENT_TOOL_EXEC_END),
    ];

    for (start, end) in &pairs {
        assert!(start.ends_with(".start"));
        assert!(end.ends_with(".end"));
        let start_prefix = start.rsplit_once('.').map(|x| x.0).unwrap_or(start);
        let end_prefix = end.rsplit_once('.').map(|x| x.0).unwrap_or(end);
        assert_eq!(
            start_prefix, end_prefix,
            "Start event '{}' and end event '{}' should share prefix",
            start, end
        );
    }
}

#[test]
fn test_error_event_coverage() {
    assert!(!EVENT_TOOL_EXEC_ERROR.is_empty());
    assert!(!EVENT_TOOL_EXEC_TIMEOUT.is_empty());
    assert!(!EVENT_DISPATCH_ERROR.is_empty());
    assert!(!EVENT_PLAN_ERROR.is_empty());
    assert!(!EVENT_AGENT_MESSAGE_ERROR.is_empty());
    assert!(!EVENT_ROUTE_FAILED.is_empty());
    assert!(!EVENT_TOOL_CANCELLED.is_empty());
}

#[test]
fn test_event_organization() {
    let tool_events = [
        EVENT_TOOL_EXEC_START,
        EVENT_TOOL_EXEC_END,
        EVENT_TOOL_EXEC_ERROR,
        EVENT_TOOL_EXEC_DEPTH,
        EVENT_TOOL_EXEC_TIMEOUT,
        EVENT_TOOL_CANCELLED,
    ];

    for event in &tool_events {
        assert!(
            event.starts_with("tool.exec."),
            "Tool event '{}' should be under tool.exec namespace",
            event
        );
    }

    let agent_events = [
        EVENT_AGENT_MESSAGE_START,
        EVENT_AGENT_MESSAGE_END,
        EVENT_AGENT_MESSAGE_ERROR,
        EVENT_AGENT_MESSAGE_SENT,
        EVENT_AGENT_MESSAGE_RECEIVED,
    ];

    for event in &agent_events {
        assert!(
            event.starts_with("agent.message."),
            "Agent event '{}' should be under agent.message namespace",
            event
        );
    }
}
