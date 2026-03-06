//! Regression tests for the ask command
//!
//! These tests verify that existing behavior doesn't break as the codebase evolves.
//! They are CHANGE DETECTORS - intentional breaking changes require test updates.

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

/// Verify JSON output schema hasn't changed unexpectedly
#[test]
fn test_ask_dry_run_json_schema() {
    let output = sunny_cli()
        .args([
            "ask",
            "test query",
            "--dry-run",
            "--no-llm",
            "--format",
            "json",
        ])
        .output()
        .expect("should run ask command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "command should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Parse JSON to verify structure
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("output should be valid JSON");

    // Required fields
    assert!(
        json.get("plan_id").is_some(),
        "JSON should have plan_id field"
    );
    assert!(
        json.get("dry_run").is_some(),
        "JSON should have dry_run field"
    );
    assert!(
        json.get("intent_kind").is_some(),
        "JSON should have intent_kind field"
    );
    assert!(
        json.get("request_id").is_some(),
        "JSON should have request_id field"
    );

    // Verify field types
    assert!(json["plan_id"].is_string(), "plan_id should be a string");
    assert!(json["dry_run"].is_boolean(), "dry_run should be a boolean");
    assert!(
        json["intent_kind"].is_string(),
        "intent_kind should be a string"
    );
}

/// Verify empty input produces graceful error (not panic)
#[test]
fn test_ask_empty_input_error() {
    let output = sunny_cli()
        .args(["ask", "", "--no-llm", "--format", "json"])
        .output()
        .expect("should run ask command");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !stderr.contains("panic"),
        "should not panic on empty input, stderr: {}",
        stderr
    );
    assert!(
        !stderr.contains("thread '"),
        "should not have thread panic, stderr: {}",
        stderr
    );
    assert!(
        !output.status.success() || stdout.contains("\"error\"") || stdout.contains("\"outcome\""),
        "empty input should fail or return a structured error envelope, stdout: {stdout}, stderr: {stderr}"
    );
}

/// Verify text format produces human-readable output
#[test]
fn test_ask_format_text_output() {
    let output = sunny_cli()
        .args([
            "ask",
            "hello world",
            "--dry-run",
            "--no-llm",
            "--format",
            "text",
        ])
        .output()
        .expect("should run ask command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "command should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Text format should have human-readable output with Request ID
    assert!(
        stdout.contains("Request ID:"),
        "text format should contain Request ID, got: {}",
        stdout
    );
}

/// Verify pretty format produces structured/colored output
#[test]
fn test_ask_format_pretty_output() {
    let output = sunny_cli()
        .args(["ask", "test", "--dry-run", "--no-llm", "--format", "pretty"])
        .output()
        .expect("should run ask command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "command should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Pretty format should have structured output (indented, with field labels)
    assert!(
        stdout.contains("Plan ID:"),
        "pretty format should contain 'Plan ID:', got: {}",
        stdout
    );
}

/// Verify analyze command is unchanged
#[test]
fn test_analyze_unchanged() {
    let output = sunny_cli()
        .args(["analyze", "--help"])
        .output()
        .expect("should run analyze --help");

    assert!(output.status.success(), "analyze command should still work");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.to_lowercase().contains("usage:"),
        "analyze help should show usage information"
    );
}

/// Verify all three output formats work for the same query
#[test]
fn test_all_formats_work() {
    for format in ["json", "text", "pretty"] {
        let output = sunny_cli()
            .args([
                "ask",
                "test query",
                "--dry-run",
                "--no-llm",
                "--format",
                format,
            ])
            .output()
            .unwrap_or_else(|_| panic!("should run ask with {} format", format));

        assert!(
            output.status.success(),
            "format={} should succeed, stderr: {}",
            format,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn test_ask_dry_run_path_input_returns_json_envelope() {
    let output = sunny_cli()
        .args([
            "ask",
            "analyze /tmp/example-project",
            "--dry-run",
            "--no-llm",
            "--format",
            "json",
        ])
        .output()
        .expect("should run ask command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "ask with real files should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Should produce valid JSON (verified by successful deserialization)
    let json: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|_| panic!("output should be valid JSON"));
    assert_eq!(json["intent_kind"].as_str(), Some("analyze"));
    assert_eq!(json["required_capability"].as_str(), Some("analyze"));
    assert_eq!(json["outcome"].as_str(), Some("planned"));
}

/// Verify routing still works for different intent types
#[test]
fn test_routing_regression() {
    // Query-type input should route correctly
    let output = sunny_cli()
        .args([
            "ask",
            "what does this codebase do?",
            "--dry-run",
            "--no-llm",
            "--format",
            "json",
        ])
        .output()
        .expect("should run ask command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "routing should work, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_str(&stdout).expect("valid JSON");
    assert_eq!(json["intent_kind"].as_str(), Some("query"));
    assert_eq!(json["required_capability"].as_str(), Some("query"));
}
