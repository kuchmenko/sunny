use std::process::Command;
use std::process::Output;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

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

fn run_with_timeout(cmd: &mut Command) -> Output {
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("should spawn ask command");
    let deadline = Instant::now() + Duration::from_secs(20);

    loop {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child
                .wait_with_output()
                .expect("timed out command should still produce output");
            panic!(
                "command timed out after 20s, stdout: {}, stderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        match child.try_wait().expect("should poll child process") {
            Some(_) => {
                return child
                    .wait_with_output()
                    .expect("finished command should produce output");
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    }
}

fn run_ask_json(args: &[&str]) -> (Output, Value, String) {
    let mut cmd = sunny_cli();
    cmd.args(args);
    let output = run_with_timeout(&mut cmd);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("expected valid JSON output, error: {e}, stdout: {stdout}"));
    (output, parsed, stdout)
}

#[test]
fn test_ask_full_pipeline_with_intake_analyze() {
    let (_output, json, _) = run_ask_json(&["ask", "review code", "--no-llm", "--format", "json"]);

    // analyze intent with --no-llm hard-fails at ReviewAgent (no provider configured)
    // Do not assert exit success; JSON is still emitted to stdout
    assert_eq!(json["intent_kind"], "analyze");
    assert_eq!(json["required_capability"], "analyze");
    assert_eq!(json["outcome"], "error");
    // intake.verdict not present in error response (pipeline fails at agent dispatch)
}

#[test]
fn test_ask_full_pipeline_with_intake_query() {
    let (output, json, _) =
        run_ask_json(&["ask", "inspect codebase", "--no-llm", "--format", "json"]);

    assert!(
        output.status.success(),
        "command should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(json["intent_kind"], "query");
    assert_eq!(json["required_capability"], "query");
    assert_eq!(json["outcome"], "success");
    assert_eq!(json["metadata"]["_sunny.intake.verdict"], "proceed");
}

#[test]
fn test_ask_full_pipeline_with_intake_action() {
    let (_output, json, _) =
        run_ask_json(&["ask", "create deployment", "--no-llm", "--format", "json"]);

    // action intent with --no-llm hard-fails at ReviewAgent (no provider configured)
    // Do not assert exit success; JSON is still emitted to stdout
    assert_eq!(json["intent_kind"], "action");
    assert_eq!(json["required_capability"], "action");
    assert_eq!(json["outcome"], "error");
    // intake.verdict not present in error response (pipeline fails at agent dispatch)
}

#[test]
fn test_ask_full_pipeline_dry_run_skips_intake() {
    let (output, json, _) = run_ask_json(&["ask", "test", "--dry-run", "--format", "json"]);

    assert!(
        output.status.success(),
        "dry-run should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(json["outcome"], "planned");
    let metadata = json["metadata"]
        .as_object()
        .expect("metadata should be an object");
    assert!(
        !metadata.contains_key("_sunny.intake.verdict"),
        "dry-run should not include intake metadata"
    );
    assert!(
        !metadata.contains_key("_sunny.intake.skip_reason"),
        "dry-run should not include intake skip reason"
    );
}

#[test]
fn test_ask_full_pipeline_blank_input() {
    let (output, json, _) = run_ask_json(&["ask", "", "--no-llm", "--format", "json"]);

    assert!(
        !output.status.success(),
        "blank input should fail command, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(json["outcome"], "error");
    assert_eq!(json["error"]["code"], "blank_input");
}
