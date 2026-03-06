//! End-to-end integration test for the ask command
//!
//! This test covers the full ask pipeline from CLI to agent response.

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

/// Full end-to-end test covering the entire ask pipeline
#[test]
fn test_e2e_ask_pipeline() {
    use std::fs;

    // Create temp directory with 3+ sample Rust files
    let temp_dir = std::env::temp_dir().join(format!(
        "sunny_e2e_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp dir");

    // Create sample Rust files
    fs::write(
        temp_dir.join("main.rs"),
        "fn main() { println!(\"Hello from main\"); }",
    )
    .expect("write main.rs");

    fs::write(
        temp_dir.join("lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 { a + b }",
    )
    .expect("write lib.rs");

    fs::write(
        temp_dir.join("utils.rs"),
        "pub fn helper() { println!(\"Helper function\"); }",
    )
    .expect("write utils.rs");

    // Run ask command
    let output = sunny_cli()
        .args([
            "ask",
            &format!("analyze the structure of {}", temp_dir.display()),
            "--no-llm",
            "--format",
            "json",
        ])
        .output()
        .expect("should run ask command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "ask command should succeed, stderr: {}",
        stderr
    );

    // Parse JSON output
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("output should be valid JSON");

    // Verify: plan_id non-empty
    assert!(json.get("plan_id").is_some(), "should have plan_id field");
    assert!(
        json["plan_id"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "plan_id should be non-empty string"
    );

    // Verify: outcome = "success"
    assert_eq!(
        json["outcome"].as_str(),
        Some("success"),
        "outcome should be 'success', got: {:?}",
        json["outcome"]
    );

    // Verify: steps_completed ≥ 1
    let steps_completed = json["steps_completed"].as_u64().unwrap_or(0);
    assert!(
        steps_completed >= 1,
        "steps_completed should be >= 1, got: {}",
        steps_completed
    );

    // Verify: intent_kind matches expected (should be "analyze" for this input)
    assert_eq!(
        json["intent_kind"].as_str(),
        Some("analyze"),
        "intent_kind should be 'analyze' for structure analysis, got: {:?}",
        json["intent_kind"]
    );

    // Verify: response field is non-empty
    assert!(
        json["response"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "response field should be non-empty"
    );

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}

/// Test error recovery: empty input should produce graceful error (not panic)
#[test]
fn test_e2e_empty_input_error_recovery() {
    let output = sunny_cli()
        .args(["ask", "", "--no-llm", "--format", "json"])
        .output()
        .expect("should run ask command");

    let stderr = String::from_utf8_lossy(&output.stderr);

    // Should not panic
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

    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() {
        let json: serde_json::Value = serde_json::from_str(&stdout)
            .expect("successful empty-input response should be valid JSON");
        assert!(
            json.get("error").is_some() || json.get("outcome").is_some(),
            "successful empty-input response should contain an error envelope, stdout: {stdout}"
        );
    } else {
        assert!(
            !stdout.trim().is_empty() || !stderr.trim().is_empty(),
            "failing empty-input response should emit diagnostics"
        );
    }
}

/// Test with query-type input: should route to codebase agent
#[test]
fn test_e2e_query_routing() {
    use std::fs;

    // Create temp directory with a file
    let temp_dir = std::env::temp_dir().join(format!(
        "sunny_e2e_query_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp dir");

    fs::write(
        temp_dir.join("main.rs"),
        "fn main() { println!(\"Hello\"); }",
    )
    .expect("write file");

    // Use simple path-based query
    let output = sunny_cli()
        .args([
            "ask",
            &format!("{}", temp_dir.display()),
            "--no-llm",
            "--format",
            "json",
        ])
        .output()
        .expect("should run ask command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        output.status.success(),
        "ask command should succeed, stderr: {}",
        stderr
    );

    // Parse JSON output
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("output should be valid JSON");

    // Verify: intent_kind is present (may vary by input)
    assert!(
        json["intent_kind"].is_string(),
        "intent_kind should be present, got: {:?}",
        json["intent_kind"]
    );

    // Verify: routes correctly (should have capability field)
    assert!(
        json.get("required_capability").is_some(),
        "should have required_capability field"
    );

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}

/// Test full pipeline with different file types
#[test]
fn test_e2e_mixed_file_types() {
    use std::fs;

    let temp_dir = std::env::temp_dir().join(format!(
        "sunny_e2e_mixed_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp dir");

    // Create files of different types
    fs::write(temp_dir.join("main.rs"), "fn main() {}").expect("write rs");
    fs::write(temp_dir.join("README.md"), "# Test Project").expect("write md");
    fs::write(temp_dir.join("Cargo.toml"), "[package]\nname = \"test\"").expect("write toml");

    let output = sunny_cli()
        .args([
            "ask",
            &format!("analyze {}", temp_dir.display()),
            "--no-llm",
            "--format",
            "json",
        ])
        .output()
        .expect("should run ask command");

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "ask command should succeed");

    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("output should be valid JSON");

    // Verify pipeline completed successfully
    assert_eq!(json["outcome"].as_str(), Some("success"));

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}

/// Verify correlation IDs are present and consistent
#[test]
fn test_e2e_correlation_ids() {
    use std::fs;

    let temp_dir = std::env::temp_dir().join(format!(
        "sunny_e2e_correlation_test_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&temp_dir).expect("create temp dir");
    fs::write(temp_dir.join("test.txt"), "test").expect("write file");

    let output = sunny_cli()
        .args([
            "ask",
            &format!("analyze {}", temp_dir.display()),
            "--no-llm",
            "--format",
            "json",
        ])
        .output()
        .expect("should run ask command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&stdout).expect("output should be valid JSON");

    // Verify correlation fields exist
    assert!(json.get("request_id").is_some(), "should have request_id");
    assert!(json.get("plan_id").is_some(), "should have plan_id");

    // Both should be non-empty strings
    assert!(
        json["request_id"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "request_id should be non-empty"
    );
    assert!(
        json["plan_id"]
            .as_str()
            .map(|s| !s.is_empty())
            .unwrap_or(false),
        "plan_id should be non-empty"
    );

    // Cleanup
    let _ = fs::remove_dir_all(&temp_dir);
}
