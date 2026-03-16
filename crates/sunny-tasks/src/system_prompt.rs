use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::model::{AcceptCriteria, Task, VerifyCommand};

pub struct TaskPromptContext {
    pub git_root: PathBuf,
    pub task: Task,
    pub accept_criteria: Option<AcceptCriteria>,
    pub verify_commands: Vec<VerifyCommand>,
    pub dep_results: Vec<CompletedDepResult>,
    pub children_results: Vec<CompletedDepResult>,
    pub workspace_snapshot: WorkspaceSnapshot,
    pub running_siblings: Vec<SiblingTask>,
    pub conventions: Option<String>,
    pub repo_map: Option<String>,
}

pub struct CompletedDepResult {
    pub task_id: String,
    pub title: String,
    pub completed_at: DateTime<Utc>,
    pub summary: String,
    pub diff: Option<String>,
    pub changed_files: Vec<String>,
}

pub struct WorkspaceSnapshot {
    pub branch: String,
    pub status_short: String,
    pub recent_log: String,
}

pub struct SiblingTask {
    pub task_id: String,
    pub title: String,
    pub claimed_paths: Vec<String>,
}

pub struct SystemPromptBuilder;

impl SystemPromptBuilder {
    pub fn build(ctx: &TaskPromptContext) -> String {
        let mut parts = vec![Self::layer_role(&ctx.git_root)];

        if let Some(ref conventions) = ctx.conventions {
            parts.push(Self::layer_conventions(conventions, 12_000));
        }

        if let Some(ref repo_map) = ctx.repo_map {
            parts.push(Self::layer_repo_map(repo_map, 20_000));
        }

        parts.push(Self::layer_task_spec(
            &ctx.task,
            &ctx.accept_criteria,
            &ctx.verify_commands,
        ));

        if !ctx.dep_results.is_empty() {
            parts.push(Self::layer_dep_results(&ctx.dep_results, 8_000));
        }

        if !ctx.children_results.is_empty() {
            parts.push(Self::layer_children_results(&ctx.children_results, 8_000));
        }

        parts.push(Self::layer_workspace_snapshot(
            &ctx.workspace_snapshot,
            2_000,
        ));

        if !ctx.running_siblings.is_empty() {
            parts.push(Self::layer_siblings(&ctx.running_siblings, 1_000));
        }

        parts.join("\n\n")
    }

    fn layer_role(git_root: &Path) -> String {
        format!(
            "You are Claude Code, Anthropic's official CLI for Claude.\n\n\
             You are a task-executing agent working in the workspace at: {}.\n\n\
             You have access to tools for reading, writing, editing files, executing shell commands, \
             and managing tasks. Use them to complete your assigned task.\n\n\
             Always think carefully before using tools. Complete your task, then call task_complete().",
            git_root.display()
        )
    }

    fn layer_conventions(conventions: &str, cap: usize) -> String {
        let truncated: String = conventions.chars().take(cap).collect();
        format!("# Project Conventions\n\n{truncated}")
    }

    fn layer_repo_map(repo_map: &str, cap: usize) -> String {
        let truncated: String = repo_map.chars().take(cap).collect();
        format!("# Repository Map\n\n{truncated}")
    }

    fn layer_task_spec(
        task: &Task,
        criteria: &Option<AcceptCriteria>,
        commands: &[VerifyCommand],
    ) -> String {
        let mut output = String::new();
        output.push_str("# Your Task\n");
        output.push_str(&format!("**ID**: {}\n", task.id));
        output.push_str(&format!("**Title**: {}\n\n", task.title));
        output.push_str("## Specification\n");
        output.push_str(&task.description);
        output.push('\n');
        output.push('\n');

        output.push_str("## Definition of Done\n");
        match criteria {
            Some(item) => output.push_str(&item.description),
            None => output.push_str(
                "No explicit definition provided. Complete the task description faithfully.",
            ),
        }
        output.push('\n');
        output.push('\n');

        if !commands.is_empty() {
            output.push_str("## Verification (automated)\n");
            for (idx, command) in commands.iter().enumerate() {
                output.push_str(&format!(
                    "[{}] {} (exit {})\n",
                    idx + 1,
                    command.command,
                    command.expected_exit_code
                ));
            }
            output.push('\n');
        }

        output.push_str("## Task Tools (required)\n");
        output.push_str("- Call task_complete(summary) when done.\n");
        output.push_str("- Call task_fail(error) if blocked by an unrecoverable issue.\n");
        output.push_str(
            "- If you need human input, call task_ask_human(question, options?) and then stop.\n",
        );
        output.push_str("- Keep edits scoped to this task's objective.\n");
        output
            .push_str("- Do not end the conversation without calling task_complete or task_fail.");

        output
    }

    fn layer_dep_results(deps: &[CompletedDepResult], budget: usize) -> String {
        let mut sorted: Vec<&CompletedDepResult> = deps.iter().collect();
        sorted.sort_by_key(|item| std::cmp::Reverse(item.completed_at));

        let mut output = String::from("# Dependency Results\n\n");
        let per_diff_cap = std::cmp::max(budget / sorted.len(), 500);

        for dep in sorted {
            let changed_files = if dep.changed_files.is_empty() {
                "(none)".to_string()
            } else {
                dep.changed_files.join(", ")
            };

            output.push_str(&format!("## {} ({})\n", dep.title, dep.task_id));
            output.push_str(&format!("Completed: {}\n", dep.completed_at.to_rfc3339()));
            output.push_str(&format!("Summary: {}\n", dep.summary));
            output.push_str(&format!("Changed files: {}\n", changed_files));

            if let Some(diff) = dep.diff.as_ref() {
                let diff_truncated: String = diff.chars().take(per_diff_cap).collect();
                output.push_str("Diff (truncated):\n");
                output.push_str(&diff_truncated);
                output.push('\n');
                output.push_str("Full diff available via fs_read if needed.\n");
            }

            output.push('\n');
            if output.chars().count() > budget {
                let capped: String = output.chars().take(budget).collect();
                return capped;
            }
        }

        if output.chars().count() > budget {
            output.chars().take(budget).collect()
        } else {
            output
        }
    }

    fn layer_children_results(children: &[CompletedDepResult], budget: usize) -> String {
        let mut sorted: Vec<&CompletedDepResult> = children.iter().collect();
        sorted.sort_by_key(|item| std::cmp::Reverse(item.completed_at));

        let mut output = String::from("# Child Task Results\n\n");
        let per_diff_cap = std::cmp::max(budget / sorted.len(), 500);

        for child in sorted {
            let changed_files = if child.changed_files.is_empty() {
                "(none)".to_string()
            } else {
                child.changed_files.join(", ")
            };

            output.push_str(&format!("## [Child] {} ({})\n", child.title, child.task_id));
            output.push_str(&format!("Completed: {}\n", child.completed_at.to_rfc3339()));
            output.push_str(&format!("Summary: {}\n", child.summary));
            output.push_str(&format!("Changed files: {}\n", changed_files));

            if let Some(diff) = child.diff.as_ref() {
                let diff_truncated: String = diff.chars().take(per_diff_cap).collect();
                output.push_str("Diff (truncated):\n");
                output.push_str(&diff_truncated);
                output.push('\n');
                output.push_str("Full diff available via fs_read if needed.\n");
            }

            output.push('\n');
            if output.chars().count() > budget {
                let capped: String = output.chars().take(budget).collect();
                return capped;
            }
        }

        if output.chars().count() > budget {
            output.chars().take(budget).collect()
        } else {
            output
        }
    }

    fn layer_workspace_snapshot(snapshot: &WorkspaceSnapshot, cap: usize) -> String {
        let modified_count = snapshot
            .status_short
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count();
        let status_label = if modified_count == 0 {
            "clean".to_string()
        } else {
            format!("{modified_count} modified entries")
        };

        let text = format!(
            "# Workspace Snapshot\n\nBranch: {}\nStatus: {}\n\nRaw status (git status --short):\n{}\n\nRecent commits (git log --oneline -10):\n{}",
            snapshot.branch,
            status_label,
            snapshot.status_short,
            snapshot.recent_log
        );

        text.chars().take(cap).collect()
    }

    fn layer_siblings(siblings: &[SiblingTask], cap: usize) -> String {
        let mut text = String::from(
            "# Running Sibling Tasks\n\nPotential conflict warning: avoid editing claimed paths unless strictly necessary.\n",
        );

        for sibling in siblings {
            let claims = if sibling.claimed_paths.is_empty() {
                "(none)".to_string()
            } else {
                sibling.claimed_paths.join(", ")
            };
            text.push_str(&format!(
                "- {} ({})\n  Claimed paths: {}\n",
                sibling.title, sibling.task_id, claims
            ));
        }

        text.chars().take(cap).collect()
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};

    use super::{
        CompletedDepResult, SiblingTask, SystemPromptBuilder, TaskPromptContext, WorkspaceSnapshot,
    };
    use crate::model::{AcceptCriteria, Task, TaskStatus, VerifyCommand};

    fn make_task(title: &str, description: &str) -> Task {
        let now = Utc::now();
        Task {
            id: "task-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            root_session_id: String::new(),
            parent_id: None,
            title: title.to_string(),
            description: description.to_string(),
            status: TaskStatus::Pending,
            session_id: None,
            created_by: "tester".to_string(),
            priority: 1,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            result_diff: None,
            result_summary: None,
            result_files: None,
            result_verify: None,
            error: None,
            retry_count: 0,
            max_retries: 3,
            metadata: None,
        }
    }

    fn make_context() -> TaskPromptContext {
        TaskPromptContext {
            git_root: PathBuf::from("/tmp/repo"),
            task: make_task("Implement feature", "Detailed implementation spec."),
            accept_criteria: Some(AcceptCriteria {
                id: 1,
                task_id: "task-1".to_string(),
                description: "Feature compiles and tests pass.".to_string(),
                requires_human_approval: false,
            }),
            verify_commands: vec![VerifyCommand {
                id: 1,
                criteria_id: 1,
                command: "cargo test".to_string(),
                expected_exit_code: 0,
                timeout_secs: 120,
                seq: 0,
            }],
            dep_results: vec![],
            children_results: vec![],
            workspace_snapshot: WorkspaceSnapshot {
                branch: "master".to_string(),
                status_short: "".to_string(),
                recent_log: "abc123 feat: add x".to_string(),
            },
            running_siblings: vec![],
            conventions: None,
            repo_map: None,
        }
    }

    use std::path::PathBuf;

    #[test]
    fn test_build_includes_task_title() {
        let ctx = make_context();
        let prompt = SystemPromptBuilder::build(&ctx);

        assert!(prompt.contains("**Title**: Implement feature"));
    }

    #[test]
    fn test_build_includes_definition_of_done() {
        let ctx = make_context();
        let prompt = SystemPromptBuilder::build(&ctx);

        assert!(prompt.contains("## Definition of Done"));
        assert!(prompt.contains("Feature compiles and tests pass."));
    }

    #[test]
    fn test_build_conventions_capped_at_12000() {
        let mut ctx = make_context();
        let oversized = "a".repeat(13_500);
        ctx.conventions = Some(oversized);

        let prompt = SystemPromptBuilder::build(&ctx);
        let layer_start = prompt
            .find("# Project Conventions\n\n")
            .expect("should include conventions section");
        let from_layer = &prompt[layer_start + "# Project Conventions\n\n".len()..];
        let layer_end = from_layer
            .find("\n\n# Your Task")
            .unwrap_or(from_layer.len());
        let conventions_body = &from_layer[..layer_end];

        assert_eq!(conventions_body.chars().count(), 12_000);
    }

    #[test]
    fn test_build_dep_results_included() {
        let mut ctx = make_context();
        ctx.dep_results = vec![CompletedDepResult {
            task_id: "dep-1".to_string(),
            title: "Dependency Task".to_string(),
            completed_at: Utc::now() - Duration::minutes(5),
            summary: "Dependency done".to_string(),
            diff: Some("diff --git a/a.rs b/a.rs".to_string()),
            changed_files: vec!["a.rs".to_string()],
        }];

        let prompt = SystemPromptBuilder::build(&ctx);

        assert!(prompt.contains("# Dependency Results"));
        assert!(prompt.contains("Dependency Task"));
        assert!(prompt.contains("Dependency done"));
    }

    #[test]
    fn test_build_task_spec_never_truncated() {
        let mut ctx = make_context();
        ctx.conventions = Some("z".repeat(100_000));

        let prompt = SystemPromptBuilder::build(&ctx);

        assert!(prompt.contains("## Specification\nDetailed implementation spec."));
    }

    #[test]
    fn test_build_no_siblings_section_when_empty() {
        let ctx = make_context();
        let prompt = SystemPromptBuilder::build(&ctx);

        assert!(!prompt.contains("# Running Sibling Tasks"));
    }

    #[test]
    fn test_build_includes_siblings_when_present() {
        let mut ctx = make_context();
        ctx.running_siblings = vec![SiblingTask {
            task_id: "task-2".to_string(),
            title: "Concurrent Task".to_string(),
            claimed_paths: vec!["crates/sunny-tasks/src".to_string()],
        }];

        let prompt = SystemPromptBuilder::build(&ctx);

        assert!(prompt.contains("# Running Sibling Tasks"));
        assert!(prompt.contains("Concurrent Task"));
    }
}
