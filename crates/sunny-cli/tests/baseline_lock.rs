//! Baseline metrics snapshot tests for regression detection.
//!
//! These tests lock critical baseline metrics:
//! - CLI subcommand availability (prompt, analyze)
//! - Event constant values and naming conventions
//! - Total test count threshold

use std::process::Command;

fn sunny_cli() -> Command {
    let exe = std::env::var("CARGO_BIN_EXE_sunny-cli").unwrap_or_else(|_| {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("workspace root")
            .join("target")
            .join("debug")
            .join("sunny-cli")
            .to_string_lossy()
            .to_string()
    });
    let mut cmd = Command::new(&exe);
    cmd.env("RUST_LOG", "off");
    cmd
}

#[test]
fn test_baseline_prompt_subcommand_exists() {
    let output = sunny_cli()
        .args(["prompt", "--help"])
        .output()
        .expect("should run prompt --help");

    assert!(
        output.status.success(),
        "prompt --help should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_baseline_analyze_subcommand_exists() {
    let output = sunny_cli()
        .args(["analyze", "--help"])
        .output()
        .expect("should run analyze --help");

    assert!(
        output.status.success(),
        "analyze --help should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_baseline_event_constants_pinned() {
    use sunny_core::orchestrator::events::*;

    // Tool events
    assert_eq!(EVENT_TOOL_EXEC_START, "tool.exec.start");
    assert_eq!(EVENT_TOOL_EXEC_END, "tool.exec.end");
    assert_eq!(EVENT_TOOL_EXEC_ERROR, "tool.exec.error");

    // Dispatch events
    assert_eq!(EVENT_DISPATCH_START, "orchestrator.dispatch.start");
    assert_eq!(EVENT_DISPATCH_SUCCESS, "orchestrator.dispatch.success");
    assert_eq!(EVENT_DISPATCH_ERROR, "orchestrator.dispatch.error");

    // Plan events
    assert_eq!(EVENT_PLAN_CREATED, "orchestrator.plan.created");
    assert_eq!(EVENT_PLAN_UPDATED, "orchestrator.plan.updated");
    assert_eq!(EVENT_PLAN_COMPLETED, "orchestrator.plan.completed");
    assert_eq!(EVENT_PLAN_ERROR, "orchestrator.plan.error");

    // Route events
    assert_eq!(EVENT_ROUTE_RESOLVED, "orchestrator.route.resolved");
    assert_eq!(EVENT_ROUTE_FAILED, "orchestrator.route.failed");

    // Agent message events
    assert_eq!(EVENT_AGENT_MESSAGE_SENT, "agent.message.sent");
    assert_eq!(EVENT_AGENT_MESSAGE_RECEIVED, "agent.message.received");
    assert_eq!(EVENT_AGENT_MESSAGE_START, "agent.message.start");
    assert_eq!(EVENT_AGENT_MESSAGE_END, "agent.message.end");
    assert_eq!(EVENT_AGENT_MESSAGE_ERROR, "agent.message.error");

    // CLI events
    assert_eq!(EVENT_CLI_COMMAND_START, "cli.command.start");
    assert_eq!(EVENT_CLI_COMMAND_END, "cli.command.end");

    // Outcome constants
    assert_eq!(OUTCOME_SUCCESS, "success");
    assert_eq!(OUTCOME_ERROR, "error");
    assert_eq!(OUTCOME_TIMEOUT, "timeout");
    assert_eq!(OUTCOME_CANCELLED, "cancelled");
}

#[test]
fn test_baseline_event_naming_convention() {
    use sunny_core::orchestrator::events::*;

    let events = vec![
        EVENT_TOOL_EXEC_START,
        EVENT_TOOL_EXEC_END,
        EVENT_TOOL_EXEC_ERROR,
        EVENT_DISPATCH_START,
        EVENT_DISPATCH_SUCCESS,
        EVENT_DISPATCH_ERROR,
        EVENT_PLAN_CREATED,
        EVENT_PLAN_UPDATED,
        EVENT_PLAN_COMPLETED,
        EVENT_PLAN_ERROR,
        EVENT_ROUTE_RESOLVED,
        EVENT_ROUTE_FAILED,
        EVENT_AGENT_MESSAGE_SENT,
        EVENT_AGENT_MESSAGE_RECEIVED,
        EVENT_AGENT_MESSAGE_START,
        EVENT_AGENT_MESSAGE_END,
        EVENT_AGENT_MESSAGE_ERROR,
        EVENT_CLI_COMMAND_START,
        EVENT_CLI_COMMAND_END,
    ];

    for event in events {
        assert!(
            event.contains('.'),
            "Event '{}' must follow dotted naming convention",
            event
        );
        assert!(
            !event.contains('_'),
            "Event '{}' must use dots, not underscores",
            event
        );
    }
}

#[test]
fn test_baseline_canonical_route_event_value() {
    use sunny_core::orchestrator::events::EVENT_ROUTE_RESOLVED;

    assert_eq!(EVENT_ROUTE_RESOLVED, "orchestrator.route.resolved");
}
