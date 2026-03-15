use std::sync::Arc;

use clap::{Parser, Subcommand};
use sunny_boys::{ExecutionOutcome, TaskExecutor};
use sunny_cli::commands::{ChatArgs, TasksArgs, WorkArgs};
use sunny_mind::{AnthropicProvider, LlmProvider};
use sunny_tasks::{TaskReadyEvent, TaskScheduler, TaskStore, UserConfig, WorkspaceDetector};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(name = "sunny")]
#[command(about = "Sunny — AI coding assistant")]
struct Cli {
    #[arg(long, global = true)]
    verbose: bool,

    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    chat: ChatArgs,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Authenticate with Anthropic (Claude Max subscription required).
    Login,
    /// Manage autonomous task records in the current workspace.
    Tasks(TasksArgs),
    /// Run the background worker daemon
    Work(WorkArgs),
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();

    let filter = if cli.verbose {
        tracing_subscriber::EnvFilter::new("debug")
    } else {
        tracing_subscriber::EnvFilter::new("warn")
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let result = match cli.command {
        Some(Command::Login) => sunny_cli::commands::login::run().await,
        Some(Command::Tasks(args)) => sunny_cli::commands::tasks::run(args).await,
        Some(Command::Work(args)) => sunny_cli::commands::work::run(args).await,
        None => {
            let scheduler_cancel = start_task_runtime(&cli.chat);
            let result = sunny_cli::commands::chat::run(cli.chat).await;
            if let Some(cancel) = scheduler_cancel {
                cancel.cancel();
            }
            result
        }
    };

    if let Err(e) = result {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

fn start_task_runtime(chat_args: &ChatArgs) -> Option<CancellationToken> {
    let git_root = WorkspaceDetector::detect_cwd()?;
    let store = TaskStore::open_default().ok()?;
    let git_root_str = git_root.to_str()?;
    let workspace = store.find_or_create_workspace(git_root_str).ok()?;

    if let Some(key) = chat_args.api_key.as_ref() {
        // SAFETY: applied at startup for current process auth configuration only.
        unsafe {
            std::env::set_var("ANTHROPIC_API_KEY", key);
        }
    }

    let provider = match AnthropicProvider::new(&chat_args.model) {
        Ok(provider) => provider,
        Err(error) => {
            warn!(error = %error, "failed to start background task runtime provider");
            return None;
        }
    };
    let provider: Arc<dyn LlmProvider> = Arc::new(provider);
    let executor = Arc::new(TaskExecutor::new(provider));

    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::unbounded_channel::<TaskReadyEvent>();
    let workspace_id = workspace.id.clone();
    let config = UserConfig::load(Some(&git_root));
    let max_concurrent = config.tasks.max_concurrent;

    let cancel = CancellationToken::new();
    let scheduler_cancel = cancel.clone();
    std::thread::spawn(move || {
        let store = match TaskStore::open_default() {
            Ok(store) => store,
            Err(error) => {
                warn!(error = %error, "failed to open task store for scheduler thread");
                return;
            }
        };
        #[allow(clippy::arc_with_non_send_sync)]
        let scheduler = TaskScheduler::new(Arc::new(store), workspace_id, max_concurrent)
            .with_ready_channel(ready_tx);

        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                warn!(error = %error, "failed to start scheduler runtime");
                return;
            }
        };
        runtime.block_on(scheduler.run(scheduler_cancel));
    });

    let launch_cancel = cancel.clone();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                warn!(error = %error, "failed to start task launcher runtime");
                return;
            }
        };

        runtime.block_on(async move {
            while let Some(event) = ready_rx.recv().await {
                let child_cancel = launch_cancel.child_token();
                let outcome = executor
                    .execute(event.task.clone(), git_root.clone(), child_cancel)
                    .await;
                match outcome {
                    ExecutionOutcome::Completed { summary } => {
                        info!(task_id = %event.task.id, summary = %summary, "task completed");
                    }
                    ExecutionOutcome::Failed { error } => {
                        warn!(task_id = %event.task.id, error = %error, "task failed");
                    }
                    ExecutionOutcome::BlockedOnHuman => {
                        info!(task_id = %event.task.id, "task blocked on human input");
                    }
                    ExecutionOutcome::BlockedOnCapability { request_id } => {
                        info!(
                            task_id = %event.task.id,
                            request_id = %request_id,
                            "task blocked on capability request"
                        );
                    }
                    ExecutionOutcome::Cancelled => {
                        info!(task_id = %event.task.id, "task cancelled");
                    }
                    ExecutionOutcome::MaxIterationsReached => {
                        warn!(task_id = %event.task.id, "task hit max iterations");
                    }
                    ExecutionOutcome::NoTerminalAction => {
                        info!(task_id = %event.task.id, "task completed with no terminal action");
                    }
                }
            }
        });
    });

    Some(cancel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parse_default_no_args() {
        let cli =
            Cli::try_parse_from(["sunny"]).expect("bare sunny should parse without subcommand");
        assert!(cli.command.is_none(), "no subcommand means chat mode");
    }

    #[test]
    fn test_cli_parse_with_model_flag() {
        let cli = Cli::try_parse_from(["sunny", "--model", "claude-3-5-sonnet"])
            .expect("sunny --model should parse");
        assert!(cli.command.is_none(), "--model should not set a subcommand");
    }

    #[test]
    fn test_cli_parse_with_api_key_flag() {
        let cli = Cli::try_parse_from(["sunny", "--api-key", "test-key"])
            .expect("sunny --api-key should parse");
        assert!(
            cli.command.is_none(),
            "--api-key should not set a subcommand"
        );
    }

    #[test]
    fn test_cli_parse_login_subcommand() {
        let cli = Cli::try_parse_from(["sunny", "login"])
            .expect("sunny login should parse as subcommand");
        assert!(
            matches!(cli.command, Some(Command::Login)),
            "login subcommand must be parsed as Command::Login"
        );
    }

    #[test]
    fn test_cli_parse_tasks_subcommand() {
        let cli = Cli::try_parse_from(["sunny", "tasks", "list"])
            .expect("sunny tasks list should parse as subcommand");
        assert!(
            matches!(cli.command, Some(Command::Tasks(_))),
            "tasks subcommand must be parsed as Command::Tasks"
        );
    }

    #[test]
    fn test_cli_parse_continue_flag() {
        let cli = Cli::try_parse_from(["sunny", "--continue"])
            .expect("sunny --continue should still work");
        assert!(
            cli.command.is_none(),
            "--continue should not set a subcommand"
        );
    }

    #[test]
    fn test_verbose_flag_default() {
        let cli = Cli::try_parse_from(["sunny"]).expect("bare sunny should parse");
        assert!(!cli.verbose, "verbose should default to false");
    }

    #[test]
    fn test_verbose_flag_enabled() {
        let cli =
            Cli::try_parse_from(["sunny", "--verbose"]).expect("sunny --verbose should parse");
        assert!(cli.verbose, "--verbose flag should set verbose to true");
    }
}
