use std::io::{self, BufRead, Write};

use clap::{Args, Subcommand};
use sunny_tasks::{
    capability_info, CapabilityPolicyEntry, CapabilityRisk, CapabilityScope, CapabilityStore, Task,
    TaskStore, WorkspaceDetector,
};

#[derive(Args, Debug)]
pub struct TasksArgs {
    #[command(subcommand)]
    pub command: TasksCommand,
}

#[derive(Subcommand, Debug)]
pub enum TasksCommand {
    /// List all tasks for the current workspace
    List {
        /// Filter by status (pending, running, completed, failed, cancelled)
        #[arg(long)]
        status: Option<String>,
    },
    /// Show a task tree with hierarchy
    Tree,
    /// Show details of a specific task
    Show {
        /// Task ID
        id: String,
    },
    /// Show and answer pending human questions
    Answer,
    /// Manage capability permissions
    Permissions {
        #[command(subcommand)]
        command: PermissionsCommand,
    },
}

#[derive(Subcommand, Debug)]
pub enum PermissionsCommand {
    /// Show audit log of capability requests
    Log {
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Revoke a workspace-level capability
    Revoke {
        /// Capability name (e.g. shell_pipes)
        capability: String,
    },
}

pub async fn run(args: TasksArgs) -> anyhow::Result<()> {
    let store = TaskStore::open_default()?;
    let git_root = WorkspaceDetector::detect_cwd()
        .ok_or_else(|| anyhow::anyhow!("no git workspace found in current directory or parents"))?;
    let git_root_str = git_root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("workspace path is not valid UTF-8"))?;
    let workspace = store.find_or_create_workspace(git_root_str)?;

    match args.command {
        TasksCommand::List { status } => {
            let tasks = store.list_tasks(&workspace.id)?;
            let tasks: Vec<_> = if let Some(filter) = status {
                let normalized = filter.to_lowercase();
                tasks
                    .into_iter()
                    .filter(|task| task.status.to_string() == normalized)
                    .collect()
            } else {
                tasks
            };

            if tasks.is_empty() {
                println!("No tasks found.");
            } else {
                for task in &tasks {
                    let short = task.id.chars().take(8).collect::<String>();
                    println!("[{}] {} - {}", task.status, short, task.title);
                }
            }
        }
        TasksCommand::Tree => {
            let tasks = store.list_tasks(&workspace.id)?;
            if tasks.is_empty() {
                println!("No tasks found.");
            } else {
                print_tree(&tasks, None, 0);
            }
        }
        TasksCommand::Show { id } => match store.get_task(&id)? {
            None => println!("Task not found: {id}"),
            Some(task) => {
                println!("ID:      {}", task.id);
                println!("Title:   {}", task.title);
                println!("Status:  {}", task.status);
                println!("Created: {}", task.created_at);
                println!("\nDescription:\n{}", task.description);

                if let Some(summary) = task.result_summary {
                    println!("\nResult:\n{summary}");
                }
                if let Some(error) = task.error {
                    println!("\nError:\n{error}");
                }
            }
        },
        TasksCommand::Answer => {
            let questions = store.pending_questions()?;
            for question in &questions {
                let short_task_id = question.task_id.chars().take(8).collect::<String>();
                println!("\n[Question] Task: {short_task_id}");
                println!("{}", question.question);

                if let Some(context) = question.context.as_deref() {
                    println!("Context: {context}");
                }

                if let Some(options) = question.options.as_ref() {
                    for (index, option) in options.iter().enumerate() {
                        println!("  {}) {}", index + 1, option);
                    }
                }

                print!("Answer: ");
                io::stdout().flush()?;

                let mut line = String::new();
                io::stdin().lock().read_line(&mut line)?;
                let answer = line.trim().to_string();
                if !answer.is_empty() {
                    store.answer_question(&question.id, &answer)?;
                    println!("Recorded.");
                }
            }

            let cap_store = CapabilityStore::open_default()?;
            let cap_requests = cap_store.pending_requests()?;
            for req in &cap_requests {
                let risk =
                    capability_info(&req.capability).map_or("unknown", |info| match info.risk {
                        CapabilityRisk::Low => "low",
                        CapabilityRisk::Medium => "medium",
                        CapabilityRisk::HardBlocked => "hard-blocked",
                    });

                println!("\n[Permission] capability={} risk={risk}", req.capability);
                if let Some(command) = req.example_command.as_deref() {
                    println!("Command: {command}");
                }
                if let Some(rhs) = req.requested_rhs.as_ref() {
                    println!("Requested pipe targets: {}", rhs.join(", "));
                }
                println!("Reason: {}", req.reason);
                println!(
                    "Approve scope: [1] invocation [2] session [3] workspace [4] global [n] deny"
                );
                print!("Choice: ");
                io::stdout().flush()?;

                let mut line = String::new();
                io::stdin().lock().read_line(&mut line)?;
                match line.trim() {
                    "1" => {
                        cap_store.approve(&req.id, CapabilityScope::Invocation)?;
                        println!("Approved (invocation).");
                    }
                    "2" => {
                        cap_store.approve(&req.id, CapabilityScope::Session)?;
                        println!("Approved (session).");
                    }
                    "3" => {
                        cap_store.approve(&req.id, CapabilityScope::Workspace)?;
                        let mut policy =
                            sunny_tasks::PolicyFile::load(&git_root).unwrap_or_default();
                        policy.set_capability(
                            &git_root,
                            &req.capability,
                            CapabilityPolicyEntry {
                                policy: "workspace".to_string(),
                                allowed_rhs: req.requested_rhs.clone(),
                                allowed_ops: None,
                                added_at: chrono::Utc::now().to_rfc3339(),
                            },
                        )?;
                        println!("Approved (workspace) and saved.");
                    }
                    "4" => {
                        cap_store.approve(&req.id, CapabilityScope::Global)?;
                        println!("Approved (global).");
                    }
                    _ => {
                        cap_store.deny(&req.id)?;
                        println!("Denied.");
                    }
                }
            }

            if questions.is_empty() && cap_requests.is_empty() {
                println!("No pending questions or permission requests.");
            }
        }
        TasksCommand::Permissions { command } => {
            let cap_store = CapabilityStore::open_default()?;
            match command {
                PermissionsCommand::Log { limit } => {
                    let rows = cap_store.audit_log(Some(limit))?;
                    if rows.is_empty() {
                        println!("No capability request audit rows.");
                    } else {
                        println!(
                            "{:<10} {:<18} {:<10} {:<10} {:<20}",
                            "status", "capability", "scope", "task", "requested_at"
                        );
                        for row in rows {
                            let task = row
                                .task_id
                                .as_deref()
                                .map(|id| id.chars().take(8).collect::<String>())
                                .unwrap_or_else(|| "-".to_string());
                            let scope = row
                                .scope
                                .map(|scope| scope.to_string())
                                .unwrap_or_else(|| "-".to_string());
                            println!(
                                "{:<10} {:<18} {:<10} {:<10} {:<20}",
                                row.status,
                                row.capability,
                                scope,
                                task,
                                row.requested_at.format("%Y-%m-%d %H:%M:%S")
                            );
                        }
                    }
                }
                PermissionsCommand::Revoke { capability } => {
                    let mut policy = sunny_tasks::PolicyFile::load(&git_root)?;
                    let removed = policy.revoke(&git_root, &capability)?;
                    if removed {
                        println!("Revoked workspace permission: {capability}");
                    } else {
                        println!("Capability not found in workspace policy: {capability}");
                    }
                }
            }
        }
    }

    Ok(())
}

fn print_tree(tasks: &[Task], parent_id: Option<&str>, depth: usize) {
    let indent = "  ".repeat(depth);
    let children: Vec<_> = tasks
        .iter()
        .filter(|task| task.parent_id.as_deref() == parent_id)
        .collect();

    for task in children {
        let short = task.id.chars().take(8).collect::<String>();
        println!("{}[{}] {} - {}", indent, task.status, short, task.title);
        print_tree(tasks, Some(&task.id), depth + 1);
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{PermissionsCommand, TasksArgs, TasksCommand};

    #[derive(Parser, Debug)]
    struct TestCli {
        #[command(flatten)]
        args: TasksArgs,
    }

    #[test]
    fn test_parse_tasks_list() {
        let parsed = TestCli::try_parse_from(["sunny", "list", "--status", "running"])
            .expect("tasks list should parse");

        match parsed.args.command {
            TasksCommand::List { status } => assert_eq!(status.as_deref(), Some("running")),
            _ => panic!("expected list subcommand"),
        }
    }

    #[test]
    fn test_parse_tasks_tree() {
        let parsed = TestCli::try_parse_from(["sunny", "tree"]).expect("tasks tree should parse");

        assert!(matches!(parsed.args.command, TasksCommand::Tree));
    }

    #[test]
    fn test_parse_tasks_show() {
        let parsed = TestCli::try_parse_from(["sunny", "show", "task-123"])
            .expect("tasks show should parse");

        match parsed.args.command {
            TasksCommand::Show { id } => assert_eq!(id, "task-123"),
            _ => panic!("expected show subcommand"),
        }
    }

    #[test]
    fn test_parse_tasks_answer() {
        let parsed =
            TestCli::try_parse_from(["sunny", "answer"]).expect("tasks answer should parse");

        assert!(matches!(parsed.args.command, TasksCommand::Answer));
    }

    #[test]
    fn test_parse_permissions_log() {
        let parsed = TestCli::try_parse_from(["sunny", "permissions", "log", "--limit", "10"])
            .expect("permissions log should parse");

        match parsed.args.command {
            TasksCommand::Permissions { command } => match command {
                PermissionsCommand::Log { limit } => assert_eq!(limit, 10),
                _ => panic!("expected permissions log"),
            },
            _ => panic!("expected permissions command"),
        }
    }

    #[test]
    fn test_parse_permissions_revoke() {
        let parsed = TestCli::try_parse_from(["sunny", "permissions", "revoke", "shell_pipes"])
            .expect("permissions revoke should parse");

        match parsed.args.command {
            TasksCommand::Permissions { command } => match command {
                PermissionsCommand::Revoke { capability } => assert_eq!(capability, "shell_pipes"),
                _ => panic!("expected permissions revoke"),
            },
            _ => panic!("expected permissions command"),
        }
    }
}
