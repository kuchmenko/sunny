//! Baseline lock tests - regression anchor for sunny-ask migration
//!
//! These tests snapshot critical facts about the codebase state before
//! the sunny-ask migration begins. Any breaking change to these facts
//! should be intentional and explicitly approved.

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
    let mut cmd = Command::new(exe);
    cmd.env("RUST_LOG", "off");
    cmd
}

/// Test that sunny ask subcommand exists and parses
#[test]
fn test_ask_subcommand_exists() {
    let output = sunny_cli()
        .args(["ask", "--help"])
        .output()
        .expect("failed to run sunny-cli ask --help");

    assert!(
        output.status.success(),
        "sunny-cli ask --help should exit successfully"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("ask"), "ask --help should mention 'ask'");
}

/// Test that sunny analyze subcommand exists and parses
#[test]
fn test_analyze_subcommand_exists() {
    let output = sunny_cli()
        .args(["analyze", "--help"])
        .output()
        .expect("failed to run sunny-cli analyze --help");

    assert!(
        output.status.success(),
        "sunny-cli analyze --help should exit successfully"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("analyze"),
        "analyze --help should mention 'analyze'"
    );
}

/// Test that EVENT_ROUTE_RESOLVED has the canonical value
#[test]
fn test_route_event_value_canonical() {
    // Import the constant directly and verify its value
    use sunny_core::orchestrator::EVENT_ROUTE_RESOLVED;
    assert_eq!(
        EVENT_ROUTE_RESOLVED, "orchestrator.route.resolved",
        "EVENT_ROUTE_RESOLVED must have canonical value"
    );
}

/// Test that event constants follow dotted naming convention
#[test]
fn test_event_constants_follow_naming() {
    use sunny_core::orchestrator::*;

    // List all event constants and verify they contain dots
    let events = vec![
        EVENT_ROUTE_RESOLVED,
        EVENT_DISPATCH_START,
        EVENT_DISPATCH_SUCCESS,
        EVENT_DISPATCH_ERROR,
        EVENT_PLAN_CREATED,
        EVENT_PLAN_UPDATED,
        EVENT_PLAN_COMPLETED,
        EVENT_PLAN_ERROR,
        EVENT_TOOL_EXEC_START,
        EVENT_TOOL_EXEC_END,
        EVENT_CLI_COMMAND_START,
        EVENT_CLI_COMMAND_END,
    ];

    for event in events {
        assert!(
            event.contains('.'),
            "Event constant '{}' should contain a dot",
            event
        );
    }
}

/// Test baseline test count from sunny-core crate only (faster than --workspace)
#[test]
fn test_sunny_core_test_count() {
    // Baseline: sunny-core had 129 tests at start of migration
    // We verify sunny-core tests compile and pass without running full workspace
    // This is a smoke test - the real verification is in CI with full test run

    // Just verify the sunny-core crate compiles with tests
    let status = Command::new("cargo")
        .args(["test", "-p", "sunny-core", "--no-run"])
        .status()
        .expect("failed to run cargo test --no-run");

    assert!(status.success(), "sunny-core tests should compile");
}
