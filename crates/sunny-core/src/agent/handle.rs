use std::error::Error;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{timeout, Instant};
use tokio_util::sync::CancellationToken;

use crate::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse};
use crate::timeouts::{agent_reply_timeout, agent_send_timeout};

#[derive(Debug)]
struct AgentMailboxClosed;

impl std::fmt::Display for AgentMailboxClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "agent mailbox closed")
    }
}

impl Error for AgentMailboxClosed {}

#[derive(Debug)]
struct AgentReplyDropped;

impl std::fmt::Display for AgentReplyDropped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "agent reply channel dropped")
    }
}

impl Error for AgentReplyDropped {}

pub(crate) enum AgentActorMsg {
    HandleMessage {
        msg: crate::agent::AgentMessage,
        reply: oneshot::Sender<Result<crate::agent::AgentResponse, crate::agent::AgentError>>,
        trace_id: Option<String>,
        timestamp: Instant,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentLifecycleState {
    Starting,
    Running,
    Stopping,
    Stopped,
}

pub struct AgentHandle {
    name: String,
    tx: mpsc::Sender<AgentActorMsg>,
    lifecycle_rx: watch::Receiver<AgentLifecycleState>,
    #[allow(dead_code)]
    join_handle: tokio::task::JoinHandle<()>,
}

impl AgentHandle {
    pub fn spawn(agent: Arc<dyn Agent>, cancellation_token: CancellationToken) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let (lifecycle_tx, lifecycle_rx) = watch::channel(AgentLifecycleState::Starting);
        let name = agent.name().to_string();

        let join_handle =
            tokio::spawn(run_agent_actor(agent, rx, cancellation_token, lifecycle_tx));

        Self {
            name,
            tx,
            lifecycle_rx,
            join_handle,
        }
    }

    pub async fn send(&self, msg: AgentMessage) -> Result<AgentResponse, AgentError> {
        self.send_with_trace_id(msg, None).await
    }

    pub(crate) async fn send_with_trace_id(
        &self,
        msg: AgentMessage,
        trace_id: Option<String>,
    ) -> Result<AgentResponse, AgentError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let actor_msg = AgentActorMsg::HandleMessage {
            msg,
            reply: reply_tx,
            trace_id,
            timestamp: Instant::now(),
        };

        timeout(agent_send_timeout(), self.tx.send(actor_msg))
            .await
            .map_err(|_| AgentError::Timeout)?
            .map_err(mailbox_closed_error)?;

        timeout(agent_reply_timeout(), reply_rx)
            .await
            .map_err(|_| AgentError::Timeout)?
            .map_err(reply_dropped_error)?
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub async fn await_stopped(&mut self) -> Result<(), AgentError> {
        while *self.lifecycle_rx.borrow() != AgentLifecycleState::Stopped {
            self.lifecycle_rx
                .changed()
                .await
                .map_err(|_| AgentError::ExecutionFailed {
                    source: Box::new(std::io::Error::other("lifecycle watch sender dropped")),
                })?;
        }

        Ok(())
    }
}

pub(crate) async fn run_agent_actor(
    agent: Arc<dyn Agent>,
    mut rx: mpsc::Receiver<AgentActorMsg>,
    cancellation_token: CancellationToken,
    lifecycle_tx: watch::Sender<AgentLifecycleState>,
) {
    let ctx = AgentContext {
        agent_name: agent.name().to_string(),
    };

    let _ = lifecycle_tx.send(AgentLifecycleState::Starting);

    if let Err(err) = agent.on_start(&ctx).await {
        tracing::error!(agent = %ctx.agent_name, error = %err, "agent on_start failed");
        let _ = lifecycle_tx.send(AgentLifecycleState::Stopped);
        return;
    }

    let _ = lifecycle_tx.send(AgentLifecycleState::Running);

    loop {
        tokio::select! {
            _ = cancellation_token.cancelled() => {
                break;
            }
            maybe_msg = rx.recv() => {
                match maybe_msg {
                    Some(AgentActorMsg::HandleMessage { msg, reply, trace_id, timestamp }) => {
                        if let Some(trace_id) = trace_id {
                            tracing::debug!(
                                agent = %ctx.agent_name,
                                trace_id = %trace_id,
                                queued_for_ms = timestamp.elapsed().as_millis() as u64,
                                "processing agent message"
                            );
                        }
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

    let _ = lifecycle_tx.send(AgentLifecycleState::Stopping);

    if let Err(err) = agent.on_stop().await {
        tracing::error!(agent = %ctx.agent_name, error = %err, "agent on_stop failed");
    }

    let _ = lifecycle_tx.send(AgentLifecycleState::Stopped);
}

fn mailbox_closed_error(_err: impl Error + Send + Sync + 'static) -> AgentError {
    AgentError::ExecutionFailed {
        source: Box::new(AgentMailboxClosed),
    }
}

fn reply_dropped_error(_err: impl Error + Send + Sync + 'static) -> AgentError {
    AgentError::ExecutionFailed {
        source: Box::new(AgentReplyDropped),
    }
}

pub(crate) fn is_transport_failure(err: &AgentError) -> bool {
    match err {
        AgentError::ExecutionFailed { source } => {
            source.is::<AgentMailboxClosed>() || source.is::<AgentReplyDropped>()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use tokio::sync::watch;
    use tokio::time::{sleep, timeout, Duration};
    use tokio_util::sync::CancellationToken;

    use crate::agent::{Agent, AgentContext, AgentError, AgentMessage, AgentResponse, Capability};

    use super::{AgentHandle, AgentLifecycleState};

    struct TestAgent {
        stopped: Arc<AtomicBool>,
    }

    struct DelayedStopAgent {
        stopped: Arc<AtomicBool>,
    }

    #[cfg(test)]
    struct FaultyAgent {
        panic_msg: Option<String>,
        error_response: Option<(String, String)>,
        delay: Option<Duration>,
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

    #[async_trait::async_trait]
    impl Agent for DelayedStopAgent {
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
            sleep(Duration::from_millis(25)).await;
            self.stopped.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    #[async_trait::async_trait]
    impl Agent for FaultyAgent {
        fn name(&self) -> &str {
            "faulty-agent"
        }

        fn capabilities(&self) -> Vec<Capability> {
            vec![Capability("test".to_string())]
        }

        async fn handle_message(
            &self,
            msg: AgentMessage,
            _ctx: &AgentContext,
        ) -> Result<AgentResponse, AgentError> {
            if let Some(panic_msg) = &self.panic_msg {
                panic!("{panic_msg}");
            }

            if let Some(delay) = self.delay {
                sleep(delay).await;
            }

            if let Some((code, message)) = &self.error_response {
                return Ok(AgentResponse::Error {
                    code: code.clone(),
                    message: message.clone(),
                });
            }

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

    #[tokio::test]
    async fn test_agent_lifecycle_state_transitions() {
        let stopped = Arc::new(AtomicBool::new(false));
        let agent = Arc::new(DelayedStopAgent {
            stopped: Arc::clone(&stopped),
        });
        let cancellation_token = CancellationToken::new();
        let handle = AgentHandle::spawn(agent, cancellation_token.clone());

        let mut lifecycle_rx = handle.lifecycle_rx.clone();
        let mut observed_states = vec![AgentLifecycleState::Starting, *lifecycle_rx.borrow()];

        wait_for_lifecycle_state(
            &mut lifecycle_rx,
            AgentLifecycleState::Running,
            &mut observed_states,
        )
        .await;

        let response = handle
            .send(AgentMessage::Task {
                id: "task-lifecycle".to_string(),
                content: "payload".to_string(),
                metadata: HashMap::new(),
            })
            .await
            .expect("send should succeed while agent is running");

        match response {
            AgentResponse::Success { .. } => {}
            AgentResponse::Error { code, message } => {
                panic!("unexpected error response {code}: {message}");
            }
        }

        cancellation_token.cancel();

        wait_for_lifecycle_state(
            &mut lifecycle_rx,
            AgentLifecycleState::Stopping,
            &mut observed_states,
        )
        .await;
        wait_for_lifecycle_state(
            &mut lifecycle_rx,
            AgentLifecycleState::Stopped,
            &mut observed_states,
        )
        .await;

        assert_state_order(
            &observed_states,
            AgentLifecycleState::Starting,
            AgentLifecycleState::Running,
        );
        assert_state_order(
            &observed_states,
            AgentLifecycleState::Running,
            AgentLifecycleState::Stopping,
        );
        assert_state_order(
            &observed_states,
            AgentLifecycleState::Stopping,
            AgentLifecycleState::Stopped,
        );
    }

    #[tokio::test]
    async fn test_await_stopped_completes_after_cancel() {
        let stopped = Arc::new(AtomicBool::new(false));
        let agent = Arc::new(TestAgent {
            stopped: Arc::clone(&stopped),
        });
        let cancellation_token = CancellationToken::new();
        let mut handle = AgentHandle::spawn(agent, cancellation_token.clone());

        cancellation_token.cancel();

        let result = timeout(Duration::from_secs(1), handle.await_stopped()).await;
        assert!(
            result.is_ok(),
            "await_stopped did not complete in time after cancel"
        );
        assert!(result.expect("timeout should be handled").is_ok());
    }

    #[tokio::test]
    async fn test_await_stopped_returns_immediately_if_already_stopped() {
        let stopped = Arc::new(AtomicBool::new(false));
        let agent = Arc::new(TestAgent {
            stopped: Arc::clone(&stopped),
        });
        let cancellation_token = CancellationToken::new();
        let mut handle = AgentHandle::spawn(agent, cancellation_token.clone());
        let mut lifecycle_rx = handle.lifecycle_rx.clone();

        cancellation_token.cancel();

        let mut observed_states = Vec::new();
        wait_for_lifecycle_state(
            &mut lifecycle_rx,
            AgentLifecycleState::Stopped,
            &mut observed_states,
        )
        .await;

        let result = timeout(Duration::from_millis(50), handle.await_stopped()).await;
        assert!(
            result.is_ok(),
            "await_stopped should return immediately when already stopped"
        );
        assert!(result.expect("timeout should be handled").is_ok());
    }

    #[tokio::test]
    async fn test_faulty_agent_returns_error_response() {
        let stopped = Arc::new(AtomicBool::new(false));
        let agent = Arc::new(FaultyAgent {
            panic_msg: None,
            error_response: Some(("bad_request".to_string(), "invalid payload".to_string())),
            delay: None,
            stopped,
        });
        let cancellation_token = CancellationToken::new();
        let handle = AgentHandle::spawn(agent, cancellation_token.clone());

        let response = handle
            .send(AgentMessage::Task {
                id: "task-error".to_string(),
                content: "payload".to_string(),
                metadata: HashMap::new(),
            })
            .await
            .expect("send should return error response variant");

        match response {
            AgentResponse::Error { code, message } => {
                assert_eq!(code, "bad_request");
                assert_eq!(message, "invalid payload");
            }
            AgentResponse::Success { .. } => {
                panic!("expected error response from FaultyAgent");
            }
        }

        cancellation_token.cancel();
    }

    #[tokio::test]
    async fn test_cancel_during_inflight_message() {
        tokio::time::pause();

        let stopped = Arc::new(AtomicBool::new(false));
        let agent = Arc::new(FaultyAgent {
            panic_msg: None,
            error_response: None,
            delay: Some(Duration::from_secs(5)),
            stopped,
        });
        let cancellation_token = CancellationToken::new();
        let handle = Arc::new(AgentHandle::spawn(agent, cancellation_token.clone()));

        let first_handle = Arc::clone(&handle);
        let first_send = tokio::spawn(async move {
            first_handle
                .send(AgentMessage::Task {
                    id: "task-inflight-1".to_string(),
                    content: "payload-1".to_string(),
                    metadata: HashMap::new(),
                })
                .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(100)).await;
        cancellation_token.cancel();
        tokio::time::advance(Duration::from_secs(5)).await;

        let first_result = first_send
            .await
            .expect("first send join should complete without join error");
        assert!(
            matches!(first_result, Ok(AgentResponse::Success { .. })),
            "first in-flight message should complete"
        );

        let second_handle = Arc::clone(&handle);
        let second_result = second_handle
            .send(AgentMessage::Task {
                id: "task-inflight-2".to_string(),
                content: "payload-2".to_string(),
                metadata: HashMap::new(),
            })
            .await;
        assert!(
            second_result.is_err(),
            "send after cancellation should fail"
        );
    }

    #[tokio::test]
    async fn test_send_after_cancel_returns_error() {
        let stopped = Arc::new(AtomicBool::new(false));
        let agent = Arc::new(FaultyAgent {
            panic_msg: None,
            error_response: None,
            delay: None,
            stopped,
        });
        let cancellation_token = CancellationToken::new();
        let handle = AgentHandle::spawn(agent, cancellation_token.clone());

        cancellation_token.cancel();
        sleep(Duration::from_millis(10)).await;

        let result = handle
            .send(AgentMessage::Task {
                id: "task-after-cancel".to_string(),
                content: "payload".to_string(),
                metadata: HashMap::new(),
            })
            .await;

        assert!(
            result.is_err(),
            "send after cancellation should return an error"
        );
        match result.expect_err("result should be an error") {
            AgentError::ExecutionFailed { .. } => {}
            err => panic!("expected ExecutionFailed, got {err:?}"),
        }
    }

    #[tokio::test]
    async fn test_agent_panic_does_not_block_handle() {
        let stopped = Arc::new(AtomicBool::new(false));
        let agent = Arc::new(FaultyAgent {
            panic_msg: Some("boom".to_string()),
            error_response: None,
            delay: None,
            stopped,
        });
        let cancellation_token = CancellationToken::new();
        let handle = AgentHandle::spawn(agent, cancellation_token);

        let result = timeout(
            Duration::from_secs(1),
            handle.send(AgentMessage::Task {
                id: "task-panic".to_string(),
                content: "payload".to_string(),
                metadata: HashMap::new(),
            }),
        )
        .await
        .expect("send should not hang when agent panics");

        assert!(result.is_err(), "send should return error after panic");
    }

    #[tokio::test]
    async fn test_timeout_on_slow_agent() {
        tokio::time::pause();

        let stopped = Arc::new(AtomicBool::new(false));
        let agent = Arc::new(FaultyAgent {
            panic_msg: None,
            error_response: None,
            delay: Some(Duration::from_secs(300)),
            stopped,
        });
        let cancellation_token = CancellationToken::new();
        let handle = AgentHandle::spawn(agent, cancellation_token.clone());

        let send_task = tokio::spawn(async move {
            handle
                .send(AgentMessage::Task {
                    id: "task-timeout".to_string(),
                    content: "payload".to_string(),
                    metadata: HashMap::new(),
                })
                .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(crate::timeouts::agent_reply_timeout() + Duration::from_millis(1))
            .await;

        let result = send_task
            .await
            .expect("send task should complete without join error");

        match result.expect_err("send should timeout on slow agent") {
            AgentError::Timeout => {}
            err => panic!("expected timeout error, got {err:?}"),
        }

        cancellation_token.cancel();
    }

    #[tokio::test]
    async fn test_agent_tracing_events() {
        let stopped = Arc::new(AtomicBool::new(false));
        let agent = Arc::new(TestAgent {
            stopped: Arc::clone(&stopped),
        });
        let cancellation_token = CancellationToken::new();
        let handle = AgentHandle::spawn(agent, cancellation_token.clone());

        let response = handle
            .send(AgentMessage::Task {
                id: "task-tracing-test".to_string(),
                content: "test-payload".to_string(),
                metadata: HashMap::new(),
            })
            .await
            .expect("send should succeed");

        match response {
            AgentResponse::Success { .. } => {}
            AgentResponse::Error { code, message } => {
                panic!("unexpected error response {code}: {message}");
            }
        }

        // Test verifies that tracing events are emitted without panicking
        // Start event: agent.message.start with agent_name, task_id
        // End event: agent.message.end with duration_ms, outcome=success
        // Error event: agent.message.error with error_code, error_message (on AgentResponse::Error)

        cancellation_token.cancel();
        let _ = timeout(Duration::from_secs(1), async {
            while !stopped.load(Ordering::SeqCst) {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
    }

    #[tokio::test]
    async fn test_agent_tracing_error_event() {
        let stopped = Arc::new(AtomicBool::new(false));
        let agent = Arc::new(FaultyAgent {
            panic_msg: None,
            error_response: Some(("test_error".to_string(), "test error message".to_string())),
            delay: None,
            stopped,
        });
        let cancellation_token = CancellationToken::new();
        let handle = AgentHandle::spawn(agent, cancellation_token.clone());

        let response = handle
            .send(AgentMessage::Task {
                id: "task-error-tracing".to_string(),
                content: "test-payload".to_string(),
                metadata: HashMap::new(),
            })
            .await
            .expect("send should return error response");

        match response {
            AgentResponse::Error { code, message } => {
                assert_eq!(code, "test_error");
                assert_eq!(message, "test error message");
            }
            AgentResponse::Success { .. } => {
                panic!("expected error response from FaultyAgent");
            }
        }

        cancellation_token.cancel();
    }

    async fn wait_for_lifecycle_state(
        lifecycle_rx: &mut watch::Receiver<AgentLifecycleState>,
        target: AgentLifecycleState,
        observed_states: &mut Vec<AgentLifecycleState>,
    ) {
        timeout(Duration::from_secs(1), async {
            while *lifecycle_rx.borrow() != target {
                if lifecycle_rx.changed().await.is_err() {
                    break;
                }
                observed_states.push(*lifecycle_rx.borrow());
            }

            assert_eq!(
                *lifecycle_rx.borrow(),
                target,
                "expected lifecycle state {:?}, observed {:?}",
                target,
                observed_states
            );
        })
        .await
        .expect("lifecycle transition should complete in time");
    }

    fn assert_state_order(
        observed_states: &[AgentLifecycleState],
        first: AgentLifecycleState,
        second: AgentLifecycleState,
    ) {
        let first_index = observed_states
            .iter()
            .position(|state| *state == first)
            .expect("first lifecycle state should be observed");
        let second_index = observed_states
            .iter()
            .position(|state| *state == second)
            .expect("second lifecycle state should be observed");

        assert!(
            first_index <= second_index,
            "expected {:?} before {:?}, observed states: {:?}",
            first,
            second,
            observed_states
        );
    }
}
