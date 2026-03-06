//! Integration tests for the prompt command

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
fn test_prompt_dry_run_returns_plan() {
    let output = sunny_cli()
        .args([
            "prompt",
            "analyze this code",
            "--dry-run",
            "--no-llm",
            "--format",
            "json",
        ])
        .output()
        .expect("should run prompt command");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "command should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("\"dry_run\": true"),
        "should contain dry_run flag"
    );
    assert!(stdout.contains("\"plan_id\""), "should contain plan_id");
}

#[test]
fn test_prompt_help_shows_usage() {
    let output = sunny_cli()
        .args(["prompt", "--help"])
        .output()
        .expect("should run prompt --help");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "help should succeed");
    assert!(
        stdout.contains("--dry-run"),
        "help should mention --dry-run"
    );
    assert!(stdout.contains("--format"), "help should mention --format");
}

#[test]
fn test_analyze_still_works() {
    let output = sunny_cli()
        .args(["analyze", "--help"])
        .output()
        .expect("should run analyze --help");

    assert!(output.status.success(), "analyze command should still work");
}
