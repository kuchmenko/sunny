use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use crate::events::{
    EVENT_TOOL_EXEC_END, EVENT_TOOL_EXEC_ERROR, EVENT_TOOL_EXEC_START, OUTCOME_ERROR,
    OUTCOME_SUCCESS,
};
use crate::tool::ToolError;

/// Capabilities that can never be granted (hard policy).
const HARD_BLOCKED_CAPABILITIES: &[&str] = &["write_outside_workspace", "shell_arbitrary"];

/// Checks whether a named capability is active for the current session/task.
pub trait CapabilityChecker: Send + Sync {
    /// Returns true if the capability is granted for the given pattern.
    /// - `capability`: e.g. "shell_pipes"
    /// - `pattern`: optional refinement, e.g. the RHS command "tail" for shell_pipes
    fn is_granted(&self, capability: &str, pattern: Option<&str>) -> bool;

    /// Returns a human-readable hint for the agent on how to request this capability.
    fn denied_hint(&self, capability: &str, pattern: Option<&str>) -> String;

    /// Returns true if the capability is hard-blocked and can never be granted.
    fn is_hard_blocked(&self, capability: &str) -> bool {
        HARD_BLOCKED_CAPABILITIES.contains(&capability)
    }
}

/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Maximum output bytes per stream (stdout/stderr).
const MAX_OUTPUT_BYTES: usize = 102_400; // 100 KiB

/// Truncation marker appended when output is cut.
const TRUNCATION_MARKER: &str = "\n[OUTPUT TRUNCATED]";

/// Shell composition and substitution operators that are always rejected,
/// unless `shell_pipes` capability is granted.
const BLOCKED_OPERATORS: &[&str] = &[
    ";",  // sequential execution
    "&&", // conditional AND
    "||", // conditional OR
    "|",  // pipe (right-hand side is unvalidated)
    "$(", // command substitution
    "`",  // command substitution (legacy backtick)
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
    checker: Option<Arc<dyn CapabilityChecker>>,
}

impl ShellExecutor {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            checker: None,
        }
    }

    pub fn with_capabilities(root: PathBuf, checker: Arc<dyn CapabilityChecker>) -> Self {
        Self {
            root,
            checker: Some(checker),
        }
    }

    /// Execute `command` in the workspace root directory.
    ///
    /// - `timeout_secs`: override default 30s timeout (`None` = use default)
    /// - Output is captured and truncated at 100 KiB per stream
    /// - Commands require `shell_exec` capability when a checker is configured
    pub async fn execute(
        &self,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ExecResult, ToolError> {
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "shell_exec", command = command);

        // Check hard-blocked capabilities first (before any other validation).
        if let Some(ref checker) = self.checker {
            if checker.is_hard_blocked("shell_arbitrary") {
                let err = ToolError::CommandDenied {
                    command: command.to_string(),
                    reason: "hard-blocked capability: shell_arbitrary".to_string(),
                };
                info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "shell_exec", outcome = OUTCOME_ERROR, error_kind = "CommandDenied", error_message = %err);
                return Err(err);
            }
        }

        // Reject shell composition/substitution operators unless shell_pipes
        // capability is granted.
        let cmd_trimmed = command.trim();
        let has_pipes_cap = self
            .checker
            .as_ref()
            .is_some_and(|checker| checker.is_granted("shell_pipes", None));
        for op in BLOCKED_OPERATORS {
            if has_pipes_cap || !cmd_trimmed.contains(op) {
                continue;
            }

            let err = ToolError::CommandDenied {
                command: command.to_string(),
                reason: format!("shell operator '{op}' requires shell_pipes capability"),
            };
            info!(name: EVENT_TOOL_EXEC_ERROR, tool_name = "shell_exec", outcome = OUTCOME_ERROR, error_kind = "CommandDenied", error_message = %err);
            return Err(err);
        }

        if let Some(checker) = &self.checker {
            let binary_name = cmd_trimmed.split_whitespace().next().unwrap_or(command);
            if !checker.is_granted("shell_exec", Some(binary_name)) {
                let err = ToolError::CommandDenied {
                    command: command.to_string(),
                    reason: checker.denied_hint("shell_exec", Some(binary_name)),
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

    /// Execute `command` bypassing all policy checks.
    /// Use only after explicit human approval has been obtained.
    pub async fn execute_approved(
        &self,
        command: &str,
        timeout_secs: Option<u64>,
    ) -> Result<ExecResult, ToolError> {
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "shell_exec_approved", command = command);

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
            .map_err(|e| ToolError::ExecutionFailed {
                source: Box::new(e),
            })?;

        match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Err(_) => Err(ToolError::CommandTimeout {
                command: command.to_string(),
                timeout_secs: timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS),
            }),
            Ok(Err(e)) => Err(ToolError::ExecutionFailed {
                source: Box::new(e),
            }),
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);
                info!(name: EVENT_TOOL_EXEC_END, tool_name = "shell_exec_approved", outcome = OUTCOME_SUCCESS, exit_code = exit_code);
                Ok(ExecResult {
                    stdout: truncate_output(String::from_utf8_lossy(&output.stdout).into_owned()),
                    stderr: truncate_output(String::from_utf8_lossy(&output.stderr).into_owned()),
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

    struct MockCapabilityChecker {
        grant_shell_exec: bool,
        grant_shell_pipes: bool,
        hard_blocked_capabilities: Vec<String>,
    }

    impl CapabilityChecker for MockCapabilityChecker {
        fn is_granted(&self, capability: &str, _pattern: Option<&str>) -> bool {
            match capability {
                "shell_exec" => self.grant_shell_exec,
                "shell_pipes" => self.grant_shell_pipes,
                _ => false,
            }
        }

        fn denied_hint(&self, capability: &str, pattern: Option<&str>) -> String {
            format!(
                "capability '{capability}' denied for pattern '{}'",
                pattern.unwrap_or_default()
            )
        }

        fn is_hard_blocked(&self, capability: &str) -> bool {
            self.hard_blocked_capabilities
                .contains(&capability.to_string())
        }
    }

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
            .expect("echo should execute without capability checker");
        assert!(result.stdout.contains("hello"));
        assert_eq!(result.exit_code, 0);
        assert!(!result.timed_out);
    }

    #[tokio::test]
    async fn test_shell_arbitrary_command_with_capability() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let checker = Arc::new(MockCapabilityChecker {
            grant_shell_exec: true,
            grant_shell_pipes: false,
            hard_blocked_capabilities: vec![],
        });
        let exec = ShellExecutor::with_capabilities(dir.path().to_path_buf(), checker);

        let result = exec
            .execute("echo hello", None)
            .await
            .expect("shell_exec capability should allow arbitrary command");
        assert!(result.stdout.contains("hello"));
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
    async fn test_shell_denied_without_capability() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let checker = Arc::new(MockCapabilityChecker {
            grant_shell_exec: false,
            grant_shell_pipes: false,
            hard_blocked_capabilities: vec![],
        });
        let exec = ShellExecutor::with_capabilities(dir.path().to_path_buf(), checker);

        let result = exec.execute("ls", None).await;
        assert!(result.is_err());
        let err = result.expect_err("test: expected CommandDenied");
        assert!(matches!(err, ToolError::CommandDenied { .. }));
        if let ToolError::CommandDenied { reason, .. } = err {
            assert!(
                reason.contains("shell_exec"),
                "reason should indicate shell_exec capability denial"
            );
        }
    }

    #[tokio::test]
    async fn test_shell_executor_blocks_semicolon_chaining() {
        let (_dir, exec) = setup();
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
    async fn test_shell_executor_pipe_allowed_with_capability() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let checker = Arc::new(MockCapabilityChecker {
            grant_shell_exec: true,
            grant_shell_pipes: true,
            hard_blocked_capabilities: vec![],
        });
        let exec = ShellExecutor::with_capabilities(dir.path().to_path_buf(), checker);

        let result = exec
            .execute("echo hello | cat", None)
            .await
            .expect("pipe should be allowed with shell_pipes capability");

        assert!(result.stdout.contains("hello"));
        assert_eq!(result.exit_code, 0);
    }

    #[tokio::test]
    async fn test_shell_pipes_with_capability() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let checker = Arc::new(MockCapabilityChecker {
            grant_shell_exec: true,
            grant_shell_pipes: true,
            hard_blocked_capabilities: vec![],
        });
        let exec = ShellExecutor::with_capabilities(dir.path().to_path_buf(), checker);

        let result = exec
            .execute("echo hello | grep hello", None)
            .await
            .expect("shell_pipes capability should permit operators");
        assert!(result.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_shell_executor_pipe_denied_without_capability() {
        let (_dir, exec) = setup();
        let result = exec.execute("echo hello | cat", None).await;

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
            .execute(
                "echo CI=$CI GIT_TERMINAL_PROMPT=$GIT_TERMINAL_PROMPT PAGER=$PAGER",
                None,
            )
            .await
            .expect("echo should execute without capability checker");
        assert!(result.stdout.contains("CI=true"), "CI should be set");
        assert!(
            result.stdout.contains("GIT_TERMINAL_PROMPT=0"),
            "GIT_TERMINAL_PROMPT should be 0"
        );
        assert!(result.stdout.contains("PAGER=cat"), "PAGER should be cat");
    }

    #[tokio::test]
    async fn test_shell_executor_execute_approved_skips_operator_check() {
        let (_dir, exec) = setup();
        let result = exec.execute_approved("echo hello | cat", None).await;
        assert!(
            result.is_ok(),
            "approved exec should skip operator check: {result:?}"
        );
        assert!(result
            .expect("approved command should run")
            .stdout
            .contains("hello"));
    }

    #[tokio::test]
    async fn test_shell_executor_execute_approved_skips_all_checks() {
        let (_dir, exec) = setup();
        let result = exec
            .execute_approved("curl https://example.com", None)
            .await;
        assert!(result.is_ok(), "approved exec should bypass all checks");
    }

    #[tokio::test]
    async fn test_shell_hard_blocked_command_rejected_before_approval() {
        let dir = tempfile::tempdir().expect("test: create temp dir");
        let checker = Arc::new(MockCapabilityChecker {
            grant_shell_exec: true,
            grant_shell_pipes: false,
            hard_blocked_capabilities: vec!["shell_arbitrary".to_string()],
        });
        let exec = ShellExecutor::with_capabilities(dir.path().to_path_buf(), checker);

        let result = exec.execute("echo hello", None).await;

        assert!(result.is_err());
        let err = result.expect_err("test: expected CommandDenied for hard-blocked");
        match err {
            ToolError::CommandDenied { reason, .. } => {
                assert!(
                    reason.contains("hard-blocked"),
                    "reason should mention hard-block: {reason}"
                );
            }
            other => panic!("unexpected error: {other}"),
        }
    }
}
