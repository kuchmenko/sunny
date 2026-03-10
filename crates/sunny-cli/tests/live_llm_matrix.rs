use std::process::{Command, Output};

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

fn run_ask(args: &[&str]) -> Output {
    sunny_cli()
        .args(args)
        .output()
        .expect("should run sunny-cli ask command")
}

fn parse_stdout_json(output: &Output) -> serde_json::Value {
    for index in (0..output.stdout.len()).rev() {
        if output.stdout[index] != b'{' {
            continue;
        }

        if let Ok(parsed) = serde_json::from_slice(&output.stdout[index..]) {
            return parsed;
        }
    }

    panic!(
        "expected JSON stdout, stdout: {}, stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn has_kimi_api_key() -> bool {
    std::env::var("KIMI_API_KEY")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

#[test]
#[ignore = "live matrix test"]
fn test_live_matrix_dry_run_json() {
    let output = run_ask(&[
        "ask",
        "What does the Sunny runtime do?",
        "--dry-run",
        "--format",
        "json",
    ]);

    assert!(
        output.status.success(),
        "dry run should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = parse_stdout_json(&output);
    assert_eq!(parsed["dry_run"].as_bool(), Some(true));
    assert_eq!(parsed["outcome"].as_str(), Some("planned"));
    assert!(
        parsed["request_id"]
            .as_str()
            .map(|value| !value.is_empty())
            .unwrap_or(false),
        "request_id should be present"
    );
}

#[test]
#[ignore = "live matrix test"]
fn test_live_matrix_llm_kimi_json() {
    if !has_kimi_api_key() {
        return;
    }

    let output = run_ask(&[
        "ask",
        "Explain the orchestrator design in this codebase",
        "--format",
        "json",
    ]);

    assert!(
        output.status.success(),
        "llm ask should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = parse_stdout_json(&output);
    assert_ne!(
        parsed["metadata"]["_sunny.provider.id"].as_str(),
        Some("none")
    );
    assert!(
        parsed["request_id"]
            .as_str()
            .map(|value| !value.is_empty())
            .unwrap_or(false),
        "request_id should be present"
    );
}

#[test]
#[ignore = "live matrix test"]
fn test_live_matrix_no_llm_json() {
    let output = run_ask(&[
        "ask",
        "Analyze the test infrastructure",
        "--no-llm",
        "--format",
        "json",
    ]);

    assert!(
        output.status.success(),
        "no-llm ask should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = parse_stdout_json(&output);
    assert_eq!(
        parsed["metadata"]["_sunny.provider.id"].as_str(),
        Some("none")
    );
    assert_eq!(
        parsed["metadata"]["_sunny.provider.mode"].as_str(),
        Some("no_llm")
    );
    assert!(
        parsed["request_id"]
            .as_str()
            .map(|value| !value.is_empty())
            .unwrap_or(false),
        "request_id should be present"
    );
}

#[test]
#[ignore = "live matrix test"]
fn test_live_matrix_llm_review_json() {
    if !has_kimi_api_key() {
        return;
    }

    let output = run_ask(&[
        "ask",
        "Review the PlanningIntake implementation",
        "--format",
        "json",
    ]);

    assert!(
        output.status.success(),
        "llm review ask should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let parsed = parse_stdout_json(&output);
    assert_ne!(
        parsed["metadata"]["_sunny.provider.id"].as_str(),
        Some("none")
    );
    assert!(
        parsed["request_id"]
            .as_str()
            .map(|value| !value.is_empty())
            .unwrap_or(false),
        "request_id should be present"
    );
}
