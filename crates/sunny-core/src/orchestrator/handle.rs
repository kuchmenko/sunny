use std::error::Error;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio::time::{timeout, Instant};
use tokio_util::sync::CancellationToken;

use crate::agent::{AgentError, AgentMessage, AgentResponse};

use super::telemetry::{DispatchTelemetry, NoopTelemetry};
use super::{AgentRegistry, NameRouting, OrchestratorError, RoutingStrategy};

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
        Self::spawn_with_routing(registry, Box::new(NameRouting), cancellation_token)
    }

    pub fn spawn_with_routing(
        registry: AgentRegistry,
        routing: Box<dyn RoutingStrategy>,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self::spawn_full(
            registry,
            routing,
            Arc::new(NoopTelemetry),
            cancellation_token,
        )
    }

    pub fn spawn_full(
        registry: AgentRegistry,
        routing: Box<dyn RoutingStrategy>,
        telemetry: Arc<dyn DispatchTelemetry>,
        cancellation_token: CancellationToken,
    ) -> Self {
        let (tx, rx) = mpsc::channel(32);

        let join_handle = tokio::spawn(run_orchestrator(
            registry,
            routing,
            telemetry,
            rx,
            cancellation_token,
        ));

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

        timeout(
            std::time::Duration::from_secs(30),
            self.tx.send(orchestrator_msg),
        )
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
    routing: Box<dyn RoutingStrategy>,
    telemetry: Arc<dyn DispatchTelemetry>,
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
                        telemetry.on_dispatch_start(&agent_name);
                        let start = Instant::now();

                        let result = match routing.resolve(&agent_name, &registry) {
                            Some(agent_handle) => agent_handle.send(msg).await.map_err(map_agent_error),
                            None => Err(OrchestratorError::AgentNotFound { name: agent_name.clone() }),
                        };

                        match &result {
                            Ok(_) => telemetry.on_dispatch_success(&agent_name, start.elapsed()),
                            Err(e) => telemetry.on_dispatch_error(&agent_name, &e.to_string(), start.elapsed()),
                        }

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
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    use crate::agent::{AgentHandle, AgentMessage, AgentResponse, Capability, EchoAgent};
    use crate::orchestrator::telemetry::DispatchTelemetry;

    use super::{OrchestratorError, OrchestratorHandle};
    use crate::orchestrator::AgentRegistry;

    #[tokio::test]
    async fn test_orchestrator_dispatch_to_echo_agent() {
        let cancellation_token = CancellationToken::new();
        let agent_handle =
            AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());
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

    #[tokio::test]
    async fn test_noop_telemetry_does_not_affect_dispatch() {
        let cancellation_token = CancellationToken::new();
        let agent_handle =
            AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());
        let mut registry = AgentRegistry::new();
        registry
            .register(
                "echo".to_string(),
                agent_handle,
                vec![Capability("echo".to_string())],
            )
            .expect("register should succeed");

        let orchestrator = OrchestratorHandle::spawn(registry, cancellation_token.child_token());

        let result = orchestrator
            .dispatch(
                "echo",
                AgentMessage::Task {
                    id: "t-noop".to_string(),
                    content: "ping".to_string(),
                    metadata: HashMap::new(),
                },
            )
            .await;

        match result {
            Ok(AgentResponse::Success { content, .. }) => {
                assert_eq!(content, "ping");
            }
            other => panic!("expected success, got: {other:?}"),
        }

        cancellation_token.cancel();
    }

    #[derive(Default)]
    struct RecordingTelemetry {
        starts: AtomicU32,
        successes: AtomicU32,
        errors: AtomicU32,
        last_agent: Mutex<String>,
        last_error_msg: Mutex<String>,
    }

    impl DispatchTelemetry for RecordingTelemetry {
        fn on_dispatch_start(&self, agent_name: &str) {
            self.starts.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut last) = self.last_agent.try_lock() {
                *last = agent_name.to_string();
            }
        }

        fn on_dispatch_success(&self, _agent_name: &str, _duration: Duration) {
            self.successes.fetch_add(1, Ordering::SeqCst);
        }

        fn on_dispatch_error(&self, _agent_name: &str, error: &str, _duration: Duration) {
            self.errors.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut last) = self.last_error_msg.try_lock() {
                *last = error.to_string();
            }
        }
    }

    #[tokio::test]
    async fn test_telemetry_hook_receives_dispatch_events() {
        let cancellation_token = CancellationToken::new();
        let agent_handle =
            AgentHandle::spawn(Arc::new(EchoAgent), cancellation_token.child_token());
        let mut registry = AgentRegistry::new();
        registry
            .register(
                "echo".to_string(),
                agent_handle,
                vec![Capability("echo".to_string())],
            )
            .expect("register should succeed");

        let telemetry = Arc::new(RecordingTelemetry::default());
        let orchestrator = OrchestratorHandle::spawn_full(
            registry,
            Box::new(crate::orchestrator::NameRouting),
            telemetry.clone(),
            cancellation_token.child_token(),
        );

        let result = orchestrator
            .dispatch(
                "echo",
                AgentMessage::Task {
                    id: "t-1".to_string(),
                    content: "hello".to_string(),
                    metadata: HashMap::new(),
                },
            )
            .await;
        assert!(result.is_ok());

        assert_eq!(telemetry.starts.load(Ordering::SeqCst), 1);
        assert_eq!(telemetry.successes.load(Ordering::SeqCst), 1);
        assert_eq!(telemetry.errors.load(Ordering::SeqCst), 0);
        assert_eq!(*telemetry.last_agent.lock().await, "echo");

        let err_result = orchestrator
            .dispatch(
                "missing",
                AgentMessage::Task {
                    id: "t-2".to_string(),
                    content: "nope".to_string(),
                    metadata: HashMap::new(),
                },
            )
            .await;
        assert!(err_result.is_err());

        assert_eq!(telemetry.starts.load(Ordering::SeqCst), 2);
        assert_eq!(telemetry.successes.load(Ordering::SeqCst), 1);
        assert_eq!(telemetry.errors.load(Ordering::SeqCst), 1);

        cancellation_token.cancel();
    }
}
