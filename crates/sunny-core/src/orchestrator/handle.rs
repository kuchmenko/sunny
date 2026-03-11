use std::error::Error;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};
use tokio::time::{timeout, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{info, info_span};

use crate::agent::{AgentError, AgentMessage, AgentResponse, Capability};

use super::telemetry::{DispatchTelemetry, NoopTelemetry};
use super::{
    events::{
        EVENT_DISPATCH_ERROR, EVENT_DISPATCH_START, EVENT_DISPATCH_SUCCESS, OUTCOME_ERROR,
        OUTCOME_SUCCESS,
    },
    AgentRegistry, CapabilityRouter, IntentRouter, NameRouting, OrchestratorError, RequestId,
    RoutingStrategy, TieBreakPolicy,
};

pub(crate) enum OrchestratorMsg {
    Dispatch {
        agent_name: String,
        msg: AgentMessage,
        request_id: Option<RequestId>,
        reply: oneshot::Sender<Result<AgentResponse, OrchestratorError>>,
    },
    DispatchByCapability {
        capability: Capability,
        msg: AgentMessage,
        request_id: Option<RequestId>,
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
        self.dispatch_internal(agent_name, msg, None).await
    }

    pub async fn dispatch_with_context(
        &self,
        agent_name: &str,
        msg: AgentMessage,
        request_id: RequestId,
    ) -> Result<AgentResponse, OrchestratorError> {
        self.dispatch_internal(agent_name, msg, Some(request_id))
            .await
    }

    pub async fn dispatch_by_capability(
        &self,
        capability: Capability,
        msg: AgentMessage,
    ) -> Result<AgentResponse, OrchestratorError> {
        self.dispatch_by_capability_internal(capability, msg, None)
            .await
    }

    pub async fn dispatch_by_capability_with_context(
        &self,
        capability: Capability,
        msg: AgentMessage,
        request_id: RequestId,
    ) -> Result<AgentResponse, OrchestratorError> {
        self.dispatch_by_capability_internal(capability, msg, Some(request_id))
            .await
    }

    async fn dispatch_internal(
        &self,
        agent_name: &str,
        msg: AgentMessage,
        request_id: Option<RequestId>,
    ) -> Result<AgentResponse, OrchestratorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let request_id = request_id.unwrap_or_default();
        let orchestrator_msg = OrchestratorMsg::Dispatch {
            agent_name: agent_name.to_string(),
            msg,
            request_id: Some(request_id),
            reply: reply_tx,
        };

        timeout(
            std::time::Duration::from_secs(60),
            self.tx.send(orchestrator_msg),
        )
        .await
        .map_err(|_| OrchestratorError::AgentUnresponsive)?
        .map_err(channel_closed_error)?;

        timeout(std::time::Duration::from_secs(60), reply_rx)
            .await
            .map_err(|_| OrchestratorError::AgentUnresponsive)?
            .map_err(channel_closed_error)?
    }

    async fn dispatch_by_capability_internal(
        &self,
        capability: Capability,
        msg: AgentMessage,
        request_id: Option<RequestId>,
    ) -> Result<AgentResponse, OrchestratorError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let request_id = request_id.unwrap_or_default();
        let orchestrator_msg = OrchestratorMsg::DispatchByCapability {
            capability,
            msg,
            request_id: Some(request_id),
            reply: reply_tx,
        };

        timeout(
            std::time::Duration::from_secs(60),
            self.tx.send(orchestrator_msg),
        )
        .await
        .map_err(|_| OrchestratorError::AgentUnresponsive)?
        .map_err(channel_closed_error)?;

        timeout(std::time::Duration::from_secs(60), reply_rx)
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
                    Some(OrchestratorMsg::Dispatch { agent_name, msg, request_id, reply }) => {
                        let request_id = request_id.unwrap_or_default();
                        let request_id_str = request_id.to_string();
                        let dispatch_span = info_span!("orchestrator.dispatch", request_id = %request_id);
                        let _guard = dispatch_span.enter();

                        info!(
                            event = EVENT_DISPATCH_START,
                            request_id = %request_id,
                            agent_name = %agent_name
                        );
                        telemetry.on_dispatch_start(&agent_name);
                        let start = Instant::now();

                        let msg = inject_request_id_metadata(msg, &request_id_str);
                        let result = match routing.resolve(&agent_name, &registry) {
                            Some(agent_handle) => agent_handle
                                .send_with_trace_id(msg, Some(request_id_str))
                                .await
                                .map_err(map_agent_error),
                            None => Err(OrchestratorError::AgentNotFound { name: agent_name.clone() }),
                        };
                        let result = strip_internal_metadata(result);

                        let duration_ms = start.elapsed().as_millis() as u64;

                        match &result {
                            Ok(_) => {
                                info!(
                                    event = EVENT_DISPATCH_SUCCESS,
                                    request_id = %request_id,
                                    agent_name = %agent_name,
                                    outcome = OUTCOME_SUCCESS,
                                    duration_ms
                                );
                                telemetry.on_dispatch_success(&agent_name, start.elapsed())
                            }
                            Err(e) => {
                                info!(
                                    event = EVENT_DISPATCH_ERROR,
                                    request_id = %request_id,
                                    agent_name = %agent_name,
                                    outcome = OUTCOME_ERROR,
                                    error = %e,
                                    duration_ms
                                );
                                telemetry.on_dispatch_error(&agent_name, &e.to_string(), start.elapsed())
                            }
                        }

                        let _ = reply.send(result);
                    }
                    Some(OrchestratorMsg::DispatchByCapability {
                        capability,
                        msg,
                        request_id,
                        reply,
                    }) => {
                        let request_id = request_id.unwrap_or_default();
                        let request_id_str = request_id.to_string();
                        let dispatch_span = info_span!("orchestrator.dispatch", request_id = %request_id);
                        let _guard = dispatch_span.enter();

                        let router = CapabilityRouter::new(TieBreakPolicy::Lexicographic);
                        let result = match router.route(&capability, &registry) {
                            Ok(agent_handle) => {
                                let agent_name = agent_handle.name();
                                info!(
                                    event = EVENT_DISPATCH_START,
                                    request_id = %request_id,
                                    agent_name = %agent_name
                                );
                                telemetry.on_dispatch_start(agent_name);

                                let start = Instant::now();
                                let msg = inject_request_id_metadata(msg, &request_id_str);
                                let result = agent_handle
                                    .send_with_trace_id(msg, Some(request_id_str))
                                    .await
                                    .map_err(map_agent_error);
                                let result = strip_internal_metadata(result);
                                let duration_ms = start.elapsed().as_millis() as u64;

                                match &result {
                                    Ok(_) => {
                                        info!(
                                            event = EVENT_DISPATCH_SUCCESS,
                                            request_id = %request_id,
                                            agent_name = %agent_name,
                                            outcome = OUTCOME_SUCCESS,
                                            duration_ms
                                        );
                                        telemetry.on_dispatch_success(agent_name, start.elapsed())
                                    }
                                    Err(e) => {
                                        info!(
                                            event = EVENT_DISPATCH_ERROR,
                                            request_id = %request_id,
                                            agent_name = %agent_name,
                                            outcome = OUTCOME_ERROR,
                                            error = %e,
                                            duration_ms
                                        );
                                        telemetry.on_dispatch_error(agent_name, &e.to_string(), start.elapsed())
                                    }
                                }

                                result
                            }
                            Err(err) => {
                                info!(
                                    event = EVENT_DISPATCH_ERROR,
                                    request_id = %request_id,
                                    agent_name = %capability.0,
                                    outcome = OUTCOME_ERROR,
                                    error = %err,
                                    duration_ms = 0u64
                                );
                                telemetry.on_dispatch_error(&capability.0, &err.to_string(), std::time::Duration::from_millis(0));
                                Err(err)
                            }
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

fn inject_request_id_metadata(msg: AgentMessage, request_id: &str) -> AgentMessage {
    match msg {
        AgentMessage::Task {
            id,
            content,
            mut metadata,
        } => {
            metadata.insert("_sunny.request_id".to_string(), request_id.to_string());
            AgentMessage::Task {
                id,
                content,
                metadata,
            }
        }
    }
}

fn strip_internal_metadata(
    result: Result<AgentResponse, OrchestratorError>,
) -> Result<AgentResponse, OrchestratorError> {
    result.map(|response| match response {
        AgentResponse::Success {
            content,
            mut metadata,
        } => {
            metadata.remove("_sunny.request_id");
            AgentResponse::Success { content, metadata }
        }
        other => other,
    })
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

    use crate::agent::{
        Agent, AgentContext, AgentError, AgentHandle, AgentMessage, AgentResponse, Capability,
        EchoAgent,
    };
    use crate::orchestrator::telemetry::DispatchTelemetry;
    use crate::orchestrator::{
        events::{EVENT_DISPATCH_START, EVENT_DISPATCH_SUCCESS},
        RequestId,
    };

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

    struct RequestIdEchoAgent;

    #[async_trait::async_trait]
    impl Agent for RequestIdEchoAgent {
        fn name(&self) -> &str {
            "request-id-echo"
        }

        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability("echo".to_string())]
        }

        async fn handle_message(
            &self,
            msg: AgentMessage,
            _ctx: &AgentContext,
        ) -> Result<AgentResponse, AgentError> {
            match msg {
                AgentMessage::Task { metadata, .. } => {
                    let request_id = metadata
                        .get("_sunny.request_id")
                        .cloned()
                        .unwrap_or_else(|| "missing".to_string());

                    Ok(AgentResponse::Success {
                        content: request_id,
                        metadata: HashMap::new(),
                    })
                }
            }
        }
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

    #[tokio::test]
    #[tracing_test::traced_test]
    async fn test_dispatch_request_id_propagation() {
        let cancellation_token = CancellationToken::new();
        let agent_handle = AgentHandle::spawn(
            Arc::new(RequestIdEchoAgent),
            cancellation_token.child_token(),
        );
        let mut registry = AgentRegistry::new();
        registry
            .register(
                "echo".to_string(),
                agent_handle,
                vec![Capability("echo".to_string())],
            )
            .expect("register should succeed");

        let orchestrator = OrchestratorHandle::spawn(registry, cancellation_token.child_token());
        let request_id = RequestId::new();
        let request_id_str = request_id.to_string();

        let result = orchestrator
            .dispatch_with_context(
                "echo",
                AgentMessage::Task {
                    id: "t-request-id".to_string(),
                    content: "hello".to_string(),
                    metadata: HashMap::new(),
                },
                request_id,
            )
            .await;

        match result {
            Ok(AgentResponse::Success { content, metadata }) => {
                assert_eq!(content, request_id_str);
                assert!(metadata.is_empty());
            }
            other => panic!("expected success response, got: {other:?}"),
        }

        assert!(logs_contain(EVENT_DISPATCH_START));
        assert!(logs_contain(EVENT_DISPATCH_SUCCESS));
        assert!(logs_contain("processing agent message"));
        assert!(logs_contain(&format!("request_id={request_id_str}")));
        assert!(logs_contain(&format!("trace_id={request_id_str}")));

        cancellation_token.cancel();
    }
}
