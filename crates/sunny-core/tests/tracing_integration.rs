use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, Once, OnceLock};
use std::time::Instant;

use tokio::time::{sleep, Duration};
use tokio_util::sync::CancellationToken;
use tracing::field::{Field, Visit};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::Registry;

use sunny_core::agent::{
    Agent, AgentContext, AgentError, AgentHandle, AgentMessage, AgentResponse, Capability,
};
use sunny_core::orchestrator::{
    AgentRegistry, OrchestratorHandle, RequestId, EVENT_DISPATCH_START, EVENT_DISPATCH_SUCCESS,
    OUTCOME_CANCELLED, OUTCOME_SUCCESS,
};

#[derive(Default)]
struct EventStore {
    lines: Mutex<Vec<String>>,
}

impl EventStore {
    fn push(&self, line: String) {
        self.lines
            .lock()
            .expect("event store lock poisoned")
            .push(line);
    }

    fn snapshot(&self) -> Vec<String> {
        self.lines
            .lock()
            .expect("event store lock poisoned")
            .clone()
    }
}

struct EventCaptureLayer {
    store: Arc<EventStore>,
}

impl<S> Layer<S> for EventCaptureLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        visitor.fields.sort_unstable();
        let fields = visitor.fields.join(" ");
        let line = if fields.is_empty() {
            event.metadata().name().to_string()
        } else {
            format!("{} {fields}", event.metadata().name())
        };

        self.store.push(line);
    }
}

#[derive(Default)]
struct FieldVisitor {
    fields: Vec<String>,
}

impl Visit for FieldVisitor {
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.push(format!("{}={value}", field.name()));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.push(format!("{}={value}", field.name()));
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.push(format!("{}={value}", field.name()));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.fields.push(format!("{}={value}", field.name()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.fields.push(format!("{}={value:?}", field.name()));
    }
}

fn init_tracing_capture() -> Arc<EventStore> {
    static STORE: OnceLock<Arc<EventStore>> = OnceLock::new();
    static INIT: Once = Once::new();

    let store = STORE
        .get_or_init(|| Arc::new(EventStore::default()))
        .clone();

    INIT.call_once(|| {
        let subscriber = Registry::default().with(EventCaptureLayer {
            store: Arc::clone(&store),
        });

        tracing::subscriber::set_global_default(subscriber)
            .expect("global tracing subscriber should install once");
    });

    store
}

#[derive(Clone)]
struct EchoTracingAgent {
    name: String,
    delay: Duration,
    task_cancel: CancellationToken,
}

#[async_trait::async_trait]
impl Agent for EchoTracingAgent {
    fn name(&self) -> &str {
        &self.name
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
            AgentMessage::Task {
                id,
                content,
                metadata,
            } => {
                let request_id = metadata
                    .get("_sunny.request_id")
                    .cloned()
                    .unwrap_or_else(|| "missing".to_string());

                tracing::info!(
                    event = "agent.message.start",
                    agent_name = %self.name,
                    task_id = %id,
                    request_id = %request_id
                );

                let started = Instant::now();

                let outcome = tokio::select! {
                    _ = self.task_cancel.cancelled() => OUTCOME_CANCELLED,
                    _ = sleep(self.delay) => OUTCOME_SUCCESS,
                };

                tracing::info!(
                    event = "agent.message.end",
                    agent_name = %self.name,
                    task_id = %id,
                    request_id = %request_id,
                    duration_ms = started.elapsed().as_millis() as u64,
                    outcome = outcome
                );

                if outcome == OUTCOME_CANCELLED {
                    Ok(AgentResponse::Error {
                        code: "cancelled".to_string(),
                        message: "task cancelled".to_string(),
                    })
                } else {
                    Ok(AgentResponse::Success { content, metadata })
                }
            }
        }
    }
}

fn request_lines(lines: &[String], request_id: &str) -> Vec<String> {
    let needle = format!("request_id={request_id}");
    lines
        .iter()
        .filter(|line| line.contains(&needle))
        .cloned()
        .collect()
}

fn find_index(lines: &[String], needle: &str) -> usize {
    lines
        .iter()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("missing event containing `{needle}` in {lines:?}"))
}

#[tokio::test]
async fn test_tracing_integration_dispatch_chain() {
    let store = init_tracing_capture();
    let cancellation_token = CancellationToken::new();
    let agent = Arc::new(EchoTracingAgent {
        name: "echo-tracing".to_string(),
        delay: Duration::from_millis(5),
        task_cancel: CancellationToken::new(),
    });
    let agent_handle = AgentHandle::spawn(agent, cancellation_token.child_token());

    let mut registry = AgentRegistry::new();
    registry
        .register(
            "echo".to_string(),
            agent_handle,
            vec![Capability("echo".to_string())],
        )
        .expect("echo agent should register");

    let orchestrator = OrchestratorHandle::spawn(registry, cancellation_token.child_token());
    let request_id = RequestId::new();
    let request_id_text = request_id.to_string();

    let result = orchestrator
        .dispatch_with_context(
            "echo",
            AgentMessage::Task {
                id: "task-integration-success".to_string(),
                content: "payload".to_string(),
                metadata: HashMap::new(),
            },
            request_id,
        )
        .await;

    match result {
        Ok(AgentResponse::Success { content, .. }) => {
            assert_eq!(content, "payload");
        }
        other => panic!("expected successful response, got: {other:?}"),
    }

    let chain = request_lines(&store.snapshot(), &request_id_text);

    assert!(
        chain
            .iter()
            .any(|line| line.contains(&format!("event={EVENT_DISPATCH_START}"))),
        "missing dispatch start event for request_id {request_id_text}: {chain:?}"
    );
    assert!(
        chain
            .iter()
            .any(|line| line.contains("event=agent.message.start")),
        "missing agent message start event for request_id {request_id_text}: {chain:?}"
    );
    assert!(
        chain
            .iter()
            .any(|line| line.contains("event=agent.message.end") && line.contains("duration_ms=")),
        "missing agent message end duration event for request_id {request_id_text}: {chain:?}"
    );
    assert!(
        chain.iter().any(
            |line| line.contains(&format!("event={EVENT_DISPATCH_SUCCESS}"))
                && line.contains("duration_ms=")
        ),
        "missing dispatch success event for request_id {request_id_text}: {chain:?}"
    );
    assert!(
        chain
            .iter()
            .all(|line| line.contains(&format!("request_id={request_id_text}"))),
        "request-scoped events must share request_id {request_id_text}: {chain:?}"
    );

    let dispatch_start_idx = find_index(&chain, &format!("event={EVENT_DISPATCH_START}"));
    let agent_start_idx = find_index(&chain, "event=agent.message.start");
    let agent_end_idx = find_index(&chain, "event=agent.message.end");
    let dispatch_success_idx = find_index(&chain, &format!("event={EVENT_DISPATCH_SUCCESS}"));

    assert!(
        dispatch_start_idx < agent_start_idx
            && agent_start_idx < agent_end_idx
            && agent_end_idx < dispatch_success_idx,
        "event order mismatch for request_id {request_id_text}: {chain:?}"
    );

    let agent_start_line = chain
        .iter()
        .find(|line| line.contains("event=agent.message.start"))
        .expect("agent start event should exist");
    assert!(agent_start_line.contains("agent_name=echo-tracing"));
    assert!(agent_start_line.contains("task_id=task-integration-success"));

    cancellation_token.cancel();
}

#[tokio::test]
async fn test_tracing_integration_cancellation_chain() {
    let store = init_tracing_capture();
    let runtime_cancel = CancellationToken::new();
    let task_cancel = CancellationToken::new();
    let agent = Arc::new(EchoTracingAgent {
        name: "echo-tracing".to_string(),
        delay: Duration::from_secs(2),
        task_cancel: task_cancel.clone(),
    });
    let agent_handle = AgentHandle::spawn(agent, runtime_cancel.child_token());

    let mut registry = AgentRegistry::new();
    registry
        .register(
            "echo".to_string(),
            agent_handle,
            vec![Capability("echo".to_string())],
        )
        .expect("echo agent should register");

    let orchestrator = OrchestratorHandle::spawn(registry, runtime_cancel.child_token());
    let request_id = RequestId::new();
    let request_id_text = request_id.to_string();

    let dispatch = orchestrator.dispatch_with_context(
        "echo",
        AgentMessage::Task {
            id: "task-integration-cancelled".to_string(),
            content: "payload".to_string(),
            metadata: HashMap::new(),
        },
        request_id,
    );
    tokio::pin!(dispatch);

    sleep(Duration::from_millis(20)).await;
    task_cancel.cancel();

    let result = dispatch.await;

    match result {
        Ok(AgentResponse::Error { code, .. }) => {
            assert_eq!(code, "cancelled");
        }
        other => panic!("expected cancelled error response, got: {other:?}"),
    }

    let chain = request_lines(&store.snapshot(), &request_id_text);
    assert!(
        chain
            .iter()
            .any(|line| line.contains("event=agent.message.end")
                && line.contains(&format!("outcome={OUTCOME_CANCELLED}"))),
        "missing cancelled outcome event for request_id {request_id_text}: {chain:?}"
    );

    runtime_cancel.cancel();
}
