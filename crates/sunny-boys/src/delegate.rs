//! DelegateAgent for agent-to-agent task delegation.
//!
//! Routes tasks to other agents via the registry, with depth limiting
//! to prevent infinite recursion.

use std::collections::HashMap;
use std::sync::Arc;

use sunny_core::agent::{
    Agent, AgentContext, AgentCost, AgentError, AgentMessage, AgentMetadata, AgentMode,
    AgentResponse, Capability,
};
use sunny_core::orchestrator::AgentRegistry;

use crate::background::{BackgroundError, BackgroundTaskManager};

const DEFAULT_MAX_DEPTH: usize = 3;
const DELEGATE_TIMEOUT_SECS: u64 = 120;

/// Agent that delegates tasks to other agents via registry.
pub struct DelegateAgent {
    registry: Arc<AgentRegistry>,
    background: Arc<BackgroundTaskManager>,
    max_depth: usize,
    #[allow(dead_code)]
    metadata: AgentMetadata,
}

impl DelegateAgent {
    /// Creates a new DelegateAgent with the given registry and background manager.
    pub fn new(registry: Arc<AgentRegistry>, background: Arc<BackgroundTaskManager>) -> Self {
        Self {
            registry,
            background,
            max_depth: DEFAULT_MAX_DEPTH,
            metadata: AgentMetadata {
                mode: AgentMode::Subagent,
                category: "delegation",
                cost: AgentCost::Cheap,
            },
        }
    }

    /// Creates a new DelegateAgent with custom max depth.
    #[cfg(test)]
    pub fn with_max_depth(
        registry: Arc<AgentRegistry>,
        background: Arc<BackgroundTaskManager>,
        max_depth: usize,
    ) -> Self {
        Self {
            registry,
            background,
            max_depth,
            metadata: AgentMetadata {
                mode: AgentMode::Subagent,
                category: "delegation",
                cost: AgentCost::Cheap,
            },
        }
    }

    async fn delegate_sync(
        &self,
        target: &str,
        task: AgentMessage,
    ) -> Result<AgentResponse, AgentError> {
        let handle = self
            .registry
            .find(target)
            .ok_or_else(|| AgentError::ExecutionFailed {
                source: Box::new(std::io::Error::other(format!(
                    "delegate target '{}' not found",
                    target
                ))),
            })?;

        tokio::time::timeout(
            std::time::Duration::from_secs(DELEGATE_TIMEOUT_SECS),
            handle.send(task),
        )
        .await
        .map_err(|_| AgentError::Timeout)?
    }

    async fn delegate_async(
        &self,
        task_id: String,
        target: String,
        task: AgentMessage,
    ) -> Result<String, AgentError> {
        let registry = self.registry.clone();
        let background = self.background.clone();

        background
            .spawn(task_id.clone(), async move {
                let handle = registry
                    .find(&target)
                    .ok_or_else(|| AgentError::ExecutionFailed {
                        source: Box::new(std::io::Error::other(format!(
                            "delegate target '{}' not found",
                            target
                        ))),
                    })?;

                match handle.send(task).await {
                    Ok(AgentResponse::Success { content, .. }) => Ok(content),
                    Ok(AgentResponse::Error { code, message }) => {
                        Err(AgentError::ExecutionFailed {
                            source: Box::new(std::io::Error::other(format!(
                                "target agent error [{}]: {}",
                                code, message
                            ))),
                        })
                    }
                    Err(e) => Err(e),
                }
            })
            .await
            .map_err(|e| match e {
                BackgroundError::CapacityExceeded { .. } => AgentError::ExecutionFailed {
                    source: Box::new(std::io::Error::other("background task capacity exceeded")),
                },
                _ => AgentError::ExecutionFailed {
                    source: Box::new(std::io::Error::other(format!(
                        "background spawn failed: {}",
                        e
                    ))),
                },
            })?;

        Ok(task_id)
    }
}

#[async_trait::async_trait]
impl Agent for DelegateAgent {
    fn name(&self) -> &str {
        "delegate"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability("delegate".into())]
    }

    async fn handle_message(
        &self,
        msg: AgentMessage,
        ctx: &AgentContext,
    ) -> Result<AgentResponse, AgentError> {
        tracing::info!(agent = %ctx.agent_name, "DelegateAgent handling message");

        let (content, metadata) = match msg {
            AgentMessage::Task {
                content, metadata, ..
            } => (content, metadata),
        };

        let target = metadata
            .get("_sunny.delegate.target")
            .cloned()
            .ok_or_else(|| AgentError::ExecutionFailed {
                source: Box::new(std::io::Error::other(
                    "missing '_sunny.delegate.target' in metadata",
                )),
            })?;

        let depth: usize = metadata
            .get("_sunny.delegate.depth")
            .and_then(|d| d.parse().ok())
            .unwrap_or(0);

        if depth >= self.max_depth {
            return Ok(AgentResponse::Error {
                code: "DEPTH_LIMIT_EXCEEDED".into(),
                message: format!("delegate depth limit ({}) reached", self.max_depth),
            });
        }

        let is_background = metadata
            .get("_sunny.delegate.background")
            .map(|v| v == "true")
            .unwrap_or(false);

        let mut forwarded_metadata = metadata.clone();
        forwarded_metadata.insert("_sunny.delegate.depth".into(), (depth + 1).to_string());
        forwarded_metadata.remove("_sunny.delegate.background");

        let task = AgentMessage::Task {
            id: format!("delegate-{}-{}", ctx.agent_name, uuid::Uuid::new_v4()),
            content,
            metadata: forwarded_metadata,
        };

        if is_background {
            let task_id = format!("delegate-{}", uuid::Uuid::new_v4());
            match self
                .delegate_async(task_id.clone(), target.clone(), task)
                .await
            {
                Ok(_) => {
                    let mut resp_metadata = HashMap::new();
                    resp_metadata.insert("task_id".into(), task_id.clone());
                    resp_metadata.insert("mode".into(), "async".into());

                    Ok(AgentResponse::Success {
                        content: format!("delegated to '{}' (task_id: {})", target, task_id),
                        metadata: resp_metadata,
                    })
                }
                Err(e) => Err(e),
            }
        } else {
            match self.delegate_sync(&target, task).await {
                Ok(response) => {
                    tracing::info!(
                        agent = %ctx.agent_name,
                        target = %target,
                        "DelegateAgent completed sync delegation"
                    );
                    Ok(response)
                }
                Err(e) => {
                    tracing::error!(
                        agent = %ctx.agent_name,
                        target = %target,
                        error = %e,
                        "DelegateAgent failed to route"
                    );
                    Err(e)
                }
            }
        }
    }
}

#[cfg(test)]
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    use sunny_core::agent::{
        Agent, AgentContext, AgentError, AgentHandle, AgentMessage, AgentResponse, Capability,
    };
    use sunny_core::orchestrator::AgentRegistry;
    use tokio_util::sync::CancellationToken;

    struct EchoAgent;

    #[async_trait::async_trait]
    impl Agent for EchoAgent {
        fn name(&self) -> &str {
            "echo"
        }

        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability("echo".into())]
        }

        async fn handle_message(
            &self,
            msg: AgentMessage,
            _ctx: &AgentContext,
        ) -> Result<AgentResponse, AgentError> {
            let content = match msg {
                AgentMessage::Task { content, .. } => content,
            };
            Ok(AgentResponse::Success {
                content,
                metadata: HashMap::new(),
            })
        }
    }

    struct ErrorAgent;

    #[async_trait::async_trait]
    impl Agent for ErrorAgent {
        fn name(&self) -> &str {
            "error"
        }

        fn capabilities(&self) -> Vec<Capability> {
            vec![]
        }

        async fn handle_message(
            &self,
            _msg: AgentMessage,
            _ctx: &AgentContext,
        ) -> Result<AgentResponse, AgentError> {
            Ok(AgentResponse::Error {
                code: "TEST_ERROR".into(),
                message: "test error".into(),
            })
        }
    }

    fn mk_ctx() -> AgentContext {
        AgentContext {
            agent_name: "test-delegate".into(),
        }
    }

    fn mk_delegate_msg(target: &str, content: &str, depth: usize) -> AgentMessage {
        let mut metadata = HashMap::new();
        metadata.insert("_sunny.delegate.target".into(), target.into());
        metadata.insert("_sunny.delegate.depth".into(), depth.to_string());
        AgentMessage::Task {
            id: "task-1".into(),
            content: content.into(),
            metadata,
        }
    }

    #[tokio::test]
    async fn test_delegate_routes_to_named_agent() {
        let mut registry = AgentRegistry::new();
        let handle = AgentHandle::spawn(Arc::new(EchoAgent), CancellationToken::new());
        registry.register("echo".into(), handle, vec![]).unwrap();

        let registry = Arc::new(registry);
        let background = Arc::new(BackgroundTaskManager::new(10, CancellationToken::new()));
        let delegate = DelegateAgent::new(registry, background);

        let msg = mk_delegate_msg("echo", "hello", 0);
        let response = delegate.handle_message(msg, &mk_ctx()).await.unwrap();

        match response {
            AgentResponse::Success { content, .. } => {
                assert_eq!(content, "hello");
            }
            _ => panic!("expected success"),
        }
    }

    #[tokio::test]
    async fn test_delegate_target_not_found() {
        let registry = Arc::new(AgentRegistry::new());
        let background = Arc::new(BackgroundTaskManager::new(10, CancellationToken::new()));
        let delegate = DelegateAgent::new(registry, background);

        let msg = mk_delegate_msg("nonexistent", "hello", 0);
        let result = delegate.handle_message(msg, &mk_ctx()).await;

        assert!(result.is_err(), "should fail for non-existent target");
    }

    #[tokio::test]
    async fn test_delegate_depth_limit() {
        let registry = Arc::new(AgentRegistry::new());
        let background = Arc::new(BackgroundTaskManager::new(10, CancellationToken::new()));
        let delegate = DelegateAgent::with_max_depth(registry, background, 2);

        let msg = mk_delegate_msg("any", "hello", 2);
        let response = delegate.handle_message(msg, &mk_ctx()).await.unwrap();

        match response {
            AgentResponse::Error { code, .. } => {
                assert_eq!(code, "DEPTH_LIMIT_EXCEEDED");
            }
            _ => panic!("expected error for depth limit"),
        }
    }

    #[tokio::test]
    async fn test_delegate_target_returns_error() {
        let mut registry = AgentRegistry::new();
        let handle = AgentHandle::spawn(Arc::new(ErrorAgent), CancellationToken::new());
        registry.register("error".into(), handle, vec![]).unwrap();

        let registry = Arc::new(registry);
        let background = Arc::new(BackgroundTaskManager::new(10, CancellationToken::new()));
        let delegate = DelegateAgent::new(registry, background);

        let msg = mk_delegate_msg("error", "hello", 0);
        let response = delegate.handle_message(msg, &mk_ctx()).await.unwrap();

        match response {
            AgentResponse::Error { code, .. } => {
                assert_eq!(code, "TEST_ERROR");
            }
            _ => panic!("expected error from target"),
        }
    }

    #[tokio::test]
    async fn test_delegate_missing_target_metadata() {
        let registry = Arc::new(AgentRegistry::new());
        let background = Arc::new(BackgroundTaskManager::new(10, CancellationToken::new()));
        let delegate = DelegateAgent::new(registry, background);

        let msg = AgentMessage::Task {
            id: "task-1".into(),
            content: "hello".into(),
            metadata: HashMap::new(),
        };
        let result = delegate.handle_message(msg, &mk_ctx()).await;

        assert!(result.is_err(), "should fail without target metadata");
    }

    #[tokio::test]
    async fn test_delegate_async_mode() {
        let mut registry = AgentRegistry::new();
        let handle = AgentHandle::spawn(Arc::new(EchoAgent), CancellationToken::new());
        registry.register("echo".into(), handle, vec![]).unwrap();

        let registry = Arc::new(registry);
        let background = Arc::new(BackgroundTaskManager::new(10, CancellationToken::new()));
        let delegate = DelegateAgent::new(registry, background);

        let mut metadata = HashMap::new();
        metadata.insert("_sunny.delegate.target".into(), "echo".into());
        metadata.insert("_sunny.delegate.depth".into(), "0".into());
        metadata.insert("_sunny.delegate.background".into(), "true".into());

        let msg = AgentMessage::Task {
            id: "task-1".into(),
            content: "hello".into(),
            metadata,
        };

        let response = delegate.handle_message(msg, &mk_ctx()).await.unwrap();

        match response {
            AgentResponse::Success { content, metadata } => {
                assert!(content.contains("echo"));
                assert!(content.contains("task_id"));
                assert_eq!(metadata.get("mode"), Some(&"async".into()));
            }
            _ => panic!("expected success"),
        }
    }
}
