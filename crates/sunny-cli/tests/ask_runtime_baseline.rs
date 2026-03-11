use serde_json::Value;
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
fn test_ask_runtime_variants_baseline_outputs_are_json_and_valid() {
    // 1) dry-run baseline: ensure plan_id, intent_kind, and request_id exist in JSON output
    let output = sunny_cli()
        .args(["ask", "test", "--dry-run", "--no-llm", "--format", "json"])
        .output()
        .expect("should run sunny ask dry-run");

    assert!(
        output.status.success(),
        "dry-run command should exit successfully"
    );
    let stdout = output.stdout;
    let v: Value = serde_json::from_slice(&stdout).expect("valid JSON output");
    assert!(v.get("plan_id").is_some(), "dry-run should include plan_id");
    assert!(
        v.get("intent_kind").is_some(),
        "dry-run should include intent_kind"
    );
    assert!(
        v.get("request_id").is_some(),
        "dry-run should include request_id"
    );

    // 2) analyze baseline: expect success and analyze intent
    let output2 = sunny_cli()
        .args(["ask", "analyze this", "--no-llm", "--format", "json"])
        .output()
        .expect("should run sunny ask analyze");
    assert!(output2.status.success(), "analyze should exit successfully");
    let v2: Value = serde_json::from_slice(&output2.stdout).expect("valid JSON output for analyze");
    assert_eq!(v2.get("outcome").and_then(|s| s.as_str()), Some("success"));
    assert_eq!(
        v2.get("intent_kind").and_then(|s| s.as_str()),
        Some("analyze")
    );

    // 3) query baseline: expect success and query intent
    let output3 = sunny_cli()
        .args(["ask", "inspect code", "--no-llm", "--format", "json"])
        .output()
        .expect("should run sunny ask query");
    assert!(output3.status.success(), "query should exit successfully");
    let v3: Value = serde_json::from_slice(&output3.stdout).expect("valid JSON output for query");
    assert_eq!(v3.get("outcome").and_then(|s| s.as_str()), Some("success"));
    assert_eq!(
        v3.get("intent_kind").and_then(|s| s.as_str()),
        Some("query")
    );

    // 4) action baseline: expect success and action intent
    let output4 = sunny_cli()
        .args(["ask", "create plan", "--no-llm", "--format", "json"])
        .output()
        .expect("should run sunny ask action");
    assert!(output4.status.success(), "action should exit successfully");
    let v4: Value = serde_json::from_slice(&output4.stdout).expect("valid JSON output for action");
    assert_eq!(v4.get("outcome").and_then(|s| s.as_str()), Some("success"));
    assert_eq!(
        v4.get("intent_kind").and_then(|s| s.as_str()),
        Some("action")
    );
}
