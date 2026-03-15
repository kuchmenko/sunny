use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

use crate::model::VerifyCommand;

#[derive(Debug)]
pub struct VerificationResult {
    pub passed: bool,
    pub command: String,
    pub exit_code: i32,
    pub expected_exit_code: i32,
    pub output: String,
}

#[derive(Debug)]
pub struct VerificationReport {
    pub all_passed: bool,
    pub results: Vec<VerificationResult>,
}

impl VerificationReport {
    pub fn failure_summary(&self) -> Option<String> {
        if self.all_passed {
            return None;
        }

        let mut lines = vec!["Verification failed:".to_string()];
        for result in self.results.iter().filter(|item| !item.passed) {
            lines.push(format!(
                "- `{}` exited with {}, expected {}",
                result.command, result.exit_code, result.expected_exit_code
            ));
            if !result.output.trim().is_empty() {
                lines.push(format!("  output: {}", result.output.trim()));
            }
        }

        Some(lines.join("\n"))
    }
}

pub struct AcceptanceCriteriaVerifier {
    working_dir: PathBuf,
}

impl AcceptanceCriteriaVerifier {
    pub fn new(working_dir: PathBuf) -> Self {
        Self { working_dir }
    }

    pub async fn verify(&self, commands: &[VerifyCommand]) -> VerificationReport {
        let mut results = Vec::new();
        let mut all_passed = true;

        for verify_command in commands {
            let mut command = shell_command(&verify_command.command);
            command
                .current_dir(&self.working_dir)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());

            let run_result = timeout(
                Duration::from_secs(u64::from(verify_command.timeout_secs)),
                command.output(),
            )
            .await;

            let result = match run_result {
                Ok(Ok(output)) => {
                    let exit_code = output.status.code().unwrap_or(-1);
                    let mut combined = String::new();
                    combined.push_str(&String::from_utf8_lossy(&output.stdout));
                    if !output.stderr.is_empty() {
                        if !combined.is_empty() && !combined.ends_with('\n') {
                            combined.push('\n');
                        }
                        combined.push_str(&String::from_utf8_lossy(&output.stderr));
                    }
                    let passed = exit_code == verify_command.expected_exit_code;
                    VerificationResult {
                        passed,
                        command: verify_command.command.clone(),
                        exit_code,
                        expected_exit_code: verify_command.expected_exit_code,
                        output: combined,
                    }
                }
                Ok(Err(error)) => VerificationResult {
                    passed: false,
                    command: verify_command.command.clone(),
                    exit_code: -1,
                    expected_exit_code: verify_command.expected_exit_code,
                    output: format!("failed to execute command: {error}"),
                },
                Err(_) => VerificationResult {
                    passed: false,
                    command: verify_command.command.clone(),
                    exit_code: -1,
                    expected_exit_code: verify_command.expected_exit_code,
                    output: format!("command timed out after {}s", verify_command.timeout_secs),
                },
            };

            if !result.passed {
                all_passed = false;
                results.push(result);
                break;
            }

            results.push(result);
        }

        VerificationReport {
            all_passed,
            results,
        }
    }
}

#[cfg(unix)]
fn shell_command(script: &str) -> Command {
    let mut command = Command::new("sh");
    command.arg("-lc").arg(script);
    command
}

#[cfg(windows)]
fn shell_command(script: &str) -> Command {
    let mut command = Command::new("cmd");
    command.arg("/C").arg(script);
    command
}

#[cfg(test)]
mod tests {
    use super::{AcceptanceCriteriaVerifier, VerificationReport};
    use crate::model::VerifyCommand;

    fn command(command: &str, expected_exit_code: i32) -> VerifyCommand {
        VerifyCommand {
            id: 1,
            criteria_id: 1,
            command: command.to_string(),
            expected_exit_code,
            timeout_secs: 5,
            seq: 0,
        }
    }

    #[tokio::test]
    async fn test_verify_passing_command() {
        let verifier = AcceptanceCriteriaVerifier::new(std::env::temp_dir());
        let report = verifier.verify(&[command("echo ok", 0)]).await;

        assert!(report.all_passed);
        assert_eq!(report.results.len(), 1);
        assert!(report.results[0].passed);
    }

    #[tokio::test]
    async fn test_verify_failing_command() {
        let verifier = AcceptanceCriteriaVerifier::new(std::env::temp_dir());
        let report = verifier.verify(&[command("false", 0)]).await;

        assert!(!report.all_passed);
        assert_eq!(report.results.len(), 1);
        assert!(!report.results[0].passed);
        assert_eq!(report.results[0].exit_code, 1);
    }

    #[tokio::test]
    async fn test_verify_expected_nonzero_exit() {
        let verifier = AcceptanceCriteriaVerifier::new(std::env::temp_dir());
        let report = verifier.verify(&[command("false", 1)]).await;

        assert!(report.all_passed);
        assert_eq!(report.results.len(), 1);
        assert!(report.results[0].passed);
    }

    #[test]
    fn test_failure_summary_is_none_when_all_pass() {
        let report = VerificationReport {
            all_passed: true,
            results: vec![],
        };

        assert!(report.failure_summary().is_none());
    }

    #[test]
    fn test_failure_summary_includes_command_on_fail() {
        let report = VerificationReport {
            all_passed: false,
            results: vec![super::VerificationResult {
                passed: false,
                command: "false".to_string(),
                exit_code: 1,
                expected_exit_code: 0,
                output: "".to_string(),
            }],
        };

        let summary = report.failure_summary().expect("summary should exist");
        assert!(summary.contains("false"));
    }
}
