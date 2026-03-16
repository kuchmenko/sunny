use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use sunny_boys::agent::tools::build_tool_executor_with_capabilities;
use sunny_boys::tool_loop::ToolExecutor;
use sunny_core::tool::{CapabilityChecker, InterviewAnswer, InterviewContext, ToolError};
use tempfile::TempDir;

fn run_tool(
    executor: &Arc<ToolExecutor>,
    name: &str,
    args: serde_json::Value,
) -> Result<String, ToolError> {
    executor.as_ref()("test-call", name, &args.to_string(), 0)
}

struct MockCapabilityChecker {
    granted: HashSet<String>,
}

impl MockCapabilityChecker {
    fn new(granted: &[&str]) -> Self {
        Self {
            granted: granted.iter().map(|cap| cap.to_string()).collect(),
        }
    }
}

impl CapabilityChecker for MockCapabilityChecker {
    fn is_granted(&self, capability: &str, _pattern: Option<&str>) -> bool {
        self.granted.contains(capability)
    }

    fn denied_hint(&self, capability: &str, pattern: Option<&str>) -> String {
        format!(
            "capability '{capability}' denied for pattern '{}'",
            pattern.unwrap_or_default()
        )
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn test_stale_read_full_cycle() {
    let root = TempDir::new().expect("test: create temp dir");
    let file = root.path().join("tracked.txt");
    std::fs::write(&file, "initial\nvalue\n").expect("test: seed file");

    let executor =
        build_tool_executor_with_capabilities(root.path().to_path_buf(), None, None, None, None);

    let first_read = run_tool(
        &executor,
        "fs_read",
        serde_json::json!({ "path": "tracked.txt" }),
    )
    .expect("test: initial read succeeds");
    assert!(first_read.contains("1:initial"));

    tokio::time::sleep(Duration::from_millis(50)).await;
    std::fs::write(&file, "external update\n").expect("test: external write");

    let stale_err = run_tool(
        &executor,
        "fs_write",
        serde_json::json!({ "path": "tracked.txt", "content": "agent update\n" }),
    )
    .expect_err("test: stale snapshot blocks write");
    assert!(
        stale_err
            .to_string()
            .contains("File has been modified since it was last read"),
        "unexpected error: {stale_err}"
    );

    run_tool(
        &executor,
        "fs_read",
        serde_json::json!({ "path": "tracked.txt" }),
    )
    .expect("test: reread refreshes snapshot");

    let write_ok = run_tool(
        &executor,
        "fs_write",
        serde_json::json!({ "path": "tracked.txt", "content": "agent update\n" }),
    )
    .expect("test: write after reread succeeds");
    assert!(write_ok.contains("Written 13 bytes to"));
    assert_eq!(
        std::fs::read_to_string(&file).expect("test: read final content"),
        "agent update\n"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_edit_line_hint_disambiguation() {
    let root = TempDir::new().expect("test: create temp dir");
    let file = root.path().join("dupes.txt");
    let content = [
        "start one",
        "target = old",
        "end one",
        "",
        "start two",
        "target = old",
        "end two",
        "",
        "start three",
        "target = old",
        "end three",
        "",
    ]
    .join("\n");
    std::fs::write(&file, content).expect("test: seed duplicate blocks");

    let executor =
        build_tool_executor_with_capabilities(root.path().to_path_buf(), None, None, None, None);

    run_tool(
        &executor,
        "fs_edit",
        serde_json::json!({
            "path": "dupes.txt",
            "old_string": "target = old",
            "new_string": "target = new",
            "line_hint": 6
        }),
    )
    .expect("test: edit with line_hint should succeed");

    let lines: Vec<String> = std::fs::read_to_string(&file)
        .expect("test: read edited content")
        .lines()
        .map(ToString::to_string)
        .collect();

    let new_lines: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| (line == "target = new").then_some(idx + 1))
        .collect();
    let old_lines: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter_map(|(idx, line)| (line == "target = old").then_some(idx + 1))
        .collect();

    assert_eq!(new_lines, vec![6]);
    assert_eq!(old_lines, vec![2, 10]);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_shell_capability_enforcement() {
    let root = TempDir::new().expect("test: create temp dir");
    let checker: Arc<dyn CapabilityChecker> = Arc::new(MockCapabilityChecker::new(&[]));
    let executor = build_tool_executor_with_capabilities(
        root.path().to_path_buf(),
        Some(checker),
        None,
        None,
        None,
    );

    let result = run_tool(
        &executor,
        "shell_exec",
        serde_json::json!({ "command": "echo hello" }),
    );

    assert!(matches!(result, Err(ToolError::CommandDenied { .. })));
}

#[tokio::test(flavor = "multi_thread")]
async fn test_git_commit_capability_check() {
    let root = TempDir::new().expect("test: create temp dir");
    let checker: Arc<dyn CapabilityChecker> = Arc::new(MockCapabilityChecker::new(&[]));
    let executor = build_tool_executor_with_capabilities(
        root.path().to_path_buf(),
        Some(checker),
        None,
        None,
        None,
    );

    let result = run_tool(
        &executor,
        "git_commit",
        serde_json::json!({ "message": "test commit", "files": [] }),
    )
    .expect_err("test: git commit should be gated without git_write");

    assert!(
        result
            .to_string()
            .contains("missing 'git_write' capability"),
        "unexpected error: {result}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn test_interview_context_accumulation() {
    let _root = TempDir::new().expect("test: create temp dir");
    let mut context = InterviewContext::new();

    context.add_answer(InterviewAnswer {
        question_id: "q1".to_string(),
        value: "yes".to_string(),
        selected_options: vec!["option_a".to_string()],
    });
    context.add_answer(InterviewAnswer {
        question_id: "q2".to_string(),
        value: "second round".to_string(),
        selected_options: vec!["opt_1".to_string(), "opt_2".to_string()],
    });

    let json = context.to_json();
    assert_eq!(json["answers"]["q1"]["value"], "yes");
    assert_eq!(json["answers"]["q2"]["value"], "second round");
    assert_eq!(json["answers"]["q2"]["selected_options"][0], "opt_1");
    assert_eq!(json["answers"]["q2"]["selected_options"][1], "opt_2");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_fs_glob_finds_files() {
    let root = TempDir::new().expect("test: create temp dir");
    let src_dir = root.path().join("src");
    std::fs::create_dir_all(&src_dir).expect("test: create src dir");
    std::fs::write(src_dir.join("lib.rs"), "pub fn a() {}\n").expect("test: write lib.rs");
    std::fs::write(src_dir.join("main.rs"), "fn main() {}\n").expect("test: write main.rs");
    std::fs::write(root.path().join("notes.txt"), "notes\n").expect("test: write notes.txt");

    let executor =
        build_tool_executor_with_capabilities(root.path().to_path_buf(), None, None, None, None);
    let output = run_tool(
        &executor,
        "fs_glob",
        serde_json::json!({ "pattern": "**/*.rs" }),
    )
    .expect("test: glob should succeed");

    let mut matches: Vec<String> =
        serde_json::from_str(&output).expect("test: parse fs_glob json output");
    matches.sort();

    assert_eq!(
        matches,
        vec!["src/lib.rs".to_string(), "src/main.rs".to_string()]
    );
}
