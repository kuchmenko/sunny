use std::path::PathBuf;
use std::sync::Arc;

use sunny_boys::streaming_tool_loop::StreamingToolLoop;
use sunny_boys::tool_loop::{ToolCallError, ToolExecutor};
use sunny_mind::{
    ChatMessage, ChatRole, LlmError, LlmProvider, LlmRequest, StreamEvent, ToolChoice,
};
use sunny_store::{SavedSession, SessionStore};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::tools::{build_tool_definitions, build_tool_executor, build_tool_policy};

/// Maximum character budget for the conversation context.
///
/// Uses a 4 chars ≈ 1 token heuristic with a ~190K token model limit.
const MAX_CONTEXT_CHARS: usize = 190_000 * 4;

/// Errors that can occur during a chat session.
#[derive(thiserror::Error, Debug)]
pub enum ChatError {
    #[error("LLM error: {0}")]
    Llm(#[from] LlmError),
    #[error("tool loop error: {0}")]
    ToolLoop(#[from] ToolCallError),
    #[error("cancelled")]
    #[allow(dead_code)]
    Cancelled,
}

/// In-memory conversation session for the `sunny chat` REPL.
///
/// `ChatSession` is NOT an agent — it does not implement the `Agent` trait.
/// It manages conversation history and delegates streaming tool execution
/// to [`StreamingToolLoop`].
pub struct ChatSession {
    messages: Vec<ChatMessage>,
    provider: Arc<dyn LlmProvider>,
    root: PathBuf,
    cancel: CancellationToken,
    session_id: String,
    store: Arc<SessionStore>,
    is_new_session: bool,
}

impl ChatSession {
    /// Create a new chat session with the given provider and workspace root.
    ///
    /// Initialises the message history with the Claude Code system prompt.
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        root: PathBuf,
        session_id: String,
        store: Arc<SessionStore>,
    ) -> Self {
        let system_prompt = format!(
            "You are Claude Code, Anthropic's official CLI for Claude.\n\n\
             You are an expert software engineer working in the workspace at: {}.\n\n\
             You have access to tools for reading, writing, editing files, executing shell \
             commands, and searching code. Use them to help the user with coding tasks.\n\n\
             Always think carefully before using tools. Prefer targeted tool calls over \
             broad exploration.",
            root.display()
        );

        let messages = vec![ChatMessage {
            role: ChatRole::System,
            content: system_prompt,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }];

        Self {
            messages,
            provider,
            root,
            cancel: CancellationToken::new(),
            session_id,
            store,
            is_new_session: true,
        }
    }

    pub fn from_saved(
        store: Arc<SessionStore>,
        saved: SavedSession,
        messages: Vec<ChatMessage>,
        provider: Arc<dyn LlmProvider>,
        root: PathBuf,
        cancel: CancellationToken,
    ) -> Self {
        let current_dir = root.to_string_lossy();
        if saved.working_dir != current_dir.as_ref() {
            warn!(
                stored_dir = %saved.working_dir,
                current_dir = %current_dir,
                "session was created in a different directory"
            );
        }

        Self {
            messages,
            provider,
            root,
            cancel,
            session_id: saved.id,
            store,
            is_new_session: false,
        }
    }

    /// Return a clone of the cancellation token for external cancellation.
    #[allow(dead_code)]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Cancel the current request and reset the token for the next one.
    pub fn cancel_current(&mut self) {
        self.cancel.cancel();
        self.cancel = CancellationToken::new();
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub fn total_content_chars(&self) -> usize {
        self.messages.iter().map(|m| m.content.len()).sum()
    }

    /// Send a user message and stream the response.
    ///
    /// Appends the user message to history, runs the streaming tool loop,
    /// appends loop-produced messages, and returns the final text content.
    pub async fn send<F>(&mut self, user_input: &str, on_event: F) -> Result<String, ChatError>
    where
        F: Fn(StreamEvent) + Send + Sync,
    {
        self.messages.push(ChatMessage {
            role: ChatRole::User,
            content: user_input.to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });

        self.trim_context();

        let request = LlmRequest {
            messages: self.messages.clone(),
            max_tokens: Some(8192),
            temperature: None,
            tools: Some(build_tool_definitions()),
            tool_choice: Some(ToolChoice::Auto),
        };

        let tool_executor: Arc<ToolExecutor> = build_tool_executor(self.root.clone());
        let loop_runner = StreamingToolLoop::new(
            Arc::clone(&self.provider),
            build_tool_policy(),
            15,
            self.cancel.clone(),
        );

        let result = loop_runner.run(request, tool_executor, on_event).await?;
        let content = result.content.clone();
        self.messages.extend(result.messages);
        self.auto_save().await;

        Ok(content)
    }

    async fn auto_save(&mut self) {
        let _store_refs = Arc::strong_count(&self.store);

        if self.is_new_session {
            let working_dir = self.root.to_string_lossy().to_string();
            let model = self.provider.model_id().to_string();
            match tokio::task::spawn_blocking(move || {
                let db = match sunny_store::Database::open_default() {
                    Ok(db) => db,
                    Err(e) => return Err(e),
                };
                let store = SessionStore::new(db);
                store.create_session(&working_dir, Some(&model))
            })
            .await
            {
                Ok(Ok(saved)) => {
                    self.session_id = saved.id;
                    self.is_new_session = false;
                }
                Ok(Err(e)) => {
                    warn!(error = %e, "failed to create session record");
                    return;
                }
                Err(e) => {
                    warn!(error = %e, "failed to join session create task");
                    return;
                }
            }
        }

        let session_id = self.session_id.clone();
        let messages = self.messages.clone();
        tokio::task::spawn_blocking(move || {
            let db = match sunny_store::Database::open_default() {
                Ok(db) => db,
                Err(e) => {
                    warn!(error = %e, "failed to open session store for auto-save");
                    return;
                }
            };
            let store = SessionStore::new(db);
            if let Err(e) = store.save_messages(&session_id, &messages) {
                warn!(error = %e, "failed to auto-save session messages");
            }
        });
    }

    /// Trim oldest non-system messages when the conversation exceeds the
    /// character budget (4 chars ≈ 1 token, ~190K token limit).
    fn trim_context(&mut self) {
        let total_chars: usize = self.messages.iter().map(|m| m.content.len()).sum();
        if total_chars <= MAX_CONTEXT_CHARS {
            return;
        }

        info!(
            operation = "context_trim",
            total_chars = total_chars,
            limit = MAX_CONTEXT_CHARS,
            "trimming conversation context to fit model context window"
        );

        // Keep system message (index 0) and trim oldest user/assistant pairs.
        while self.messages.len() > 2 {
            let chars: usize = self.messages.iter().map(|m| m.content.len()).sum();
            if chars <= MAX_CONTEXT_CHARS {
                break;
            }
            // Remove the oldest non-system message (index 1).
            self.messages.remove(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    use async_trait::async_trait;
    use sunny_mind::{
        LlmError, LlmRequest, LlmResponse, ModelId, ProviderId, StreamEvent, StreamResult,
        TokenUsage,
    };
    use sunny_store::Database;
    use tempfile::tempdir;
    use tokio::sync::Mutex;

    struct MockStreamProvider {
        streams: Mutex<VecDeque<Vec<Result<StreamEvent, LlmError>>>>,
    }

    impl MockStreamProvider {
        fn new(streams: Vec<Vec<Result<StreamEvent, LlmError>>>) -> Self {
            Self {
                streams: Mutex::new(VecDeque::from(streams)),
            }
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
                content: "mock response".to_string(),
                usage: TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    total_tokens: 2,
                },
                finish_reason: "end_turn".to_string(),
                provider_id: ProviderId("mock".to_string()),
                model_id: ModelId("mock-stream".to_string()),
                tool_calls: None,
                reasoning_content: None,
            })
        }

        async fn chat_stream(&self, _request: LlmRequest) -> Result<StreamResult, LlmError> {
            let mut streams = self.streams.lock().await;
            let events = streams
                .pop_front()
                .unwrap_or_else(|| vec![Ok(StreamEvent::Done)]);
            // Convert the Vec into a stream using tokio_stream::iter.
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
        let dir = tempdir().expect("should create temp dir");
        let db_path = dir.path().join("session_test.db");
        let db = Database::open(&db_path).expect("should open database");
        (Arc::new(SessionStore::new(db)), dir)
    }

    #[tokio::test]
    async fn test_chat_session_new_has_system_message() {
        let provider = Arc::new(MockStreamProvider::new(vec![]));
        let root = std::env::temp_dir();
        let (store, _dir) = make_store();
        let session = ChatSession::new(provider, root, "pending-session".to_string(), store);
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].role, ChatRole::System);
        assert!(session.messages[0]
            .content
            .contains("You are Claude Code, Anthropic's official CLI for Claude."));
    }

    #[tokio::test]
    async fn test_session_has_session_id() {
        let provider = Arc::new(MockStreamProvider::new(vec![]));
        let root = std::env::temp_dir();
        let (store, _dir) = make_store();
        let session = ChatSession::new(provider, root, "pending-session".to_string(), store);
        assert!(!session.session_id.is_empty());
    }

    #[tokio::test]
    async fn test_session_from_saved_restores_messages() {
        let provider = Arc::new(MockStreamProvider::new(vec![]));
        let root = std::env::temp_dir();
        let (store, _dir) = make_store();
        let working_dir = root.to_string_lossy().to_string();
        let saved = store
            .create_session(&working_dir, Some(provider.model_id()))
            .expect("should create session");
        let messages = vec![
            ChatMessage {
                role: ChatRole::System,
                content: "sys".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: ChatRole::User,
                content: "hi".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: ChatRole::Assistant,
                content: "hello".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ];
        let session = ChatSession::from_saved(
            Arc::clone(&store),
            saved,
            messages,
            provider,
            root,
            CancellationToken::new(),
        );
        assert_eq!(session.message_count(), 3);
    }

    #[tokio::test]
    async fn test_chat_session_send_appends_messages() {
        let provider = Arc::new(MockStreamProvider::new(vec![make_text_stream("Hello!")]));
        let root = std::env::temp_dir();
        let (store, _dir) = make_store();
        let mut session = ChatSession::new(provider, root, "pending-session".to_string(), store);

        let result = session.send("hi", |_| {}).await;
        assert!(result.is_ok(), "send should succeed");
        // system + user + assistant = 3
        assert_eq!(session.messages.len(), 3);
        assert_eq!(session.messages[1].role, ChatRole::User);
        assert_eq!(session.messages[2].role, ChatRole::Assistant);
    }

    #[tokio::test]
    async fn test_chat_session_send_returns_content() {
        let provider = Arc::new(MockStreamProvider::new(vec![make_text_stream(
            "Hello world",
        )]));
        let root = std::env::temp_dir();
        let (store, _dir) = make_store();
        let mut session = ChatSession::new(provider, root, "pending-session".to_string(), store);

        let content = session.send("hi", |_| {}).await.unwrap();
        assert_eq!(content, "Hello world");
    }

    #[tokio::test]
    async fn test_chat_session_cancel_resets_token() {
        let provider = Arc::new(MockStreamProvider::new(vec![]));
        let root = std::env::temp_dir();
        let (store, _dir) = make_store();
        let mut session = ChatSession::new(provider, root, "pending-session".to_string(), store);

        let token_before = session.cancellation_token();
        session.cancel_current();
        assert!(token_before.is_cancelled(), "old token should be cancelled");
        assert!(
            !session.cancel.is_cancelled(),
            "new token should not be cancelled"
        );
    }

    #[tokio::test]
    async fn test_chat_session_context_trim() {
        let provider = Arc::new(MockStreamProvider::new(vec![make_text_stream("ok")]));
        let root = std::env::temp_dir();
        let (store, _dir) = make_store();
        let mut session = ChatSession::new(provider, root, "pending-session".to_string(), store);

        // Add messages to push over MAX_CONTEXT_CHARS (760_000 chars).
        let large_msg = "x".repeat(200_000);
        for role in [ChatRole::User, ChatRole::Assistant, ChatRole::User] {
            session.messages.push(ChatMessage {
                role,
                content: large_msg.clone(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            });
        }
        // Add one more large message to push total over 760_000.
        session.messages.push(ChatMessage {
            role: ChatRole::Assistant,
            content: "y".repeat(600_000),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        });

        let count_before = session.messages.len();
        session.trim_context();
        let count_after = session.messages.len();

        assert!(
            count_after < count_before,
            "trim should have removed messages ({count_before} -> {count_after})"
        );
        // System message must always be kept.
        assert_eq!(session.messages[0].role, ChatRole::System);
    }
}
