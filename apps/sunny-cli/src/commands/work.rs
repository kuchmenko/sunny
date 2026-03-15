use std::sync::Arc;

use clap::Args;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use sunny_boys::{ExecutionOutcome, TaskExecutor};
use sunny_mind::AnthropicProvider;
use sunny_tasks::{TaskReadyEvent, TaskScheduler, TaskStore, UserConfig, WorkspaceDetector};

#[derive(Args, Debug)]
pub struct WorkArgs {
    /// Maximum number of tasks to run concurrently
    #[arg(long, default_value_t = 3)]
    pub max_concurrent: usize,
}

pub async fn run(args: WorkArgs) -> anyhow::Result<()> {
    let git_root = WorkspaceDetector::detect_cwd()
        .ok_or_else(|| anyhow::anyhow!("no git workspace found"))?;
    let git_root_str = git_root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("workspace path not valid UTF-8"))?;

    let store = TaskStore::open_default()?;
    let workspace = store.find_or_create_workspace(git_root_str)?;
    let workspace_id = workspace.id.clone();

    let config = UserConfig::load(Some(&git_root));
    let max_concurrent = if args.max_concurrent > 0 {
        args.max_concurrent
    } else {
        config.tasks.max_concurrent
    };

    let model = std::env::var("SUNNY_MODEL").unwrap_or_else(|_| "claude-sonnet-4-6".to_string());
    let provider = AnthropicProvider::new(&model)
        .map_err(|e| anyhow::anyhow!("failed to create LLM provider: {e}"))?;
    let provider: Arc<dyn sunny_mind::LlmProvider> = Arc::new(provider);

    let cancel = CancellationToken::new();
    let semaphore = Arc::new(Semaphore::new(max_concurrent));

    let (ready_tx, mut ready_rx) = tokio::sync::mpsc::unbounded_channel::<TaskReadyEvent>();

    #[allow(clippy::arc_with_non_send_sync)]
    let scheduler_store = Arc::new(TaskStore::open_default()?);
    let scheduler =
        TaskScheduler::new(scheduler_store, workspace_id, max_concurrent).with_ready_channel(ready_tx);

    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let scheduler_cancel = cancel.clone();
            let mut scheduler_task = tokio::task::spawn_local(async move {
                scheduler.run(scheduler_cancel).await;
            });

            info!(max_concurrent, git_root = ?git_root, "worker started");

            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        info!("received shutdown signal, stopping worker");
                        cancel.cancel();
                        break;
                    }
                    _ = cancel.cancelled() => {
                        break;
                    }
                    scheduler_result = &mut scheduler_task => {
                        if let Err(error) = scheduler_result {
                            warn!(error = %error, "scheduler task failed");
                        }
                        break;
                    }
                    event = ready_rx.recv() => {
                        let Some(event) = event else {
                            break;
                        };
                        let permit = semaphore.clone().acquire_owned().await?;
                        let task_cancel = cancel.child_token();
                        let provider = Arc::clone(&provider);
                        let git_root = git_root.clone();

                        tokio::task::spawn_local(async move {
                            let _permit = permit;
                            let _task_store = match TaskStore::open_default() {
                                Ok(store) => store,
                                Err(error) => {
                                    warn!(task_id = %event.task.id, error = %error, "failed to open task store for worker task");
                                    return;
                                }
                            };
                            let executor = TaskExecutor::new(provider);
                            let outcome = executor.execute(event.task.clone(), git_root, task_cancel).await;
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
                                    info!(task_id = %event.task.id, request_id = %request_id, "task blocked on capability request");
                                }
                                ExecutionOutcome::Cancelled => {
                                    info!(task_id = %event.task.id, "task cancelled");
                                }
                                ExecutionOutcome::MaxIterationsReached => {
                                    warn!(task_id = %event.task.id, "task hit max iterations");
                                }
                                ExecutionOutcome::NoTerminalAction => {
                                    info!(task_id = %event.task.id, "task ended without terminal action (suspension handled separately)");
                                }
                            }
                        });
                    }
                }
            }

            Ok::<(), anyhow::Error>(())
        })
        .await?;

    info!("worker stopped");
    Ok(())
}
