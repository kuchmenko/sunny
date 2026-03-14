//! Chat session — manages conversation history and delegates to the streaming tool loop.

use std::path::PathBuf;
use std::sync::Arc;
use std::{collections::HashMap, collections::HashSet};

use sunny_boys::streaming_tool_loop::StreamingToolLoop;
use sunny_boys::tool_loop::{ToolCallError, ToolExecutor};
use sunny_mind::{
    ChatMessage, ChatRole, LlmError, LlmProvider, LlmRequest, StreamEvent, ToolChoice,
};
use sunny_store::{SavedSession, SessionStore, TokenBudget};
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
    budget: sunny_store::TokenBudget,
    provider: Arc<dyn LlmProvider>,
    root: PathBuf,
    cancel: CancellationToken,
    session_id: String,
    #[allow(clippy::arc_with_non_send_sync)]
    store: Arc<SessionStore>,
    is_new_session: bool,
    /// Whether to generate a title after the first exchange.
    /// True for new sessions, false for resumed sessions.
    generate_title: bool,
}

impl ChatSession {
    /// Create a new chat session with the given provider and workspace root.
    ///
    /// Initialises the message history with the Claude Code system prompt,
    /// optionally appending SUNNY.md project context if present.
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        root: PathBuf,
        session_id: String,
        store: Arc<SessionStore>,
    ) -> Self {
        let mut system_prompt = format!(
            "You are Claude Code, Anthropic's official CLI for Claude.\n\n\
             You are an expert software engineer working in the workspace at: {}.\n\n\
             You have access to tools for reading, writing, editing files, executing shell \
             commands, and searching code. Use them to help the user with coding tasks.\n\n\
             Always think carefully before using tools. Prefer targeted tool calls over \
             broad exploration.",
            root.display()
        );

        // Inject SUNNY.md context if available (capped at 12 000 chars total).
        if let Ok(context) = sunny_store::context_file::read_context_files(&root) {
            if !context.is_empty() {
                let truncated: String = context.chars().take(12_000).collect();
                system_prompt.push_str("\n\n# Project Context\n\n");
                system_prompt.push_str(&truncated);
            }
        }

        let messages = vec![ChatMessage {
            role: ChatRole::System,
            content: system_prompt,
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }];

        Self {
            messages,
            budget: TokenBudget::new(200_000, provider.token_counter()),
            provider,
            root,
            cancel: CancellationToken::new(),
            session_id,
            store,
            is_new_session: true,
            generate_title: true,
        }
    }

    /// Reconstruct a session from persisted state.
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
            budget: TokenBudget::new(200_000, provider.token_counter()),
            provider,
            root,
            cancel,
            session_id: saved.id,
            store,
            is_new_session: false,
            generate_title: false,
        }
    }

    /// Return a clone of the cancellation token for external cancellation.
    #[allow(dead_code)]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Return the workspace root path.
    pub fn root(&self) -> &PathBuf {
        &self.root
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

    /// The current session ID.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Send a user message and stream the response.
    ///
    /// Appends the user message to history, runs the streaming tool loop,
    /// appends loop-produced messages, auto-saves, and returns the final text.
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

        // Extend message history with full tool chain from this exchange.
        self.messages.extend(result.messages);

        self.budget.record_usage(&result.usage);

        if self.budget.should_compact() {
            self.compact_tool_outputs();
        }

        // Persist to SQLite in background (don't block conversation).
        self.auto_save().await;

        // Generate a title after the first exchange (non-blocking background task).
        if self.generate_title {
            self.generate_title = false;
            let fallback_title: String = user_input.chars().take(50).collect();
            let _store = Arc::clone(&self.store); // kept for API compatibility, fresh conn used below
            let session_id = self.session_id.clone();
            let provider = Arc::clone(&self.provider);
            let first_msg = user_input.to_string();

            tokio::spawn(async move {
                let title_request = sunny_mind::LlmRequest {
                    messages: vec![sunny_mind::ChatMessage {
                        role: sunny_mind::ChatRole::User,
                        content: format!(
                            "Generate a concise 5-word title for this coding conversation. \
                             Respond with ONLY the title, nothing else.\n\nUser's first message: {first_msg}"
                        ),
                        tool_calls: None,
                        tool_call_id: None,
                        reasoning_content: None,
                    }],
                    max_tokens: Some(20),
                    temperature: Some(0.3),
                    tools: None,
                    tool_choice: None,
                };

                let title = match provider.chat(title_request).await {
                    Ok(response) => {
                        let t = response.content.trim().to_string();
                        if t.is_empty() {
                            fallback_title
                        } else {
                            t
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "title generation failed, using fallback");
                        fallback_title
                    }
                };

                let _ =
                    tokio::task::spawn_blocking(
                        move || match sunny_store::Database::open_default() {
                            Ok(db) => {
                                let s = SessionStore::new(db);
                                if let Err(e) = s.update_title(&session_id, &title) {
                                    warn!(error = %e, "failed to store session title");
                                }
                            }
                            Err(e) => warn!(error = %e, "failed to open db for title update"),
                        },
                    )
                    .await;
            });
        }

        Ok(content)
    }

    /// LLM-based conversation summarization compaction.
    ///
    /// Summarizes messages older than the last 5 turns and replaces them
    /// with a single `[Context Summary]` user message.
    pub async fn compact_with_llm(&mut self) -> Result<String, ChatError> {
        let user_msgs: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == ChatRole::User)
            .map(|(i, _)| i)
            .collect();

        if user_msgs.len() <= 5 {
            return Ok("Not enough messages to compact (need more than 5 turns).".to_string());
        }

        let keep_from = user_msgs[user_msgs.len() - 5];
        if keep_from <= 1 {
            return Ok("Nothing to compact.".to_string());
        }

        let to_summarize = &self.messages[1..keep_from];
        if to_summarize.is_empty() {
            return Ok("Nothing to compact.".to_string());
        }

        let conversation_text: String = to_summarize
            .iter()
            .map(|m| {
                let role = match m.role {
                    ChatRole::User => "User",
                    ChatRole::Assistant => "Assistant",
                    ChatRole::Tool => "Tool",
                    ChatRole::System => "System",
                };
                let content: String = m.content.chars().take(500).collect();
                format!("{role}: {content}\n")
            })
            .collect();

        let before_count = self.messages.len();
        let before_chars: usize = self.messages.iter().map(|m| m.content.len()).sum();

        let summary_request = LlmRequest {
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: format!(
                    "Summarize the following coding conversation. Preserve: key decisions made, \
                     files created/modified, code patterns discussed, and any outstanding tasks. \
                     Be concise.\n\n{conversation_text}"
                ),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            max_tokens: Some(1000),
            temperature: Some(0.3),
            tools: None,
            tool_choice: None,
        };

        let summary = match self.provider.chat(summary_request).await {
            Ok(response) => response.content,
            Err(e) => {
                warn!(error = %e, "summarization failed, skipping llm compaction");
                return Ok(format!("Compaction failed: {e}"));
            }
        };

        let summary_message = ChatMessage {
            role: ChatRole::User,
            content: format!("[Context Summary]\n{summary}"),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        };

        let mut new_messages = Vec::with_capacity(self.messages.len() - to_summarize.len() + 1);
        if let Some(system_prompt) = self.messages.first().cloned() {
            new_messages.push(system_prompt);
        }
        new_messages.push(summary_message);
        new_messages.extend_from_slice(&self.messages[keep_from..]);

        self.messages = new_messages;
        self.budget.reset();

        let after_count = self.messages.len();
        let after_chars: usize = self.messages.iter().map(|m| m.content.len()).sum();

        self.auto_save().await;

        Ok(format!(
            "Compacted: {before_count} -> {after_count} messages, ~{} -> ~{} tokens",
            before_chars / 4,
            after_chars / 4
        ))
    }

    async fn auto_save(&mut self) {
        if self.is_new_session {
            let working_dir = self.root.to_string_lossy().to_string();
            let model = self.provider.model_id().to_string();
            let working_dir_clone = working_dir.clone();
            let model_clone = model.clone();
            let session_id_clone = self.session_id.clone();
            match tokio::task::spawn_blocking(move || {
                // Open a fresh DB connection (rusqlite::Connection is not Send).
                let db = sunny_store::Database::open_default()
                    .map_err(|e| sunny_store::StoreError::Migration(e.to_string()))?;
                let s = SessionStore::new(db);
                match s.load_session(&session_id_clone) {
                    Ok(None) => s
                        .create_session(&working_dir_clone, Some(&model_clone))
                        .map(|_| ()),
                    Ok(Some(_)) => Ok(()),
                    Err(e) => Err(e),
                }
            })
            .await
            {
                Ok(Ok(())) => {
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
        tokio::task::spawn_blocking(move || match sunny_store::Database::open_default() {
            Ok(db) => {
                let s = SessionStore::new(db);
                if let Err(e) = s.save_messages(&session_id, &messages) {
                    warn!(error = %e, "failed to auto-save session messages");
                }
            }
            Err(e) => warn!(error = %e, "failed to open db for auto-save: {e}"),
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

    fn compact_tool_outputs(&mut self) {
        const READ_ONLY_TOOLS: &[&str] = &[
            "fs_read",
            "fs_scan",
            "text_grep",
            "grep_files",
            "git_log",
            "git_diff",
            "git_status",
        ];
        const WRITE_TOOLS: &[&str] = &["fs_write", "fs_edit", "shell_exec"];

        let read_only_tools: HashSet<&str> = READ_ONLY_TOOLS.iter().copied().collect();
        let write_tools: HashSet<&str> = WRITE_TOOLS.iter().copied().collect();

        let user_msg_indices: Vec<usize> = self
            .messages
            .iter()
            .enumerate()
            .filter(|(_, msg)| msg.role == ChatRole::User)
            .map(|(index, _)| index)
            .collect();

        let recent_cutoff = if user_msg_indices.len() > 3 {
            user_msg_indices[user_msg_indices.len() - 3]
        } else {
            0
        };

        let mut tool_name_by_call_id: HashMap<String, String> = HashMap::new();
        for msg in &self.messages {
            if msg.role != ChatRole::Assistant {
                continue;
            }

            if let Some(tool_calls) = &msg.tool_calls {
                for tool_call in tool_calls {
                    tool_name_by_call_id.insert(tool_call.id.clone(), tool_call.name.clone());
                }
            }
        }

        let mut compacted_count = 0usize;
        for (index, msg) in self.messages.iter_mut().enumerate() {
            if msg.role != ChatRole::Tool || index >= recent_cutoff || msg.content.len() <= 200 {
                continue;
            }

            let Some(call_id) = msg.tool_call_id.as_ref() else {
                continue;
            };
            let Some(tool_name) = tool_name_by_call_id.get(call_id) else {
                continue;
            };

            if write_tools.contains(tool_name.as_str())
                || !read_only_tools.contains(tool_name.as_str())
            {
                continue;
            }

            let original_len = msg.content.len();
            msg.content = format!("[Compacted: {tool_name} result, {original_len} chars]");
            compacted_count += 1;
        }

        if compacted_count > 0 {
            info!(
                operation = "compact_tool_outputs",
                compacted_count, "compacted old tool output messages"
            );
            self.budget.reset();
        }

        self.trim_context();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::arc_with_non_send_sync)]
    use super::*;
    use std::collections::VecDeque;

    use async_trait::async_trait;
    use sunny_mind::{
        LlmError, LlmRequest, LlmResponse, ModelId, ProviderId, StreamEvent, StreamResult,
        TokenUsage, ToolCall,
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
    fn make_session_in(dir: &tempfile::TempDir) -> ChatSession {
        let db = Database::open(dir.path().join("session_test.db").as_path())
            .expect("should open database");
        let store = Arc::new(SessionStore::new(db));
        let provider = Arc::new(MockStreamProvider::new(vec![]));
        ChatSession::new(
            provider,
            dir.path().to_path_buf(),
            "pending-session".to_string(),
            store,
        )
    }

    #[tokio::test]
    async fn test_chat_session_new_has_system_message() {
        let dir = tempdir().expect("should create temp dir");
        let session = make_session_in(&dir);
        assert_eq!(session.messages.len(), 1);
        assert_eq!(session.messages[0].role, ChatRole::System);
        assert!(session.messages[0]
            .content
            .contains("You are Claude Code, Anthropic's official CLI for Claude."));
    }

    #[tokio::test]
    async fn test_session_has_session_id() {
        let dir = tempdir().expect("should create temp dir");
        let session = make_session_in(&dir);
        assert!(!session.session_id.is_empty());
    }

    #[tokio::test]
    async fn test_generate_title_flag_set_on_new_session() {
        let dir = tempdir().expect("should create temp dir");
        let session = make_session_in(&dir);
        assert!(session.generate_title, "new session should generate title");
    }

    #[tokio::test]
    async fn test_generate_title_false_on_from_saved() {
        let dir = tempdir().expect("should create temp dir");
        let db = Database::open(dir.path().join("test.db").as_path()).expect("open db");
        let store = Arc::new(SessionStore::new(db));
        let saved = store
            .create_session("/test", Some("model"))
            .expect("create session");
        let provider = Arc::new(MockStreamProvider::new(vec![]));
        let session = ChatSession::from_saved(
            store,
            saved,
            vec![],
            provider,
            dir.path().to_path_buf(),
            CancellationToken::new(),
        );
        assert!(
            !session.generate_title,
            "resumed session should not generate title"
        );
    }

    #[tokio::test]
    async fn test_session_from_saved_restores_messages() {
        let dir = tempdir().expect("should create temp dir");
        let db = Database::open(dir.path().join("test.db").as_path()).expect("open db");
        let store = Arc::new(SessionStore::new(db));
        let saved = store
            .create_session("/test", Some("model"))
            .expect("create session");
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
        let provider = Arc::new(MockStreamProvider::new(vec![]));
        let session = ChatSession::from_saved(
            store,
            saved,
            messages,
            provider,
            dir.path().to_path_buf(),
            CancellationToken::new(),
        );
        assert_eq!(session.message_count(), 3);
    }

    #[tokio::test]
    async fn test_chat_session_send_appends_messages() {
        let dir = tempdir().expect("should create temp dir");
        let db = Database::open(dir.path().join("test.db").as_path()).expect("open db");
        let store = Arc::new(SessionStore::new(db));
        let provider = Arc::new(MockStreamProvider::new(vec![make_text_stream("Hello!")]));
        let mut session = ChatSession::new(
            provider,
            dir.path().to_path_buf(),
            "pending-session".to_string(),
            store,
        );

        let result = session.send("hi", |_| {}).await;
        assert!(result.is_ok(), "send should succeed");
        // system + user + assistant = 3
        assert_eq!(session.messages.len(), 3);
        assert_eq!(session.messages[1].role, ChatRole::User);
        assert_eq!(session.messages[2].role, ChatRole::Assistant);
    }

    #[tokio::test]
    async fn test_chat_session_send_returns_content() {
        let dir = tempdir().expect("should create temp dir");
        let db = Database::open(dir.path().join("test.db").as_path()).expect("open db");
        let store = Arc::new(SessionStore::new(db));
        let provider = Arc::new(MockStreamProvider::new(vec![make_text_stream(
            "Hello world",
        )]));
        let mut session = ChatSession::new(
            provider,
            dir.path().to_path_buf(),
            "pending-session".to_string(),
            store,
        );

        let content = session.send("hi", |_| {}).await.unwrap();
        assert_eq!(content, "Hello world");
    }

    #[tokio::test]
    async fn test_chat_session_cancel_resets_token() {
        let dir = tempdir().expect("should create temp dir");
        let mut session = make_session_in(&dir);

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
        let dir = tempdir().expect("should create temp dir");
        let db = Database::open(dir.path().join("test.db").as_path()).expect("open db");
        let store = Arc::new(SessionStore::new(db));
        let provider = Arc::new(MockStreamProvider::new(vec![make_text_stream("ok")]));
        let mut session = ChatSession::new(
            provider,
            dir.path().to_path_buf(),
            "pending-session".to_string(),
            store,
        );

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

    #[tokio::test]
    async fn test_sunny_md_injected_into_system_prompt() {
        let dir = tempdir().expect("should create temp dir");
        // Write SUNNY.md to the tempdir
        let sunny_md = dir.path().join("SUNNY.md");
        std::fs::write(&sunny_md, "Use async/await everywhere").expect("write SUNNY.md");

        let db = Database::open(dir.path().join("test.db").as_path()).expect("open db");
        let store = Arc::new(SessionStore::new(db));
        let provider = Arc::new(MockStreamProvider::new(vec![]));
        let session = ChatSession::new(
            provider,
            dir.path().to_path_buf(),
            "pending-session".to_string(),
            store,
        );

        assert!(
            session.messages[0]
                .content
                .contains("Use async/await everywhere"),
            "system prompt should contain SUNNY.md content"
        );
    }

    fn user_message(content: &str) -> ChatMessage {
        ChatMessage {
            role: ChatRole::User,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn assistant_tool_call_message(call_id: &str, tool_name: &str) -> ChatMessage {
        ChatMessage {
            role: ChatRole::Assistant,
            content: String::new(),
            tool_calls: Some(vec![ToolCall {
                id: call_id.to_string(),
                name: tool_name.to_string(),
                arguments: "{}".to_string(),
                execution_depth: 0,
            }]),
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn tool_result_message(call_id: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: ChatRole::Tool,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: Some(call_id.to_string()),
            reasoning_content: None,
        }
    }

    fn assistant_text_message(content: &str) -> ChatMessage {
        ChatMessage {
            role: ChatRole::Assistant,
            content: content.to_string(),
            tool_calls: None,
            tool_call_id: None,
            reasoning_content: None,
        }
    }

    fn push_tool_turn(
        messages: &mut Vec<ChatMessage>,
        user: &str,
        call_id: &str,
        tool_name: &str,
        tool_output: &str,
    ) {
        messages.push(user_message(user));
        messages.push(assistant_tool_call_message(call_id, tool_name));
        messages.push(tool_result_message(call_id, tool_output));
        messages.push(assistant_text_message("done"));
    }

    fn tool_content_for_call(messages: &[ChatMessage], call_id: &str) -> Option<String> {
        messages
            .iter()
            .find(|m| m.role == ChatRole::Tool && m.tool_call_id.as_deref() == Some(call_id))
            .map(|m| m.content.clone())
    }

    #[tokio::test]
    async fn test_compact_tool_outputs_replaces_old_results() {
        let dir = tempdir().expect("should create temp dir");
        let mut session = make_session_in(&dir);
        let long_output = "r".repeat(240);

        push_tool_turn(
            &mut session.messages,
            "turn-1",
            "call-old-read",
            "fs_read",
            &long_output,
        );
        push_tool_turn(
            &mut session.messages,
            "turn-2",
            "call-old-write",
            "fs_write",
            &long_output,
        );
        push_tool_turn(
            &mut session.messages,
            "turn-3",
            "call-recent-read-1",
            "git_diff",
            &long_output,
        );
        push_tool_turn(
            &mut session.messages,
            "turn-4",
            "call-recent-read-2",
            "fs_scan",
            &long_output,
        );
        push_tool_turn(
            &mut session.messages,
            "turn-5",
            "call-recent-read-3",
            "text_grep",
            &long_output,
        );

        session.compact_tool_outputs();

        let old_read = tool_content_for_call(&session.messages, "call-old-read")
            .expect("old read tool result");
        let old_write = tool_content_for_call(&session.messages, "call-old-write")
            .expect("old write tool result");
        let recent_read = tool_content_for_call(&session.messages, "call-recent-read-1")
            .expect("recent read tool result");

        assert!(
            old_read.starts_with("[Compacted: fs_read result, 240 chars]"),
            "old read-only tool results should be compacted"
        );
        assert_eq!(
            old_write, long_output,
            "write tool results should be preserved regardless of age"
        );
        assert_eq!(
            recent_read, long_output,
            "tool results from the last 3 turns should be preserved"
        );
    }

    #[tokio::test]
    async fn test_budget_tracks_usage() {
        let dir = tempdir().expect("should create temp dir");
        let db = Database::open(dir.path().join("test.db").as_path()).expect("open db");
        let store = Arc::new(SessionStore::new(db));
        let provider = Arc::new(MockStreamProvider::new(vec![make_text_stream("budget")]));
        let mut session = ChatSession::new(
            provider,
            dir.path().to_path_buf(),
            "pending-session".to_string(),
            store,
        );

        let _ = session
            .send("track usage", |_| {})
            .await
            .expect("send should work");

        assert_eq!(
            session.budget.consumed_tokens(),
            15,
            "budget should accumulate usage from send()"
        );
    }

    #[tokio::test]
    async fn test_compact_with_llm_reduces_messages() {
        let dir = tempdir().expect("should create temp dir");
        let mut session = make_session_in(&dir);

        for turn in 1..=6 {
            session
                .messages
                .push(user_message(&format!("user-turn-{turn}")));
            session
                .messages
                .push(assistant_text_message(&format!("assistant-turn-{turn}")));
        }

        let before_count = session.messages.len();
        let result = session
            .compact_with_llm()
            .await
            .expect("compaction should succeed");

        assert!(result.starts_with("Compacted:"));
        assert!(session.messages.len() < before_count);
        assert_eq!(session.messages[0].role, ChatRole::System);
        assert_eq!(session.messages[1].role, ChatRole::User);
        assert!(session.messages[1]
            .content
            .starts_with("[Context Summary]\n"));

        let preserved_users: Vec<&str> = session
            .messages
            .iter()
            .filter(|m| m.role == ChatRole::User)
            .skip(1)
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(
            preserved_users,
            vec![
                "user-turn-2",
                "user-turn-3",
                "user-turn-4",
                "user-turn-5",
                "user-turn-6"
            ],
            "last 5 user turns should be preserved"
        );
    }

    #[tokio::test]
    async fn test_compact_skips_small_sessions() {
        let dir = tempdir().expect("should create temp dir");
        let mut session = make_session_in(&dir);

        for turn in 1..=5 {
            session
                .messages
                .push(user_message(&format!("user-turn-{turn}")));
            session
                .messages
                .push(assistant_text_message(&format!("assistant-turn-{turn}")));
        }

        let before = session.messages.clone();
        let message = session
            .compact_with_llm()
            .await
            .expect("compaction should return skip message");

        assert_eq!(
            message,
            "Not enough messages to compact (need more than 5 turns)."
        );
        assert_eq!(session.messages, before);
    }
}
