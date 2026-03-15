use std::sync::Arc;

use clap::Args;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use sunny_boys::{ExecutionOutcome, TaskExecutor};
use sunny_mind::AnthropicProvider;
use sunny_tasks::{
    TaskReadyEvent, TaskScheduler, TaskStatus, TaskStore, UserConfig, WorkspaceDetector,
};

#[derive(Args, Debug)]
pub struct WorkArgs {
    /// Maximum number of tasks to run concurrently
    #[arg(long, default_value_t = 3)]
    pub max_concurrent: usize,
}

pub async fn run(args: WorkArgs) -> anyhow::Result<()> {
    let git_root =
        WorkspaceDetector::detect_cwd().ok_or_else(|| anyhow::anyhow!("no git workspace found"))?;
    let git_root_str = git_root
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("workspace path not valid UTF-8"))?;

    let store = TaskStore::open_default()?;
    let workspace = store.find_or_create_workspace(git_root_str)?;
    let workspace_id = workspace.id.clone();

    // Startup recovery: re-queue suspended tasks whose children all completed
    recover_suspended_tasks(&store, &workspace_id);

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
    let scheduler = TaskScheduler::new(scheduler_store, workspace_id, max_concurrent)
        .with_ready_channel(ready_tx);

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
                            let task_store = match TaskStore::open_default() {
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
                                    check_parent_requeue(&task_store, &event.task.id);
                                }
                                ExecutionOutcome::Failed { error } => {
                                    warn!(task_id = %event.task.id, error = %error, "task failed");
                                    check_parent_requeue(&task_store, &event.task.id);
                                }
                                ExecutionOutcome::BlockedOnHuman => {
                                    info!(task_id = %event.task.id, "task blocked on human input");
                                }
                                ExecutionOutcome::BlockedOnCapability { request_id } => {
                                    info!(task_id = %event.task.id, request_id = %request_id, "task blocked on capability request");
                                }
                                ExecutionOutcome::Cancelled => {
                                    info!(task_id = %event.task.id, "task cancelled");
                                    check_parent_requeue(&task_store, &event.task.id);
                                }
                                ExecutionOutcome::MaxIterationsReached => {
                                    warn!(task_id = %event.task.id, "task hit max iterations");
                                }
                                ExecutionOutcome::NoTerminalAction => {
                                    handle_no_terminal_action(&task_store, &event.task.id);
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

/// Suspension detection: called when an agent ends without task_complete/task_fail.
/// - Has pending children → set Suspended (implicit suspension design)
/// - All children already terminal → set Pending (immediate re-queue)
/// - No children → genuine failure
fn handle_no_terminal_action(store: &TaskStore, task_id: &str) {
    let children = match store.list_children(task_id) {
        Ok(c) => c,
        Err(e) => {
            warn!(task_id = %task_id, error = %e, "failed to list children for suspension check");
            return;
        }
    };

    if children.is_empty() {
        // No children — genuinely failed without terminal action
        if let Err(e) = store.set_error(
            task_id,
            "agent ended without calling task_complete or task_fail",
        ) {
            warn!(task_id = %task_id, error = %e, "failed to set error on task");
        }
        if let Err(e) = store.update_status(task_id, TaskStatus::Failed) {
            warn!(task_id = %task_id, error = %e, "failed to mark task failed");
        }
        warn!(task_id = %task_id, "task failed: no terminal action and no children");
        return;
    }

    let all_terminal = children.iter().all(|c| {
        matches!(
            c.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        )
    });

    if all_terminal {
        // Children all done already — re-queue immediately
        if let Err(e) = store.update_status(task_id, TaskStatus::Pending) {
            warn!(task_id = %task_id, error = %e, "failed to re-queue task");
        }
        info!(task_id = %task_id, "children already complete, re-queuing parent");
        return;
    }

    // Check and enforce max suspension cap (5) via task metadata
    let task = match store.get_task(task_id) {
        Ok(Some(t)) => t,
        Ok(None) => {
            warn!(task_id = %task_id, "task not found during suspension check");
            return;
        }
        Err(e) => {
            warn!(task_id = %task_id, error = %e, "failed to load task for suspension check");
            return;
        }
    };

    let suspension_count = task
        .metadata
        .as_ref()
        .and_then(|m| m.get("suspension_count"))
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    if suspension_count >= 5 {
        // set_error sets status=failed + completed_at internally
        if let Err(e) = store.set_error(task_id, "max suspension count (5) exceeded") {
            warn!(task_id = %task_id, error = %e, "failed to mark task failed after cap");
        }
        warn!(task_id = %task_id, suspension_count, "task failed: max suspension count exceeded");
        return;
    }

    // Increment suspension_count and suspend
    let new_count = suspension_count + 1;
    let new_metadata = {
        let mut m = task.metadata.unwrap_or_else(|| serde_json::json!({}));
        m["suspension_count"] = serde_json::json!(new_count);
        m
    };
    if let Err(e) = store.update_metadata(task_id, new_metadata) {
        warn!(task_id = %task_id, error = %e, "failed to update suspension_count metadata");
    }
    if let Err(e) = store.update_status(task_id, TaskStatus::Suspended) {
        warn!(task_id = %task_id, error = %e, "failed to suspend task");
    }
    info!(task_id = %task_id, children = children.len(), suspension_count = new_count, "task suspended awaiting children");
}

/// Re-queue a suspended parent when all its children reach terminal state.
/// Called after every task reaches a terminal outcome.
fn check_parent_requeue(store: &TaskStore, child_task_id: &str) {
    let child = match store.get_task(child_task_id) {
        Ok(Some(t)) => t,
        _ => return,
    };
    let Some(ref parent_id) = child.parent_id else {
        return;
    };
    let parent = match store.get_task(parent_id) {
        Ok(Some(t)) => t,
        _ => return,
    };
    if parent.status != TaskStatus::Suspended {
        return;
    }
    match store.all_children_terminal(parent_id) {
        Ok(true) => {
            if let Err(e) = store.update_status(parent_id, TaskStatus::Pending) {
                warn!(parent_id = %parent_id, error = %e, "failed to re-queue suspended parent");
            } else {
                info!(parent_id = %parent_id, "re-queued suspended parent: all children terminal");
            }
        }
        Ok(false) => {}
        Err(e) => {
            warn!(parent_id = %parent_id, error = %e, "failed to check all_children_terminal");
        }
    }
}

/// Startup recovery: find suspended tasks whose children are all terminal and re-queue them.
pub fn recover_suspended_tasks(store: &TaskStore, workspace_id: &str) {
    let suspended = match store.list_tasks_by_status(workspace_id, TaskStatus::Suspended) {
        Ok(tasks) => tasks,
        Err(e) => {
            warn!(error = %e, "startup recovery: failed to list suspended tasks");
            return;
        }
    };
    for task in suspended {
        match store.all_children_terminal(&task.id) {
            Ok(true) => {
                if let Err(e) = store.update_status(&task.id, TaskStatus::Pending) {
                    warn!(task_id = %task.id, error = %e, "startup recovery: failed to re-queue");
                } else {
                    info!(task_id = %task.id, "startup recovery: re-queued stale suspended task");
                }
            }
            Ok(false) => {}
            Err(e) => {
                warn!(task_id = %task.id, error = %e, "startup recovery: all_children_terminal failed");
            }
        }
    }
}
