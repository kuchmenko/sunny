//! Integration tests for the ask command

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
fn test_ask_dry_run_returns_plan() {
    let output = sunny_cli()
        .args([
            "ask",
            "analyze this code",
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
    assert!(
        stdout.contains("\"dry_run\": true"),
        "should contain dry_run flag"
    );
    assert!(stdout.contains("\"plan_id\""), "should contain plan_id");
}

#[test]
fn test_ask_help_shows_usage() {
    let output = sunny_cli()
        .args(["ask", "--help"])
        .output()
        .expect("should run ask --help");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "help should succeed");
    assert!(
        stdout.contains("--dry-run"),
        "help should mention --dry-run"
    );
    assert!(stdout.contains("--format"), "help should mention --format");
}

#[test]
fn test_ask_with_no_llm() {
    let output = sunny_cli()
        .args([
            "ask",
            "what is this",
            "--no-llm",
            "--dry-run",
            "--format",
            "json",
        ])
        .output()
        .expect("should run ask command with --no-llm");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "command should succeed, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("\"plan_id\""), "should contain plan_id");
}

#[test]
fn test_ask_routes_to_codebase_for_query() {
    use std::fs;
    let dir = std::env::temp_dir().join(format!("sunny_ask_int_codebase_{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create temp dir");
    fs::write(dir.join("main.rs"), "fn main() {}\n").expect("write file");

    let output = sunny_cli()
        .args([
            "ask",
            dir.to_str().expect("valid path"),
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
    assert!(
        stdout.contains("\"intent_kind\": \"query\""),
        "should classify as query, got: {stdout}"
    );
    assert!(
        stdout.contains("file_count"),
        "should contain CodebaseAgent response marker, got: {stdout}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn test_ask_routes_to_review_for_analyze() {
    let output = sunny_cli()
        .args([
            "ask",
            "review this code snippet",
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
    assert!(
        stdout.contains("\"intent_kind\": \"analyze\""),
        "should classify as analyze, got: {stdout}"
    );
    assert!(
        stdout.contains("REVIEW FEEDBACK"),
        "should contain ReviewAgent response marker, got: {stdout}"
    );
}

#[test]
fn test_ask_routes_to_critique_for_action() {
    let output = sunny_cli()
        .args([
            "ask",
            "create a fix for this bug",
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
    assert!(
        stdout.contains("\"intent_kind\": \"action\""),
        "should classify as action, got: {stdout}"
    );
    assert!(
        stdout.contains("CRITIQUE REPORT"),
        "should contain CritiqueAgent response marker, got: {stdout}"
    );
}
