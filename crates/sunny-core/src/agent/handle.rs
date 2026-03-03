use std::error::Error;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse};

pub(crate) enum AgentActorMsg {
    HandleMessage {
        msg: crate::agent::AgentMessage,
        reply: oneshot::Sender<Result<crate::agent::AgentResponse, crate::agent::AgentError>>,
    },
}

pub struct AgentHandle {
    name: String,
    tx: mpsc::Sender<AgentActorMsg>,
    #[allow(dead_code)]
    join_handle: tokio::task::JoinHandle<()>,
}

impl AgentHandle {
    pub fn spawn(agent: Arc<dyn Agent>, cancellation_token: CancellationToken) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let name = agent.name().to_string();

        let join_handle = tokio::spawn(run_agent_actor(agent, rx, cancellation_token));

        Self { name, tx, join_handle }
    }

    pub async fn send(&self, msg: AgentMessage) -> Result<AgentResponse, AgentError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let actor_msg = AgentActorMsg::HandleMessage {
            msg,
            reply: reply_tx,
        };

        timeout(std::time::Duration::from_secs(30), self.tx.send(actor_msg))
            .await
            .map_err(|_| AgentError::Timeout)?
            .map_err(channel_closed_error)?;

        timeout(std::time::Duration::from_secs(30), reply_rx)
            .await
            .map_err(|_| AgentError::Timeout)?
            .map_err(channel_closed_error)?
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

pub(crate) async fn run_agent_actor(
    agent: Arc<dyn Agent>,
    mut rx: mpsc::Receiver<AgentActorMsg>,
    cancellation_token: CancellationToken,
) {
    let ctx = AgentContext {
        agent_name: agent.name().to_string(),
    };

    if let Err(err) = agent.on_start(&ctx).await {
        tracing::error!(agent = %ctx.agent_name, error = %err, "agent on_start failed");
        return;
    }

    loop {
        tokio::select! {
            _ = cancellation_token.cancelled() => {
                break;
            }
            maybe_msg = rx.recv() => {
                match maybe_msg {
                    Some(AgentActorMsg::HandleMessage { msg, reply }) => {
                        let result = agent.handle_message(msg, &ctx).await;
                        let _ = reply.send(result);
                    }
                    None => {
                        break;
                    }
                }
            }
        }
    }

    if let Err(err) = agent.on_stop().await {
        tracing::error!(agent = %ctx.agent_name, error = %err, "agent on_stop failed");
    }
}

fn channel_closed_error(err: impl Error + Send + Sync + 'static) -> AgentError {
    AgentError::ExecutionFailed {
        source: Box::new(err),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use tokio::time::{Duration, sleep, timeout};
    use tokio_util::sync::CancellationToken;

    use crate::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability};

    use super::AgentHandle;

    struct TestAgent {
        stopped: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl Agent for TestAgent {
        fn name(&self) -> &str {
            "test-agent"
        }

        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability("test".to_string())]
        }

        async fn handle_message(
            &self,
            msg: AgentMessage,
            _ctx: &AgentContext,
        ) -> Result<AgentResponse, AgentError> {
            match msg {
                AgentMessage::Task {
                    id: _,
                    content,
                    metadata,
                } => Ok(AgentResponse::Success { content, metadata }),
            }
        }

        async fn on_stop(&self) -> Result<(), AgentError> {
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_agent_handle_send_returns_response() {
        let stopped = Arc::new(AtomicBool::new(false));
        let agent = Arc::new(TestAgent {
            stopped: Arc::clone(&stopped),
        });
        let cancellation_token = CancellationToken::new();
        let handle = AgentHandle::spawn(agent, cancellation_token.clone());

        let mut metadata = HashMap::new();
        metadata.insert("k".to_string(), "v".to_string());

        let response = handle
            .send(AgentMessage::Task {
                id: "task-1".to_string(),
                content: "payload".to_string(),
                metadata: metadata.clone(),
            })
            .await
            .expect("send should return success response");

        match response {
            AgentResponse::Success {
                content,
                metadata: response_metadata,
            } => {
                assert_eq!(content, "payload");
                assert_eq!(response_metadata, metadata);
            }
            AgentResponse::Error { code, message } => {
                panic!("unexpected error response {code}: {message}");
            }
        }

        assert_eq!(handle.name(), "test-agent");

        cancellation_token.cancel();
        let _ = timeout(Duration::from_secs(1), async {
            while !stopped.load(Ordering::SeqCst) {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_agent_handle_shutdown_on_cancel() {
        let stopped = Arc::new(AtomicBool::new(false));
        let agent = Arc::new(TestAgent {
            stopped: Arc::clone(&stopped),
        });
        let cancellation_token = CancellationToken::new();
        let _handle = AgentHandle::spawn(agent, cancellation_token.clone());

        cancellation_token.cancel();

        let shutdown_result = timeout(Duration::from_secs(1), async {
            while !stopped.load(Ordering::SeqCst) {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await;

        assert!(shutdown_result.is_ok(), "actor did not shutdown on cancel");
    }
}
