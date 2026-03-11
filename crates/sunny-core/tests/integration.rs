use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tokio::time::{sleep, timeout, Duration};
use tokio_util::sync::CancellationToken;

use sunny_core::agent::{
    Agent, AgentContext, AgentError, AgentHandle, AgentMessage, AgentResponse, Capability,
    EchoAgent,
};
use sunny_core::orchestrator::{AgentRegistry, OrchestratorError, OrchestratorHandle};

struct StopTrackingAgent {
    name: String,
    stopped: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Agent for StopTrackingAgent {
    fn name(&self) -> &str {
        &self.name
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
async fn test_dispatch_task_to_echo_agent_returns_success() {
    let cancellation_token = CancellationToken::new();
    let echo = Arc::new(EchoAgent);
    let echo_handle = AgentHandle::spawn(echo, cancellation_token.child_token());

    let mut registry = AgentRegistry::new();
    registry
        .register(
            "echo".to_string(),
            echo_handle,
            vec![Capability("echo".to_string())],
        )
        .expect("echo agent should register");

    let orchestrator = OrchestratorHandle::spawn(registry, cancellation_token.child_token());

    let mut metadata = HashMap::new();
    metadata.insert("k".to_string(), "v".to_string());
    let result = orchestrator
        .dispatch(
            "echo",
            AgentMessage::Task {
                id: "task-1".to_string(),
                content: "hello".to_string(),
                metadata: metadata.clone(),
            },
        )
        .await;

    match result {
        Ok(AgentResponse::Success {
            content,
            metadata: response_metadata,
        }) => {
            assert_eq!(content, "hello");
            assert_eq!(response_metadata, metadata);
        }
        Ok(AgentResponse::Error { code, message }) => {
            panic!("unexpected error response {code}: {message}");
        }
        Err(err) => {
            panic!("dispatch should succeed, got: {err}");
        }
    }

    cancellation_token.cancel();
}

#[tokio::test]
async fn test_dispatch_to_nonexistent_agent_returns_not_found() {
    let cancellation_token = CancellationToken::new();
    let registry = AgentRegistry::new();
    let orchestrator = OrchestratorHandle::spawn(registry, cancellation_token.child_token());

    let result = orchestrator
        .dispatch(
            "nonexistent",
            AgentMessage::Task {
                id: "task-1".to_string(),
                content: "payload".to_string(),
                metadata: HashMap::new(),
            },
        )
        .await;

    match result {
        Err(OrchestratorError::AgentNotFound { name }) => assert_eq!(name, "nonexistent"),
        other => panic!("expected AgentNotFound, got: {other:?}"),
    }

    cancellation_token.cancel();
}

#[tokio::test]
async fn test_graceful_shutdown_all_actors_complete() {
    let cancellation_token = CancellationToken::new();
    let agent_1_stopped = Arc::new(AtomicBool::new(false));
    let agent_2_stopped = Arc::new(AtomicBool::new(false));

    let agent_1 = Arc::new(StopTrackingAgent {
        name: "agent-1".to_string(),
        stopped: Arc::clone(&agent_1_stopped),
    });
    let agent_2 = Arc::new(StopTrackingAgent {
        name: "agent-2".to_string(),
        stopped: Arc::clone(&agent_2_stopped),
    });

    let handle_1 = AgentHandle::spawn(agent_1, cancellation_token.child_token());
    let handle_2 = AgentHandle::spawn(agent_2, cancellation_token.child_token());

    let mut registry = AgentRegistry::new();
    registry
        .register(
            "agent-1".to_string(),
            handle_1,
            vec![Capability("test".to_string())],
        )
        .expect("agent-1 should register");
    registry
        .register(
            "agent-2".to_string(),
            handle_2,
            vec![Capability("test".to_string())],
        )
        .expect("agent-2 should register");

    let orchestrator = OrchestratorHandle::spawn(registry, cancellation_token.child_token());

    cancellation_token.cancel();

    let shutdown_wait = timeout(Duration::from_secs(1), async {
        loop {
            let agents_stopped =
                agent_1_stopped.load(Ordering::SeqCst) && agent_2_stopped.load(Ordering::SeqCst);
            let orchestrator_stopped = matches!(
                orchestrator
                    .dispatch(
                        "agent-1",
                        AgentMessage::Task {
                            id: "post-cancel".to_string(),
                            content: "payload".to_string(),
                            metadata: HashMap::new(),
                        },
                    )
                    .await,
                Err(OrchestratorError::ShuttingDown)
            );

            if agents_stopped && orchestrator_stopped {
                break;
            }

            sleep(Duration::from_millis(10)).await;
        }
    })
    .await;

    assert!(
        shutdown_wait.is_ok(),
        "not all actors stopped within 1 second"
    );
}

// ============================================================================
// EDGE_CASE_TESTS: Orchestrator dispatch error paths
// ============================================================================

struct SlowAgent {
    delay: Duration,
    stopped: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl Agent for SlowAgent {
    fn name(&self) -> &str {
        "slow-agent"
    }

    fn capabilities(&self) -> Vec<Capability> {
        vec![Capability("test".to_string())]
    }

    async fn handle_message(
        &self,
        _msg: AgentMessage,
        _ctx: &AgentContext,
    ) -> Result<AgentResponse, AgentError> {
        sleep(self.delay).await;
        Ok(AgentResponse::Success {
            content: "delayed".to_string(),
            metadata: HashMap::new(),
        })
    }

    async fn on_stop(&self) -> Result<(), AgentError> {
        self.stopped.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test(start_paused = true)]
async fn test_dispatch_to_slow_agent_times_out() {
    let cancellation_token = CancellationToken::new();
    let stopped = Arc::new(AtomicBool::new(false));

    let agent = Arc::new(SlowAgent {
        delay: Duration::from_secs(300),
        stopped: Arc::clone(&stopped),
    });

    let handle = AgentHandle::spawn(agent, cancellation_token.child_token());

    let mut registry = AgentRegistry::new();
    registry
        .register(
            "slow".to_string(),
            handle,
            vec![Capability("test".to_string())],
        )
        .expect("slow agent should register");

    let orchestrator = OrchestratorHandle::spawn(registry, cancellation_token.child_token());

    let result = orchestrator
        .dispatch(
            "slow",
            AgentMessage::Task {
                id: "task-slow".to_string(),
                content: "payload".to_string(),
                metadata: HashMap::new(),
            },
        )
        .await;

    match result {
        Err(OrchestratorError::AgentUnresponsive) => {} // expected
        other => panic!("expected AgentUnresponsive, got: {other:?}"),
    }

    cancellation_token.cancel();
}

#[tokio::test(start_paused = true)]
async fn test_dispatch_after_orchestrator_shutdown_returns_error() {
    let cancellation_token = CancellationToken::new();
    let registry = AgentRegistry::new();
    let orchestrator = OrchestratorHandle::spawn(registry, cancellation_token.child_token());

    // Trigger orchestrator shutdown via cancellation
    cancellation_token.cancel();

    // Allow the orchestrator task to process cancellation and exit
    sleep(Duration::from_millis(10)).await;

    let result = orchestrator
        .dispatch(
            "any-agent",
            AgentMessage::Task {
                id: "task-after-shutdown".to_string(),
                content: "payload".to_string(),
                metadata: HashMap::new(),
            },
        )
        .await;

    match result {
        Err(OrchestratorError::ShuttingDown) => {} // expected
        other => panic!("expected ShuttingDown, got: {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn test_dispatch_when_agent_channel_closed() {
    let agent_token = CancellationToken::new();
    let orchestrator_token = CancellationToken::new();
    let stopped = Arc::new(AtomicBool::new(false));

    let agent = Arc::new(SlowAgent {
        delay: Duration::from_millis(1),
        stopped: Arc::clone(&stopped),
    });

    let handle = AgentHandle::spawn(agent, agent_token.clone());

    let mut registry = AgentRegistry::new();
    registry
        .register(
            "crash-agent".to_string(),
            handle,
            vec![Capability("test".to_string())],
        )
        .expect("crash agent should register");

    let orchestrator = OrchestratorHandle::spawn(registry, orchestrator_token.child_token());

    // Simulate agent crash by cancelling only the agent's token
    agent_token.cancel();

    // Allow the agent actor to exit and drop its receiver
    sleep(Duration::from_millis(10)).await;

    let result = orchestrator
        .dispatch(
            "crash-agent",
            AgentMessage::Task {
                id: "task-post-crash".to_string(),
                content: "payload".to_string(),
                metadata: HashMap::new(),
            },
        )
        .await;

    match result {
        Err(OrchestratorError::AgentUnresponsive) => {} // expected: agent channel closed
        other => panic!("expected AgentUnresponsive (agent channel closed), got: {other:?}"),
    }

    orchestrator_token.cancel();
}
