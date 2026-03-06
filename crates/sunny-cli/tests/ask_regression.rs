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

    // Empty input should produce an error, not panic
    // The exact exit code may vary, but it should not segfault/panic
    let stderr = String::from_utf8_lossy(&output.stderr);
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

/// Verify prompt command still exists and works (removed only in T28)
#[test]
fn test_prompt_still_exists() {
    let output = sunny_cli()
        .args(["prompt", "--help"])
        .output()
        .expect("should run prompt --help");

    assert!(
        output.status.success(),
        "prompt command should still exist and work"
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.to_lowercase().contains("usage:"),
        "prompt help should show usage information"
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
            .expect(&format!("should run ask with {} format", format));

        assert!(
            output.status.success(),
            "format={} should succeed, stderr: {}",
            format,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

/// Verify ask with real files produces valid output
#[test]
fn test_ask_with_real_files() {
    use std::fs;

    let temp_dir = std::env::temp_dir().join(format!(
        "sunny_regression_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp dir");

    // Create a sample Rust file
    fs::write(
        temp_dir.join("main.rs"),
        "fn main() { println!(\"Hello\"); }",
    )
    .expect("write file");

    let output = sunny_cli()
        .args([
            "ask",
            &format!("analyze {}", temp_dir.display()),
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

    // Should produce valid JSON
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("output should be valid JSON");

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
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
    assert!(
        json["intent_kind"].is_string(),
        "should have intent_kind for routing"
    );
}
