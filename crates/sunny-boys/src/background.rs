use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use sunny_core::agent::AgentError;
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

pub type TaskId = String;

pub struct BackgroundTaskManager {
    tasks: Arc<Mutex<HashMap<TaskId, TaskHandle>>>,
    max_concurrent: usize,
    cancel: CancellationToken,
}

pub struct TaskHandle {
    join_handle: JoinHandle<Result<String, AgentError>>,
    cancel_token: CancellationToken,
    status: Arc<RwLock<TaskStatus>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Completed(String),
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskResult {
    pub task_id: TaskId,
    pub status: TaskStatus,
}

#[derive(thiserror::Error, Debug, Clone, PartialEq, Eq)]
pub enum BackgroundError {
    #[error("task capacity exceeded: max={max}")]
    CapacityExceeded { max: usize },
    #[error("task not found: {task_id}")]
    TaskNotFound { task_id: TaskId },
    #[error("task failed: {message}")]
    TaskFailed { message: String },
    #[error("task timed out")]
    Timeout,
}

impl BackgroundTaskManager {
    pub fn new(max_concurrent: usize, cancel: CancellationToken) -> Self {
        Self {
            tasks: Arc::new(Mutex::new(HashMap::new())),
            max_concurrent,
            cancel,
        }
    }

    pub async fn spawn<F>(&self, task_id: TaskId, future: F) -> Result<(), BackgroundError>
    where
        F: Future<Output = Result<String, AgentError>> + Send + 'static,
    {
        let mut tasks = self.tasks.lock().await;

        if tasks.len() >= self.max_concurrent {
            return Err(BackgroundError::CapacityExceeded {
                max: self.max_concurrent,
            });
        }

        if tasks.contains_key(&task_id) {
            return Err(BackgroundError::TaskFailed {
                message: format!("task already exists: {task_id}"),
            });
        }

        let cancel_token = self.cancel.child_token();
        let task_status = Arc::new(RwLock::new(TaskStatus::Running));
        let task_status_clone = Arc::clone(&task_status);
        let task_cancel_token = cancel_token.clone();

        let join_handle = tokio::spawn(async move {
            tokio::select! {
                _ = task_cancel_token.cancelled() => {
                    *task_status_clone.write().await = TaskStatus::Cancelled;
                    Err(AgentError::ExecutionFailed {
                        source: Box::new(std::io::Error::other("task cancelled")),
                    })
                }
                result = future => {
                    match result {
                        Ok(output) => {
                            *task_status_clone.write().await = TaskStatus::Completed(output.clone());
                            Ok(output)
                        }
                        Err(err) => {
                            *task_status_clone.write().await = TaskStatus::Failed(err.to_string());
                            Err(err)
                        }
                    }
                }
            }
        });

        tasks.insert(
            task_id,
            TaskHandle {
                join_handle,
                cancel_token,
                status: task_status,
            },
        );

        Ok(())
    }

    pub async fn cancel(&self, task_id: &str) -> Result<(), BackgroundError> {
        let (cancel_token, status) = {
            let tasks = self.tasks.lock().await;
            let task = tasks
                .get(task_id)
                .ok_or_else(|| BackgroundError::TaskNotFound {
                    task_id: task_id.to_string(),
                })?;
            (task.cancel_token.clone(), Arc::clone(&task.status))
        };

        cancel_token.cancel();
        *status.write().await = TaskStatus::Cancelled;

        Ok(())
    }

    pub async fn collect(&self, task_id: &str) -> Result<TaskResult, BackgroundError> {
        const COLLECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

        let task = {
            let mut tasks = self.tasks.lock().await;
            tasks
                .remove(task_id)
                .ok_or_else(|| BackgroundError::TaskNotFound {
                    task_id: task_id.to_string(),
                })?
        };

        let task_id = task_id.to_string();
        let status = Arc::clone(&task.status);

        let join_outcome = timeout(COLLECT_TIMEOUT, task.join_handle)
            .await
            .map_err(|_| BackgroundError::Timeout)?;

        match join_outcome {
            Ok(Ok(output)) => Ok(TaskResult {
                task_id,
                status: TaskStatus::Completed(output),
            }),
            Ok(Err(err)) => {
                let current_status = status.read().await.clone();
                match current_status {
                    TaskStatus::Cancelled => Ok(TaskResult {
                        task_id,
                        status: TaskStatus::Cancelled,
                    }),
                    TaskStatus::Failed(message) => Ok(TaskResult {
                        task_id,
                        status: TaskStatus::Failed(message),
                    }),
                    TaskStatus::Completed(output) => Ok(TaskResult {
                        task_id,
                        status: TaskStatus::Completed(output),
                    }),
                    TaskStatus::Running => {
                        let message = err.to_string();
                        *status.write().await = TaskStatus::Failed(message.clone());
                        Ok(TaskResult {
                            task_id,
                            status: TaskStatus::Failed(message),
                        })
                    }
                }
            }
            Err(join_err) => {
                let message = format!("task join failed: {join_err}");
                *status.write().await = TaskStatus::Failed(message.clone());
                Err(BackgroundError::TaskFailed { message })
            }
        }
    }

    pub async fn collect_all(&self) -> Vec<TaskResult> {
        let task_ids = {
            let tasks = self.tasks.lock().await;
            tasks.keys().cloned().collect::<Vec<_>>()
        };

        let mut results = Vec::with_capacity(task_ids.len());

        for task_id in task_ids {
            match self.collect(&task_id).await {
                Ok(task_result) => results.push(task_result),
                Err(BackgroundError::TaskNotFound { .. }) => {}
                Err(BackgroundError::Timeout) => results.push(TaskResult {
                    task_id,
                    status: TaskStatus::Failed("task timed out during collect".to_string()),
                }),
                Err(BackgroundError::TaskFailed { message }) => results.push(TaskResult {
                    task_id,
                    status: TaskStatus::Failed(message),
                }),
                Err(BackgroundError::CapacityExceeded { max }) => results.push(TaskResult {
                    task_id,
                    status: TaskStatus::Failed(format!("capacity exceeded: {max}")),
                }),
            }
        }

        results
    }

    pub async fn status(&self, task_id: &str) -> Option<TaskStatus> {
        let status = {
            let tasks = self.tasks.lock().await;
            tasks.get(task_id).map(|task| Arc::clone(&task.status))
        }?;

        let current = status.read().await.clone();
        Some(current)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::sleep;
    use tokio_util::sync::CancellationToken;

    use super::{BackgroundError, BackgroundTaskManager, TaskStatus};

    #[tokio::test]
    async fn test_spawn_and_collect_result() {
        let manager = BackgroundTaskManager::new(4, CancellationToken::new());

        manager
            .spawn("task-1".to_string(), async { Ok("done".to_string()) })
            .await
            .expect("spawn should succeed");

        let result = manager
            .collect("task-1")
            .await
            .expect("collect should succeed");
        assert_eq!(result.task_id, "task-1");
        assert_eq!(result.status, TaskStatus::Completed("done".to_string()));
    }

    #[tokio::test]
    async fn test_cancel_running_task() {
        let manager = BackgroundTaskManager::new(2, CancellationToken::new());

        manager
            .spawn("task-cancel".to_string(), async {
                sleep(Duration::from_secs(60)).await;
                Ok("late".to_string())
            })
            .await
            .expect("spawn should succeed");

        manager
            .cancel("task-cancel")
            .await
            .expect("cancel should succeed");

        let result = manager
            .collect("task-cancel")
            .await
            .expect("collect should return cancelled status");
        assert_eq!(result.status, TaskStatus::Cancelled);
    }

    #[tokio::test]
    async fn test_multiple_concurrent_tasks() {
        let manager = BackgroundTaskManager::new(8, CancellationToken::new());

        for idx in 0..5 {
            manager
                .spawn(
                    format!("task-{idx}"),
                    async move { Ok(format!("ok-{idx}")) },
                )
                .await
                .expect("spawn should succeed");
        }

        let mut completed = 0;
        for idx in 0..5 {
            let result = manager
                .collect(&format!("task-{idx}"))
                .await
                .expect("collect should succeed");
            if matches!(result.status, TaskStatus::Completed(_)) {
                completed += 1;
            }
        }

        assert_eq!(completed, 5);
    }

    #[tokio::test]
    async fn test_task_panic_handled_gracefully() {
        let manager = BackgroundTaskManager::new(2, CancellationToken::new());

        manager
            .spawn("task-panic".to_string(), async {
                panic!("boom");
                #[allow(unreachable_code)]
                Ok("never".to_string())
            })
            .await
            .expect("spawn should succeed");

        let err = manager
            .collect("task-panic")
            .await
            .expect_err("panic should not crash runtime and must return error");
        assert!(matches!(err, BackgroundError::TaskFailed { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn test_timeout_on_slow_task() {
        let manager = BackgroundTaskManager::new(2, CancellationToken::new());

        manager
            .spawn("task-timeout".to_string(), async {
                sleep(Duration::from_secs(31)).await;
                Ok("slow".to_string())
            })
            .await
            .expect("spawn should succeed");

        let (collect_result, _) =
            tokio::join!(async { manager.collect("task-timeout").await }, async {
                tokio::task::yield_now().await;
                tokio::time::advance(Duration::from_secs(30) + Duration::from_millis(1)).await;
            });

        let result = collect_result.expect_err("collect should return timeout");
        assert_eq!(result, BackgroundError::Timeout);
    }

    #[tokio::test]
    async fn test_bounded_capacity() {
        let manager = BackgroundTaskManager::new(1, CancellationToken::new());

        manager
            .spawn("task-1".to_string(), async {
                sleep(Duration::from_millis(200)).await;
                Ok("ok".to_string())
            })
            .await
            .expect("first spawn should succeed");

        let err = manager
            .spawn("task-2".to_string(), async { Ok("ok2".to_string()) })
            .await
            .expect_err("second spawn should exceed capacity");
        assert_eq!(err, BackgroundError::CapacityExceeded { max: 1 });
    }
}
