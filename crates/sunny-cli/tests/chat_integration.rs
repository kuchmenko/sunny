use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use sunny_cli::chat::ChatSession;
use sunny_mind::{
    ChatRole, LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId, StreamEvent,
    StreamResult, TokenUsage,
};
use sunny_store::{Database, SessionStore};
use tempfile::tempdir;
use tokio::sync::Mutex;

struct MockStreamProvider {
    streams: Mutex<VecDeque<Vec<Result<StreamEvent, LlmError>>>>,
    requests: Mutex<Vec<LlmRequest>>,
}

impl MockStreamProvider {
    fn new(streams: Vec<Vec<Result<StreamEvent, LlmError>>>) -> Self {
        Self {
            streams: Mutex::new(VecDeque::from(streams)),
            requests: Mutex::new(Vec::new()),
        }
    }

    async fn request_count(&self) -> usize {
        self.requests.lock().await.len()
    }
}

#[async_trait]
impl LlmProvider for MockStreamProvider {
    fn provider_id(&self) -> &str {
        "mock"
    }

    fn model_id(&self) -> &str {
        "mock-stream"
    }

    async fn chat(&self, _request: LlmRequest) -> Result<LlmResponse, LlmError> {
        Ok(LlmResponse {
            content: "unused".to_string(),
            usage: TokenUsage {
                input_tokens: 1,
                output_tokens: 1,
                total_tokens: 2,
            },
            finish_reason: "stop".to_string(),
            provider_id: ProviderId("mock".to_string()),
            model_id: ModelId("mock-stream".to_string()),
            tool_calls: None,
            reasoning_content: None,
        })
    }

    async fn chat_stream(&self, request: LlmRequest) -> Result<StreamResult, LlmError> {
        self.requests.lock().await.push(request);
        let events =
            self.streams
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| LlmError::InvalidResponse {
                    message: "no mock stream configured".to_string(),
                })?;

        Ok(Box::pin(tokio_stream::iter(events)))
    }
}

fn make_text_stream(text: &str) -> Vec<Result<StreamEvent, LlmError>> {
    vec![
        Ok(StreamEvent::ContentDelta {
            text: text.to_string(),
        }),
        Ok(StreamEvent::Usage {
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
        }),
        Ok(StreamEvent::Done),
    ]
}

#[allow(clippy::arc_with_non_send_sync)]
fn make_store() -> (Arc<SessionStore>, tempfile::TempDir) {
    let dir = tempdir().expect("temp dir");
    let db_path = dir.path().join("chat_integration.db");
    let db = Database::open(&db_path).expect("database should open");
    (Arc::new(SessionStore::new(db)), dir)
}

#[tokio::test]
async fn test_chat_simple_response_no_tools() {
    let provider = Arc::new(MockStreamProvider::new(vec![make_text_stream("4")]));
    let tmp = tempdir().expect("temp dir");
    let (store, _dir) = make_store();
    let mut session = ChatSession::new(
        provider,
        tmp.path().to_path_buf(),
        "pending-session".to_string(),
        store,
    );

    let result = session.send("What is 2+2?", |_| {}).await;

    assert!(result.is_ok(), "send should succeed");
    assert_eq!(result.expect("content should be present"), "4");
}

#[tokio::test]
async fn test_chat_tool_call_flow_fs_read() {
    let tmp = tempdir().expect("create temp dir");
    std::fs::write(tmp.path().join("test.txt"), "hello from file").expect("write fixture file");

    let first_stream = vec![
        Ok(StreamEvent::ToolCallStart {
            id: "tc1".to_string(),
            name: "fs_read".to_string(),
        }),
        Ok(StreamEvent::ToolCallComplete {
            id: "tc1".to_string(),
            name: "fs_read".to_string(),
            arguments: r#"{"path":"test.txt"}"#.to_string(),
        }),
        Ok(StreamEvent::Done),
    ];
    let second_stream = make_text_stream("The file contains: hello from file");

    let provider = Arc::new(MockStreamProvider::new(vec![first_stream, second_stream]));
    let provider_handle = Arc::clone(&provider);
    let (store, _dir) = make_store();
    let mut session = ChatSession::new(
        provider,
        tmp.path().to_path_buf(),
        "pending-session".to_string(),
        store,
    );

    let result = session.send("read the test file", |_| {}).await;

    assert!(result.is_ok(), "send should succeed with tool loop");
    let content = result.expect("tool flow should produce content");
    assert!(content.contains("hello from file"));
    assert!(session.message_count() >= 3);
    assert_eq!(provider_handle.request_count().await, 2);
}

#[tokio::test]
async fn test_chat_multi_turn_conversation_history() {
    let provider = Arc::new(MockStreamProvider::new(vec![
        make_text_stream("first answer"),
        make_text_stream("second answer"),
    ]));
    let tmp = tempdir().expect("temp dir");
    let (store, _dir) = make_store();
    let mut session = ChatSession::new(
        provider,
        tmp.path().to_path_buf(),
        "pending-session".to_string(),
        store,
    );

    let first = session.send("turn 1", |_| {}).await;
    let second = session.send("turn 2", |_| {}).await;

    assert!(first.is_ok());
    assert!(second.is_ok());
    assert_eq!(session.message_count(), 5);
}

#[tokio::test]
async fn test_chat_context_management_trims_old_messages() {
    let big_reply = "r".repeat(250_000);
    let provider = Arc::new(MockStreamProvider::new(vec![
        make_text_stream(&big_reply),
        make_text_stream(&big_reply),
        make_text_stream("trimmed"),
    ]));
    let tmp = tempdir().expect("temp dir");
    let (store, _dir) = make_store();
    let mut session = ChatSession::new(
        provider,
        tmp.path().to_path_buf(),
        "pending-session".to_string(),
        store,
    );

    let first = session.send(&"u".repeat(250_000), |_| {}).await;
    let second = session.send(&"v".repeat(250_000), |_| {}).await;
    assert!(first.is_ok());
    assert!(second.is_ok());

    let chars_before_trim = session.total_content_chars();
    let third = session.send("trigger trim", |_| {}).await;
    let chars_after_trim = session.total_content_chars();

    assert!(third.is_ok());
    assert!(chars_after_trim < chars_before_trim);
    assert_eq!(session.messages()[0].role, ChatRole::System);
}
