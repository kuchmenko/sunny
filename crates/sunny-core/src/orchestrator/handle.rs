use std::error::Error;

use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentError, AgentMessage, AgentResponse};

use super::{AgentRegistry, OrchestratorError};

pub(crate) enum OrchestratorMsg {
    Dispatch {
        agent_name: String,
        msg: AgentMessage,
        reply: oneshot::Sender<Result<AgentResponse, OrchestratorError>>,
    },
}

pub struct OrchestratorHandle {
    tx: mpsc::Sender<OrchestratorMsg>,
    join_handle: tokio::task::JoinHandle<()>,
}

impl OrchestratorHandle {
    pub fn spawn(registry: AgentRegistry, cancellation_token: CancellationToken) -> Self {
        let (tx, rx) = mpsc::channel(32);

        let join_handle = tokio::spawn(run_orchestrator(registry, rx, cancellation_token));

        Self { tx, join_handle }
    }

    pub async fn dispatch(
        &self,
        agent_name: &str,
        msg: AgentMessage,
    ) -> Result<AgentResponse, OrchestratorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let orchestrator_msg = OrchestratorMsg::Dispatch {
            agent_name: agent_name.to_string(),
            msg,
            reply: reply_tx,
        };

        timeout(std::time::Duration::from_secs(30), self.tx.send(orchestrator_msg))
            .await
            .map_err(|_| OrchestratorError::AgentUnresponsive)?
            .map_err(channel_closed_error)?;

        timeout(std::time::Duration::from_secs(30), reply_rx)
            .await
            .map_err(|_| OrchestratorError::AgentUnresponsive)?
            .map_err(channel_closed_error)?
    }

    pub async fn shutdown(self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        drop(self.tx);
        self.join_handle.await?;
        Ok(())
    }
}

pub(crate) async fn run_orchestrator(
    registry: AgentRegistry,
    mut rx: mpsc::Receiver<OrchestratorMsg>,
    cancellation_token: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancellation_token.cancelled() => {
                break;
            }
            maybe_msg = rx.recv() => {
                match maybe_msg {
                    Some(OrchestratorMsg::Dispatch { agent_name, msg, reply }) => {
                        let result = match registry.find(&agent_name) {
                            Some(agent_handle) => agent_handle.send(msg).await.map_err(map_agent_error),
                            None => Err(OrchestratorError::AgentNotFound { name: agent_name }),
                        };
                        let _ = reply.send(result);
                    }
                    None => {
                        break;
                    }
                }
            }
        }
    }
}

fn map_agent_error(err: AgentError) -> OrchestratorError {
    match err {
        AgentError::Timeout | AgentError::ExecutionFailed { .. } => {
            OrchestratorError::AgentUnresponsive
        }
        other => OrchestratorError::DispatchFailed { source: other },
    }
}

fn channel_closed_error(_err: impl Error + Send + Sync + 'static) -> OrchestratorError {
    OrchestratorError::ShuttingDown
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use tokio_util::sync::CancellationToken;

    use crate::agent::{AgentHandle, AgentMessage, AgentResponse, Capability, EchoAgent};

    use super::{OrchestratorError, OrchestratorHandle};
    use crate::orchestrator::AgentRegistry;

    #[tokio::test]
    async fn test_orchestrator_dispatch_to_echo_agent() {
        let cancellation_token = CancellationToken::new();
        let agent_handle = AgentHandle::spawn(
            Arc::new(EchoAgent),
            cancellation_token.child_token(),
        );
        let mut registry = AgentRegistry::new();
        let register_result = registry.register(
            "echo".to_string(),
            agent_handle,
            vec![Capability("echo".to_string())],
        );
        assert!(register_result.is_ok());

        let orchestrator = OrchestratorHandle::spawn(registry, cancellation_token.child_token());

        let mut metadata = HashMap::new();
        metadata.insert("k".to_string(), "v".to_string());
        let dispatch_result = orchestrator
            .dispatch(
                "echo",
                AgentMessage::Task {
                    id: "task-1".to_string(),
                    content: "hello".to_string(),
                    metadata,
                },
            )
            .await;

        match dispatch_result {
            Ok(AgentResponse::Success {
                content,
                metadata: response_metadata,
            }) => {
                assert_eq!(content, "hello");
                assert_eq!(response_metadata.get("k"), Some(&"v".to_string()));
            }
            Ok(AgentResponse::Error { code, message }) => {
                panic!("unexpected error response {code}: {message}");
            }
            Err(err) => {
                panic!("dispatch failed: {err}");
            }
        }

        cancellation_token.cancel();
    }

    #[tokio::test]
    async fn test_orchestrator_dispatch_unknown_agent_returns_error() {
        let cancellation_token = CancellationToken::new();
        let registry = AgentRegistry::new();
        let orchestrator = OrchestratorHandle::spawn(registry, cancellation_token.child_token());

        let dispatch_result = orchestrator
            .dispatch(
                "unknown",
                AgentMessage::Task {
                    id: "task-1".to_string(),
                    content: "hello".to_string(),
                    metadata: HashMap::new(),
                },
            )
            .await;

        match dispatch_result {
            Err(OrchestratorError::AgentNotFound { name }) => {
                assert_eq!(name, "unknown");
            }
            other => {
                panic!("unexpected dispatch result: {other:?}");
            }
        }

        cancellation_token.cancel();
    }
}
