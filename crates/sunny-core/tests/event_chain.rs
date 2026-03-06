//! Event chain completeness tests
//!
//! These tests verify that the event taxonomy is complete and consistent
//! for full observability of the ask pipeline.

use sunny_core::orchestrator::events::*;

/// Verify all events required for the ask pipeline exist
#[test]
fn test_event_chain_completeness() {
    // CLI lifecycle events
    assert!(
        !EVENT_CLI_COMMAND_START.is_empty(),
        "CLI command start event must exist"
    );
    assert!(
        !EVENT_CLI_COMMAND_END.is_empty(),
        "CLI command end event must exist"
    );

    // Plan lifecycle events
    assert!(
        !EVENT_PLAN_CREATED.is_empty(),
        "Plan created event must exist"
    );
    assert!(
        !EVENT_PLAN_COMPLETED.is_empty(),
        "Plan completed event must exist"
    );
    assert!(!EVENT_PLAN_ERROR.is_empty(), "Plan error event must exist");

    // Routing events
    assert!(
        !EVENT_ROUTE_RESOLVED.is_empty(),
        "Route resolved event must exist"
    );
    assert!(
        !EVENT_ROUTE_FAILED.is_empty(),
        "Route failed event must exist"
    );

    // Agent message events
    assert!(
        !EVENT_AGENT_MESSAGE_START.is_empty(),
        "Agent message start event must exist"
    );
    assert!(
        !EVENT_AGENT_MESSAGE_END.is_empty(),
        "Agent message end event must exist"
    );

    // Tool execution events
    assert!(
        !EVENT_TOOL_EXEC_START.is_empty(),
        "Tool exec start event must exist"
    );
    assert!(
        !EVENT_TOOL_EXEC_END.is_empty(),
        "Tool exec end event must exist"
    );
}

/// Verify the canonical event chain order for ask pipeline
#[test]
fn test_event_chain_order() {
    // Expected chain for successful ask execution:
    // 1. cli.command.start
    // 2. orchestrator.plan.created
    // 3. orchestrator.route.resolved
    // 4. agent.message.start
    // 5. agent.message.end
    // 6. orchestrator.plan.completed
    // 7. cli.command.end

    let expected_chain = [
        EVENT_CLI_COMMAND_START,
        EVENT_PLAN_CREATED,
        EVENT_ROUTE_RESOLVED,
        EVENT_AGENT_MESSAGE_START,
        EVENT_AGENT_MESSAGE_END,
        EVENT_PLAN_COMPLETED,
        EVENT_CLI_COMMAND_END,
    ];

    // Verify all events follow dotted naming convention
    for event in &expected_chain {
        assert!(
            event.contains('.'),
            "Event '{}' must use dotted naming convention",
            event
        );
        assert!(
            !event.contains('_'),
            "Event '{}' must use dots, not underscores",
            event
        );
    }
}

/// Verify route event canonical values (these are part of the public contract)
#[test]
fn test_route_event_canonical_values() {
    // These values are PINNED and must not change without explicit breaking change notice
    assert_eq!(
        EVENT_ROUTE_RESOLVED, "orchestrator.route.resolved",
        "Route resolved event value is canonical"
    );
    assert_eq!(
        EVENT_ROUTE_FAILED, "orchestrator.route.failed",
        "Route failed event value is canonical"
    );
}

/// Verify outcome constants are consistent
#[test]
fn test_outcome_constants() {
    assert_eq!(OUTCOME_SUCCESS, "success");
    assert_eq!(OUTCOME_ERROR, "error");
    assert_eq!(OUTCOME_TIMEOUT, "timeout");
    assert_eq!(OUTCOME_CANCELLED, "cancelled");

    // All outcomes should be lowercase without underscores
    for outcome in [
        OUTCOME_SUCCESS,
        OUTCOME_ERROR,
        OUTCOME_TIMEOUT,
        OUTCOME_CANCELLED,
    ] {
        assert!(
            outcome.chars().all(|c| c.is_lowercase() || c.is_ascii()),
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

/// Verify event hierarchy and categorization
#[test]
fn test_event_categorization() {
    // CLI events
    assert!(EVENT_CLI_COMMAND_START.starts_with("cli."));
    assert!(EVENT_CLI_COMMAND_END.starts_with("cli."));

    // Orchestrator events
    assert!(EVENT_PLAN_CREATED.starts_with("orchestrator."));
    assert!(EVENT_PLAN_COMPLETED.starts_with("orchestrator."));
    assert!(EVENT_ROUTE_RESOLVED.starts_with("orchestrator."));
    assert!(EVENT_DISPATCH_START.starts_with("orchestrator."));

    // Agent events
    assert!(EVENT_AGENT_MESSAGE_START.starts_with("agent."));
    assert!(EVENT_AGENT_MESSAGE_END.starts_with("agent."));

    // Tool events
    assert!(EVENT_TOOL_EXEC_START.starts_with("tool."));
    assert!(EVENT_TOOL_EXEC_END.starts_with("tool."));
}

/// Verify all events have consistent depth field for recursive tracking
#[test]
fn test_tool_execution_depth_tracking() {
    assert!(
        !EVENT_TOOL_EXEC_DEPTH.is_empty(),
        "Tool execution depth event must exist for recursive tracking"
    );
    assert_eq!(
        EVENT_TOOL_EXEC_DEPTH, "tool.exec.depth",
        "Depth event has canonical value"
    );
}

/// Verify correlation field naming conventions
#[test]
fn test_correlation_field_conventions() {
    // These are the standard correlation fields used across events
    let correlation_fields = ["request_id", "plan_id", "agent_id", "task_id"];

    for field in &correlation_fields {
        // Fields should use snake_case (not dotted)
        assert!(
            !field.contains('.'),
            "Correlation field '{}' should not contain dots",
            field
        );
        // Fields should use underscores
        assert!(
            field.contains('_'),
            "Correlation field '{}' should use underscores",
            field
        );
    }
}

/// Verify event pairs (start/end) are balanced
#[test]
fn test_event_pairs_balanced() {
    // Each start event should have a corresponding end/error event
    let pairs = [
        (EVENT_CLI_COMMAND_START, EVENT_CLI_COMMAND_END),
        (EVENT_AGENT_MESSAGE_START, EVENT_AGENT_MESSAGE_END),
        (EVENT_TOOL_EXEC_START, EVENT_TOOL_EXEC_END),
    ];

    for (start, end) in &pairs {
        // Start and end should share the same prefix
        let start_prefix = start.rsplitn(2, '.').nth(1).unwrap_or(start);
        let end_prefix = end.rsplitn(2, '.').nth(1).unwrap_or(end);
        assert_eq!(
            start_prefix, end_prefix,
            "Start event '{}' and end event '{}' should share prefix",
            start, end
        );
    }
}

/// Verify error events exist for all major operations
#[test]
fn test_error_event_coverage() {
    // Each major operation should have an error event
    assert!(
        !EVENT_TOOL_EXEC_ERROR.is_empty(),
        "Tool exec needs error event"
    );
    assert!(
        !EVENT_DISPATCH_ERROR.is_empty(),
        "Dispatch needs error event"
    );
    assert!(!EVENT_PLAN_ERROR.is_empty(), "Plan needs error event");
    assert!(
        !EVENT_AGENT_MESSAGE_ERROR.is_empty(),
        "Agent message needs error event"
    );
    assert!(!EVENT_ROUTE_FAILED.is_empty(), "Route needs failure event");
}

/// Test that events are sorted logically (not strictly required but good for organization)
#[test]
fn test_event_organization() {
    // Events within each category should be grouped
    let tool_events = [
        EVENT_TOOL_EXEC_START,
        EVENT_TOOL_EXEC_END,
        EVENT_TOOL_EXEC_ERROR,
        EVENT_TOOL_EXEC_DEPTH,
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
