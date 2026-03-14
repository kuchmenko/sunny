use std::path::PathBuf;
use std::time::Duration;

use tracing::info;

use crate::events::{
    EVENT_TOOL_EXEC_END, EVENT_TOOL_EXEC_ERROR, EVENT_TOOL_EXEC_START, OUTCOME_ERROR,
    OUTCOME_SUCCESS,
};
use crate::tool::ToolError;

/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Maximum output bytes per stream (stdout/stderr).
const MAX_OUTPUT_BYTES: usize = 102_400; // 100 KiB

/// Truncation marker appended when output is cut.
const TRUNCATION_MARKER: &str = "\n[OUTPUT TRUNCATED]";

/// Commands that are never allowed to execute.
const DENYLIST: &[&str] = &["rm -rf /", "sudo", "mkfs", "dd if=", ":(){"];

/// Environment variables injected into every spawned command.
///
/// Prevents interactive prompts that would hang the timeout silently:
/// - `CI=true` — suppresses interactive prompts in most tools
/// - `GIT_TERMINAL_PROMPT=0` — prevents git credential prompts
/// - `GIT_PAGER=cat` / `PAGER=cat` — disables pagers that block output capture
/// - `DEBIAN_FRONTEND=noninteractive` — suppresses apt interactive dialogs
/// - `HOMEBREW_NO_AUTO_UPDATE=1` — prevents brew from auto-updating on every run
/// - `GIT_EDITOR=:` / `EDITOR=:` — prevents editor spawns (e.g. git commit without -m)
/// - `GIT_MERGE_AUTOEDIT=no` — skips merge commit message editor
/// - `VISUAL=` — clears visual editor to prevent TUI spawns
/// - `NPM_CONFIG_YES=true` — auto-confirms npm prompts
/// - `PIP_NO_INPUT=1` — suppresses pip interactive prompts
/// - `YARN_ENABLE_IMMUTABLE_INSTALLS=false` — allows yarn install in CI-like envs
const NON_INTERACTIVE_ENV: &[(&str, &str)] = &[
    ("CI", "true"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_PAGER", "cat"),
    ("PAGER", "cat"),
    ("DEBIAN_FRONTEND", "noninteractive"),
    ("HOMEBREW_NO_AUTO_UPDATE", "1"),
    ("GIT_EDITOR", ":"),
    ("EDITOR", ":"),
    ("VISUAL", ""),
    ("GIT_SEQUENCE_EDITOR", ":"),
    ("GIT_MERGE_AUTOEDIT", "no"),
    ("NPM_CONFIG_YES", "true"),
    ("PIP_NO_INPUT", "1"),
    ("YARN_ENABLE_IMMUTABLE_INSTALLS", "false"),
];

/// Result of a shell command execution.
#[derive(Debug)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

/// Executes shell commands within a sandboxed working directory.
pub struct ShellExecutor {
    root: PathBuf,
}

impl ShellExecutor {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Execute `command` in the workspace root directory.
    ///
    /// - `timeout_secs`: override default 30s timeout (`None` = use default)
    /// - Output is captured and truncated at 100 KiB per stream
    /// - Denylisted commands are rejected before execution
    pub async fn execute(
        &self,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ExecResult, ToolError> {
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "shell_exec", command = command);

        // Check denylist
        let cmd_trimmed = command.trim();
        for denied in DENYLIST {
            if cmd_trimmed.contains(denied) {
                let err = ToolError::CommandDenied {
                    command: command.to_string(),
                    reason: format!("matches denylist pattern '{denied}'"),
                };
                info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "shell_exec", outcome = OUTCOME_ERROR, error_kind = "CommandDenied", error_message = %err);
                return Err(err);
            }
        }

        let timeout = Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS));

        let child = tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.root)
            .envs(NON_INTERACTIVE_ENV.iter().copied())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                let err = ToolError::ExecutionFailed {
                    source: Box::new(e),
                };
                info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "shell_exec", outcome = OUTCOME_ERROR, error_kind = "SpawnFailed", error_message = %err);
                err
            })?;

        let result = tokio::time::timeout(timeout, child.wait_with_output()).await;

        match result {
            Err(_elapsed) => {
                // Timeout — process is killed by kill_on_drop
                let err = ToolError::CommandTimeout {
                    command: command.to_string(),
                    timeout_secs: timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
                };
                info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "shell_exec", outcome = OUTCOME_ERROR, error_kind = "CommandTimeout", error_message = %err);
                Err(err)
            }
            Ok(Err(e)) => {
                let err = ToolError::ExecutionFailed {
                    source: Box::new(e),
                };
                info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "shell_exec", outcome = OUTCOME_ERROR, error_kind = "ExecutionFailed", error_message = %err);
                Err(err)
            }
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);

                let stdout = truncate_output(String::from_utf8_lossy(&output.stdout).into_owned());
                let stderr = truncate_output(String::from_utf8_lossy(&output.stderr).into_owned());

                info!(name: EVENT_TOOL_EXEC_END, tool_name = "shell_exec", outcome = OUTCOME_SUCCESS, exit_code = exit_code);

                Ok(ExecResult {
                    stdout,
                    stderr,
                    exit_code,
                    timed_out: false,
                })
            }
        }
    }
}

fn truncate_output(mut s: String) -> String {
    if s.len() > MAX_OUTPUT_BYTES {
        s.truncate(MAX_OUTPUT_BYTES);
        s.push_str(TRUNCATION_MARKER);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (tempfile::TempDir, ShellExecutor) {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let exec = ShellExecutor::new(dir.path().to_path_buf());
        (dir, exec)
    }

    #[tokio::test]
    async fn test_shell_executor_simple_command() {
        let (_dir, exec) = setup();
        let result = exec
            .execute("echo hello", None)
            .await
            .expect("execute should succeed");
        assert!(result.stdout.contains("hello"));
        assert_eq!(result.exit_code, 0);
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn test_shell_executor_timeout() {
        let (_dir, exec) = setup();
        let result = exec.execute("sleep 60", Some(1)).await;
        assert!(result.is_err());
        assert!(matches!(
            result.expect_err("test: expected timeout"),
            ToolError::CommandTimeout { .. }
        ));
    }

    #[tokio::test]
    async fn test_shell_executor_denylist_rejected() {
        let (_dir, exec) = setup();
        let result = exec.execute("sudo rm -rf /tmp/test", None).await;
        assert!(result.is_err());
        assert!(matches!(
            result.expect_err("test: expected denied"),
            ToolError::CommandDenied { .. }
        ));
    }

    #[tokio::test]
    async fn test_shell_executor_nonzero_exit() {
        let (_dir, exec) = setup();
        let result = exec
            .execute("exit 1", None)
            .await
            .expect("should not error");
        assert_eq!(result.exit_code, 1);
    }

    #[tokio::test]
    async fn test_shell_executor_non_interactive_env_injected() {
        let (_dir, exec) = setup();
        // Verify CI and GIT_TERMINAL_PROMPT are set in the child environment.
        let result = exec
            .execute("echo CI=$CI GIT_TERMINAL_PROMPT=$GIT_TERMINAL_PROMPT PAGER=$PAGER", None)
            .await
            .expect("should execute");
        assert!(result.stdout.contains("CI=true"), "CI should be set");
        assert!(
            result.stdout.contains("GIT_TERMINAL_PROMPT=0"),
            "GIT_TERMINAL_PROMPT should be 0"
        );
        assert!(result.stdout.contains("PAGER=cat"), "PAGER should be cat");
    }
}
