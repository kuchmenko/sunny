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

/// Commands permitted to execute via `shell_exec`.
///
/// Only the **first whitespace-delimited token** of the command is matched,
/// so `"cargo"` permits `cargo build`, `cargo test`, etc. while still
/// rejecting an unknown token like `cargobuild`.
///
/// Commands not starting with an allowed token are rejected with
/// [`ToolError::CommandDenied`] before any process is spawned.
const ALLOWLIST: &[&str] = &[
    // Rust toolchain
    "cargo", "rustfmt", "rustup", "rustc",
    // Version control (broad operations not covered by git_log/diff/status tools)
    "git",
    // JavaScript / Node
    "npm", "npx", "yarn", "pnpm", "node",
    // Python / packaging
    "python", "python3", "pip", "pip3", "uv", "poetry",
    // Build systems
    "make", "cmake",
    // Read-only file and text inspection
    // (`find` is excluded — use fs_scan / grep_files tools instead)
    "ls", "cat", "head", "tail", "wc", "echo", "pwd",
    "grep", "rg", "ag", "fd", "jq",
    // Environment introspection
    "which", "type", "date", "env", "stat", "du", "df",
];

/// Shell composition and substitution operators that are always rejected,
/// regardless of the command prefix.
///
/// These prevent chaining an allowlisted prefix with an arbitrary second
/// command: `cargo build; rm -rf ~` starts with `"cargo"` but is dangerous.
const BLOCKED_OPERATORS: &[&str] = &[
    ";",   // sequential execution
    "&&",  // conditional AND
    "||",  // conditional OR
    "|",   // pipe (right-hand side is unvalidated)
    "$(",  // command substitution
    "`",   // command substitution (legacy backtick)
    "\n", // newline-separated commands
];
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
    /// - Commands are validated against an allowlist by first token
    pub async fn execute(
        &self,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ExecResult, ToolError> {
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "shell_exec", command = command);

        // Reject shell composition/substitution operators first.
        // Checked before the allowlist so the error message is specific.
        let cmd_trimmed = command.trim();
        for op in BLOCKED_OPERATORS {
            if cmd_trimmed.contains(op) {
                let err = ToolError::CommandDenied {
                    command: command.to_string(),
                    reason: format!("shell operator '{op}' is not permitted; use a single command without chaining"),
                };
                info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "shell_exec", outcome = OUTCOME_ERROR, error_kind = "CommandDenied", error_message = %err);
                return Err(err);
            }
        }

        // Validate the first token against the allowlist.
        let first_token = cmd_trimmed.split_whitespace().next().unwrap_or("");
        if !ALLOWLIST.contains(&first_token) {
            let err = ToolError::CommandDenied {
                command: command.to_string(),
                reason: format!(
                    "'{}' is not in the command allowlist; permitted commands: {}",
                    first_token,
                    ALLOWLIST.join(", ")
                ),
            };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "shell_exec", outcome = OUTCOME_ERROR, error_kind = "CommandDenied", error_message = %err);
            return Err(err);
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
    async fn test_shell_executor_allowlisted_command_runs() {
        let (_dir, exec) = setup();
        let result = exec
            .execute("echo hello", None)
            .await
            .expect("echo is on the allowlist");
        assert!(result.stdout.contains("hello"));
        assert_eq!(result.exit_code, 0);
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn test_shell_executor_timeout() {
        let (_dir, exec) = setup();
        // 0-second timeout forces immediate timeout regardless of how fast the command is.
        let result = exec.execute("cargo search serde", Some(0)).await;
        assert!(result.is_err());
        assert!(matches!(
            result.expect_err("test: expected timeout"),
            ToolError::CommandTimeout { .. }
        ));
    }

    #[tokio::test]
    async fn test_shell_executor_rejects_command_not_on_allowlist() {
        let (_dir, exec) = setup();
        // `sudo` is not on the allowlist
        let result = exec.execute("sudo rm -rf /", None).await;
        assert!(result.is_err());
        let err = result.expect_err("test: expected CommandDenied");
        assert!(matches!(err, ToolError::CommandDenied { .. }));
        if let ToolError::CommandDenied { reason, .. } = err {
            assert!(reason.contains("sudo"), "reason should name the rejected token");
        }
    }

    #[tokio::test]
    async fn test_shell_executor_rejects_empty_command() {
        let (_dir, exec) = setup();
        let result = exec.execute("", None).await;
        assert!(matches!(
            result.expect_err("test: expected CommandDenied"),
            ToolError::CommandDenied { .. }
        ));
    }

    #[tokio::test]
    async fn test_shell_executor_blocks_semicolon_chaining() {
        let (_dir, exec) = setup();
        // Allowlisted prefix + dangerous chained command
        let result = exec.execute("cargo build; rm -rf ~", None).await;
        assert!(result.is_err());
        let err = result.expect_err("test: expected CommandDenied");
        assert!(matches!(err, ToolError::CommandDenied { .. }));
        if let ToolError::CommandDenied { reason, .. } = err {
            assert!(reason.contains(";"), "reason should name the operator");
        }
    }

    #[tokio::test]
    async fn test_shell_executor_blocks_pipe() {
        let (_dir, exec) = setup();
        let result = exec.execute("cargo build | sh", None).await;
        assert!(result.is_err());
        assert!(matches!(
            result.expect_err("test: expected CommandDenied"),
            ToolError::CommandDenied { .. }
        ));
    }

    #[tokio::test]
    async fn test_shell_executor_blocks_and_operator() {
        let (_dir, exec) = setup();
        let result = exec.execute("ls && rm -rf /", None).await;
        assert!(result.is_err());
        assert!(matches!(
            result.expect_err("test: expected CommandDenied"),
            ToolError::CommandDenied { .. }
        ));
    }

    #[tokio::test]
    async fn test_shell_executor_blocks_command_substitution() {
        let (_dir, exec) = setup();
        let result = exec.execute("echo $(cat /etc/passwd)", None).await;
        assert!(result.is_err());
        assert!(matches!(
            result.expect_err("test: expected CommandDenied"),
            ToolError::CommandDenied { .. }
        ));
    }

    #[tokio::test]
    async fn test_shell_executor_blocks_backtick_substitution() {
        let (_dir, exec) = setup();
        let result = exec.execute("echo `id`", None).await;
        assert!(result.is_err());
        assert!(matches!(
            result.expect_err("test: expected CommandDenied"),
            ToolError::CommandDenied { .. }
        ));
    }

    #[tokio::test]
    async fn test_shell_executor_nonzero_exit_is_not_an_error() {
        let (_dir, exec) = setup();
        // `ls /nonexistent` is allowed but exits nonzero — that is not a ToolError
        let result = exec
            .execute("ls /nonexistent_path_xyz", None)
            .await
            .expect("nonzero exit should not be a ToolError");
        assert_ne!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn test_shell_executor_non_interactive_env_injected() {
        let (_dir, exec) = setup();
        let result = exec
            .execute("echo CI=$CI GIT_TERMINAL_PROMPT=$GIT_TERMINAL_PROMPT PAGER=$PAGER", None)
            .await
            .expect("echo is on the allowlist");
        assert!(result.stdout.contains("CI=true"), "CI should be set");
        assert!(
            result.stdout.contains("GIT_TERMINAL_PROMPT=0"),
            "GIT_TERMINAL_PROMPT should be 0"
        );
        assert!(result.stdout.contains("PAGER=cat"), "PAGER should be cat");
    }
}
