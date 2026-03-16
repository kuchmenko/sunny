use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use sunny_core::tool::ToolPolicy;
use sunny_mind::{
    ChatMessage, ChatRole, LlmError, LlmProvider, LlmRequest, StreamEvent, TokenUsage, ToolCall,
};
use tokio::time::timeout;
use tokio_stream::StreamExt;
use tokio_util::sync::CancellationToken;
use tracing::{info_span, Instrument};

use crate::timeouts::{tool_call_timeout, tool_provider_timeout};
use crate::tool_loop::{
    is_recoverable_tool_error, recoverable_error_fingerprint, recoverable_error_kind,
    recoverable_tool_error_payload, ToolCallError, ToolExecutor, MAX_RECOVERABLE_ERROR_STREAK,
};

// ADR: StreamingToolResult.messages addition (class L - public API change)
//
// Context: Session persistence requires the full message history including intermediate
// tool call/result messages, not just the final text content.
// Decision: StreamingToolResult now includes `messages: Vec<ChatMessage>` containing
// all messages generated during the tool loop (assistant+tool_calls + tool results + final assistant).
// Consequences: Callers receive the complete history; session.rs uses this to extend
// self.messages instead of manually constructing the final assistant message.

#[derive(Debug, Clone, Default)]
pub struct StreamingToolMetrics {
    pub iterations: u32,
    pub total_tool_calls: u32,
    pub tools_by_name: HashMap<String, u32>,
    pub total_content_events: u32,
}

pub struct StreamingToolResult {
    pub content: String,
    pub metrics: StreamingToolMetrics,
    pub usage: TokenUsage,
    pub messages: Vec<ChatMessage>,
}

pub struct StreamingToolLoop<P: LlmProvider + ?Sized> {
    provider: Arc<P>,
    policy: ToolPolicy,
    max_iterations: usize,
    cancel: CancellationToken,
    tool_timeout: Duration,
    provider_timeout: Duration,
    dedup_tools: HashSet<String>,
    /// Per-tool timeout overrides. Tools not listed use `tool_timeout`.
    tool_timeouts: HashMap<String, Duration>,
}

impl<P: LlmProvider + ?Sized> StreamingToolLoop<P> {
    pub fn new(
        provider: Arc<P>,
        policy: ToolPolicy,
        max_iterations: usize,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            provider,
            policy,
            max_iterations,
            cancel,
            tool_timeout: tool_call_timeout(),
            provider_timeout: tool_provider_timeout(),
            dedup_tools: HashSet::new(),
            tool_timeouts: HashMap::new(),
        }
    }

    pub fn with_dedup_tools(mut self, tools: HashSet<String>) -> Self {
        self.dedup_tools = tools;
        self
    }

    /// Register per-tool timeout overrides (e.g. `Duration::MAX` for human-interaction tools).
    pub fn with_tool_timeouts(mut self, timeouts: HashMap<String, Duration>) -> Self {
        self.tool_timeouts = timeouts;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_tool_timeout(mut self, timeout: Duration) -> Self {
        self.tool_timeout = timeout;
        self
    }

    #[cfg(test)]
    pub(crate) fn with_provider_timeout(mut self, timeout: Duration) -> Self {
        self.provider_timeout = timeout;
        self
    }

    pub async fn run<F>(
        &self,
        request: LlmRequest,
        tool_executor: Arc<ToolExecutor>,
        mut on_event: F,
    ) -> Result<StreamingToolResult, ToolCallError>
    where
        F: FnMut(StreamEvent) + Send,
    {
        let mut current_request = request;
        let mut metrics = StreamingToolMetrics::default();
        let mut usage = TokenUsage {
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
        };
        let mut content = String::new();
        let mut result_messages: Vec<ChatMessage> = Vec::new();
        let mut depth = 0usize;
        let mut last_recoverable_error: Option<String> = None;
        let mut recoverable_error_streak = 0usize;
        let mut dedup_cache: HashMap<(String, String), String> = HashMap::new();

        loop {
            if self.cancel.is_cancelled() {
                return Err(ToolCallError::Cancelled);
            }

            if metrics.iterations as usize >= self.max_iterations {
                return Err(ToolCallError::MaxIterationsReached {
                    count: metrics.iterations as usize,
                });
            }

            let iteration = metrics.iterations;
            let iteration_result = async {
                metrics.iterations += 1;

                let mut stream = tokio::select! {
                    _ = self.cancel.cancelled() => return Err(ToolCallError::Cancelled),
                    response = timeout(self.provider_timeout, self.provider.chat_stream(current_request.clone())) => {
                        match response {
                            Ok(Ok(stream)) => stream,
                            Ok(Err(source)) => return Err(ToolCallError::Llm { source }),
                            Err(_) => return Err(ToolCallError::ProviderTimeout {
                                timeout_secs: self.provider_timeout.as_secs(),
                            }),
                        }
                    }
                };

                let mut iteration_content = String::new();
                let mut pending_tool_calls = Vec::new();
                let mut started_tool_calls: HashMap<String, String> = HashMap::new();

                while let Some(item) = tokio::select! {
                    _ = self.cancel.cancelled() => return Err(ToolCallError::Cancelled),
                    event = stream.next() => event,
                } {
                    let event = item.map_err(|source| ToolCallError::Llm { source })?;
                    match event {
                        StreamEvent::ContentDelta { text } => {
                            metrics.total_content_events += 1;
                            iteration_content.push_str(&text);
                            content.push_str(&text);
                            on_event(StreamEvent::ContentDelta { text });
                        }
                        StreamEvent::ThinkingDelta { text } => {
                            on_event(StreamEvent::ThinkingDelta { text });
                        }
                        StreamEvent::ToolCallStart { id, name } => {
                            on_event(StreamEvent::ToolCallStart { id: id.clone(), name: name.clone() });
                            started_tool_calls.insert(id, name);
                        }
                        StreamEvent::ToolCallDelta { .. } => {}
                        StreamEvent::ToolCallComplete {
                            id,
                            name,
                            arguments,
                        } => {
                            on_event(StreamEvent::ToolCallComplete {
                                id: id.clone(),
                                name: name.clone(),
                                arguments: arguments.clone(),
                            });
                            let _ = started_tool_calls.remove(&id);
                            pending_tool_calls.push(ToolCall {
                                id,
                                name,
                                arguments,
                                execution_depth: depth,
                            });
                        }
                        StreamEvent::Usage { usage: stream_usage } => {
                            on_event(StreamEvent::Usage { usage: stream_usage.clone() });
                            usage.input_tokens += stream_usage.input_tokens;
                            usage.output_tokens += stream_usage.output_tokens;
                            usage.total_tokens += stream_usage.total_tokens;
                        }
                        StreamEvent::Error { message } => {
                            on_event(StreamEvent::Error { message: message.clone() });
                            return Err(ToolCallError::Llm {
                                source: LlmError::InvalidResponse { message },
                            });
                        }
                        StreamEvent::Done => break,
                    }
                }

                Ok::<_, ToolCallError>((iteration_content, pending_tool_calls))
            }
            .instrument(info_span!(
                "streaming_tool_call_iteration",
                iteration,
                depth,
                event = sunny_core::events::EVENT_TOOL_EXEC_DEPTH,
            ))
            .await?;

            let (iteration_content, pending_tool_calls) = iteration_result;

            if pending_tool_calls.is_empty() {
                let final_assistant_message = ChatMessage {
                    role: ChatRole::Assistant,
                    content: content.clone(),
                    tool_calls: None,
                    tool_call_id: None,
                    reasoning_content: None,
                };
                result_messages.push(final_assistant_message);

                return Ok(StreamingToolResult {
                    content,
                    metrics,
                    usage,
                    messages: result_messages,
                });
            }

            metrics.total_tool_calls += pending_tool_calls.len() as u32;
            for tool_call in &pending_tool_calls {
                *metrics
                    .tools_by_name
                    .entry(tool_call.name.clone())
                    .or_insert(0) += 1;
            }

            let mut tool_results = Vec::with_capacity(pending_tool_calls.len());
            for mut tool_call in pending_tool_calls.clone() {
                if !self.policy.is_allowed(&tool_call.name) {
                    return Err(ToolCallError::PolicyViolation {
                        tool_name: tool_call.name,
                    });
                }

                tool_call.execution_depth = depth;

                let executor = tool_executor.clone();
                let call_id = tool_call.id.clone();
                let call_name = tool_call.name.clone();
                let call_arguments = tool_call.arguments.clone();
                let dedup_key = (tool_call.name.clone(), tool_call.arguments.clone());
                if self.dedup_tools.contains(&tool_call.name) {
                    if let Some(cached) = dedup_cache.get(&dedup_key) {
                        tracing::debug!(
                            event = "tool.dedup.cache_hit",
                            tool_name = %tool_call.name,
                            "returning cached result"
                        );
                        tool_results.push((tool_call, cached.clone()));
                        continue;
                    }
                }
                let effective_timeout = self
                    .tool_timeouts
                    .get(&call_name)
                    .copied()
                    .unwrap_or(self.tool_timeout);
                let tool_result = timeout(effective_timeout, async move {
                    tokio::task::spawn_blocking(move || {
                        executor(&call_id, &call_name, &call_arguments, depth)
                    })
                    .await
                    .map_err(|join_err| {
                        sunny_core::tool::ToolError::ExecutionFailed {
                            source: Box::new(std::io::Error::other(format!(
                                "tool execution task failed: {join_err}"
                            ))),
                        }
                    })?
                });

                match tokio::select! {
                    _ = self.cancel.cancelled() => return Err(ToolCallError::Cancelled),
                    tool_result = tool_result => tool_result,
                } {
                    Ok(Ok(result)) => {
                        last_recoverable_error = None;
                        recoverable_error_streak = 0;
                        if self.dedup_tools.contains(&tool_call.name) {
                            dedup_cache.insert(dedup_key, result.clone());
                        }
                        tool_results.push((tool_call, result));
                    }
                    Ok(Err(source)) => {
                        if is_recoverable_tool_error(&source) {
                            let fingerprint =
                                recoverable_error_fingerprint(&tool_call.name, &source);
                            if last_recoverable_error.as_deref() == Some(fingerprint.as_str()) {
                                recoverable_error_streak += 1;
                            } else {
                                last_recoverable_error = Some(fingerprint);
                                recoverable_error_streak = 1;
                            }
                            if recoverable_error_streak >= MAX_RECOVERABLE_ERROR_STREAK {
                                return Err(ToolCallError::RecoverableErrorStreak {
                                    tool_name: tool_call.name,
                                    error_kind: recoverable_error_kind(&source).to_string(),
                                    count: recoverable_error_streak,
                                });
                            }
                            tool_results.push((tool_call, recoverable_tool_error_payload(&source)));
                        } else {
                            return Err(ToolCallError::ToolExecution { source });
                        }
                    }
                    Err(_) => {
                        return Err(ToolCallError::ToolTimeout {
                            tool_name: tool_call.name,
                            timeout_secs: self.tool_timeout.as_secs(),
                        });
                    }
                }
            }

            let assistant_with_tool_calls = ChatMessage {
                role: ChatRole::Assistant,
                content: iteration_content,
                tool_calls: Some(pending_tool_calls),
                tool_call_id: None,
                reasoning_content: None,
            };
            current_request
                .messages
                .push(assistant_with_tool_calls.clone());
            result_messages.push(assistant_with_tool_calls);

            let tool_result_messages: Vec<ChatMessage> = tool_results
                .iter()
                .map(|(tc, result)| ChatMessage {
                    role: ChatRole::Tool,
                    content: result.clone(),
                    tool_call_id: Some(tc.id.clone()),
                    tool_calls: None,
                    reasoning_content: None,
                })
                .collect();
            current_request
                .messages
                .extend(tool_result_messages.clone());
            result_messages.extend(tool_result_messages);

            depth += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashSet, VecDeque};
    use std::sync::Arc;
    use std::time::Duration;

    use sunny_core::tool::{ToolError, ToolPolicy};
    use sunny_mind::{
        ChatMessage, ChatRole, LlmError, LlmProvider, LlmRequest, Provider, StreamEvent,
        StreamResult, TokenUsage,
    };
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    use super::StreamingToolLoop;
    use crate::tool_loop::ToolCallError;

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

        async fn requests(&self) -> Vec<LlmRequest> {
            self.requests.lock().await.clone()
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockStreamProvider {
        fn provider(&self) -> Provider {
            Provider::Anthropic
        }

        fn model_id(&self) -> &str {
            "mock-model"
        }

        async fn chat(
            &self,
            _req: LlmRequest,
        ) -> Result<sunny_mind::LlmResponse, sunny_mind::LlmError> {
            Err(LlmError::InvalidResponse {
                message: "chat() should not be called in streaming tests".to_string(),
            })
        }

        async fn chat_stream(&self, req: LlmRequest) -> Result<StreamResult, LlmError> {
            self.requests.lock().await.push(req);

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

    fn mk_request() -> LlmRequest {
        LlmRequest {
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: "initial prompt".to_string(),
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            }],
            max_tokens: Some(256),
            temperature: Some(0.2),
            tools: None,
            tool_choice: None,
            thinking_budget: None,
        }
    }

    fn mk_usage(input: u32, output: u32, total: u32) -> StreamEvent {
        StreamEvent::Usage {
            usage: TokenUsage {
                input_tokens: input,
                output_tokens: output,
                total_tokens: total,
            },
        }
    }

    fn mk_content(text: &str) -> StreamEvent {
        StreamEvent::ContentDelta {
            text: text.to_string(),
        }
    }

    fn mk_tool_start(id: &str, name: &str) -> StreamEvent {
        StreamEvent::ToolCallStart {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    fn mk_tool_complete(id: &str, name: &str, args: &str) -> StreamEvent {
        StreamEvent::ToolCallComplete {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args.to_string(),
        }
    }

    fn mk_done() -> StreamEvent {
        StreamEvent::Done
    }

    fn mk_cancel_token() -> CancellationToken {
        CancellationToken::new()
    }

    #[tokio::test]
    async fn test_streaming_tool_loop_no_tools() {
        let provider = Arc::new(MockStreamProvider::new(vec![vec![
            Ok(mk_content("hello ")),
            Ok(mk_content("world")),
            Ok(mk_usage(10, 2, 12)),
            Ok(mk_done()),
        ]]));
        let loop_runner = StreamingToolLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            3,
            mk_cancel_token(),
        );

        let call_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call_count_clone = call_count.clone();
        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(move |_, _, _, _| {
                    call_count_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok("unused".to_string())
                }),
                |_| {},
            )
            .await
            .expect("stream should return first response without tools");

        assert_eq!(result.content, "hello world");
        assert_eq!(result.metrics.iterations, 1);
        assert_eq!(result.metrics.total_tool_calls, 0);
        assert_eq!(result.metrics.total_content_events, 2);
        assert_eq!(result.usage.total_tokens, 12);
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn test_streaming_tool_loop_single_tool_call() {
        let provider = Arc::new(MockStreamProvider::new(vec![
            vec![
                Ok(mk_content("calling tool ")),
                Ok(mk_tool_start("call-1", "fs_read")),
                Ok(StreamEvent::ToolCallDelta {
                    id: "call-1".to_string(),
                    arguments_fragment: "{\"path\":\"a.rs\"}".to_string(),
                }),
                Ok(mk_tool_complete("call-1", "fs_read", "{\"path\":\"a.rs\"}")),
                Ok(mk_usage(10, 5, 15)),
                Ok(mk_done()),
            ],
            vec![
                Ok(mk_content("final")),
                Ok(mk_usage(4, 3, 7)),
                Ok(mk_done()),
            ],
        ]));

        let loop_runner = StreamingToolLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            4,
            mk_cancel_token(),
        );

        let executed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let executed_clone = executed.clone();
        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(move |id, name, args, _| {
                    executed_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    assert_eq!(id, "call-1");
                    assert_eq!(name, "fs_read");
                    assert_eq!(args, "{\"path\":\"a.rs\"}");
                    Ok("file content".to_string())
                }),
                |_| {},
            )
            .await
            .expect("streaming loop should execute single tool call");

        assert_eq!(result.content, "calling tool final");
        assert_eq!(result.metrics.iterations, 2);
        assert_eq!(result.metrics.total_tool_calls, 1);
        assert_eq!(result.metrics.total_content_events, 2);
        assert_eq!(result.metrics.tools_by_name.get("fs_read"), Some(&1));
        assert_eq!(result.usage.total_tokens, 22);
        assert_eq!(executed.load(std::sync::atomic::Ordering::SeqCst), 1);

        let requests = provider.requests().await;
        assert_eq!(requests.len(), 2);
        let assistant_msg = &requests[1].messages[requests[1].messages.len() - 2];
        assert_eq!(assistant_msg.role, ChatRole::Assistant);
        assert_eq!(assistant_msg.content, "calling tool ");
        assert_eq!(assistant_msg.tool_calls.as_ref().map(Vec::len), Some(1));

        let tool_result_msg = requests[1]
            .messages
            .last()
            .expect("second request should include tool result message");
        assert_eq!(tool_result_msg.role, ChatRole::Tool);
        assert_eq!(tool_result_msg.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(tool_result_msg.content, "file content");
    }

    #[tokio::test]
    async fn test_streaming_tool_loop_returns_full_message_chain() {
        let provider = Arc::new(MockStreamProvider::new(vec![
            vec![
                Ok(mk_content("planning tools ")),
                Ok(mk_tool_start("call-1", "fs_read")),
                Ok(mk_tool_complete("call-1", "fs_read", "{\"path\":\"a.rs\"}")),
                Ok(mk_tool_start("call-2", "fs_scan")),
                Ok(mk_tool_complete("call-2", "fs_scan", "{\"root\":\"src\"}")),
                Ok(mk_done()),
            ],
            vec![Ok(mk_content("done")), Ok(mk_done())],
        ]));
        let loop_runner =
            StreamingToolLoop::new(provider, ToolPolicy::default_ask(), 4, mk_cancel_token());

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|id, name, args, _| Ok(format!("result:{id}:{name}:{args}"))),
                |_| {},
            )
            .await
            .expect("streaming loop should return complete message chain");

        assert_eq!(result.messages.len(), 4);

        let first = &result.messages[0];
        assert_eq!(first.role, ChatRole::Assistant);
        assert_eq!(first.content, "planning tools ");
        assert_eq!(first.tool_calls.as_ref().map(Vec::len), Some(2));

        let second = &result.messages[1];
        assert_eq!(second.role, ChatRole::Tool);
        assert_eq!(second.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(second.content, "result:call-1:fs_read:{\"path\":\"a.rs\"}");

        let third = &result.messages[2];
        assert_eq!(third.role, ChatRole::Tool);
        assert_eq!(third.tool_call_id.as_deref(), Some("call-2"));
        assert_eq!(third.content, "result:call-2:fs_scan:{\"root\":\"src\"}");

        let fourth = &result.messages[3];
        assert_eq!(fourth.role, ChatRole::Assistant);
        assert_eq!(fourth.tool_calls, None);
        assert_eq!(fourth.tool_call_id, None);
        assert_eq!(fourth.content, "planning tools done");
    }

    #[tokio::test]
    async fn test_streaming_tool_loop_multi_iteration() {
        let provider = Arc::new(MockStreamProvider::new(vec![
            vec![
                Ok(mk_content("first ")),
                Ok(mk_tool_start("call-1", "fs_read")),
                Ok(mk_tool_complete("call-1", "fs_read", "{}")),
                Ok(mk_done()),
            ],
            vec![
                Ok(mk_content("second ")),
                Ok(mk_tool_start("call-2", "fs_scan")),
                Ok(mk_tool_complete("call-2", "fs_scan", "{}")),
                Ok(mk_done()),
            ],
            vec![Ok(mk_content("done")), Ok(mk_done())],
        ]));

        let loop_runner =
            StreamingToolLoop::new(provider, ToolPolicy::default_ask(), 5, mk_cancel_token());

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|id, name, _, depth| Ok(format!("{id}:{name}:{depth}"))),
                |_| {},
            )
            .await
            .expect("streaming loop should handle multiple tool iterations");

        assert_eq!(result.content, "first second done");
        assert_eq!(result.metrics.iterations, 3);
        assert_eq!(result.metrics.total_tool_calls, 2);
        assert_eq!(result.metrics.total_content_events, 3);
        assert_eq!(result.metrics.tools_by_name.get("fs_read"), Some(&1));
        assert_eq!(result.metrics.tools_by_name.get("fs_scan"), Some(&1));
    }

    #[tokio::test]
    async fn test_streaming_tool_loop_cancelled() {
        let provider = Arc::new(MockStreamProvider::new(vec![vec![
            Ok(mk_content("unreachable")),
            Ok(mk_done()),
        ]]));
        let cancel_token = CancellationToken::new();
        let loop_runner =
            StreamingToolLoop::new(provider, ToolPolicy::default_ask(), 3, cancel_token.clone());

        cancel_token.cancel();

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|_, _, _, _| Ok("ok".to_string())),
                |_| {},
            )
            .await;

        match result {
            Err(ToolCallError::Cancelled) => {}
            Ok(_) => panic!("expected cancellation error"),
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }

    #[tokio::test]
    async fn test_streaming_tool_loop_policy_violation() {
        let provider = Arc::new(MockStreamProvider::new(vec![vec![
            Ok(mk_content("calling forbidden")),
            Ok(mk_tool_start("call-1", "exec")),
            Ok(mk_tool_complete("call-1", "exec", "{}")),
            Ok(mk_done()),
        ]]));
        let loop_runner =
            StreamingToolLoop::new(provider, ToolPolicy::default_ask(), 3, mk_cancel_token());

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|_, _, _, _| Ok("should not run".to_string())),
                |_| {},
            )
            .await;

        match result {
            Err(ToolCallError::PolicyViolation { tool_name }) => assert_eq!(tool_name, "exec"),
            Ok(_) => panic!("expected policy violation"),
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }

    #[tokio::test]
    async fn test_streaming_tool_loop_max_iterations() {
        let provider = Arc::new(MockStreamProvider::new(vec![
            vec![
                Ok(mk_content("first tool")),
                Ok(mk_tool_start("call-1", "fs_read")),
                Ok(mk_tool_complete("call-1", "fs_read", "{}")),
                Ok(mk_done()),
            ],
            vec![Ok(mk_content("never reached")), Ok(mk_done())],
        ]));
        let loop_runner =
            StreamingToolLoop::new(provider, ToolPolicy::default_ask(), 1, mk_cancel_token());

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|_, _, _, _| Ok("ok".to_string())),
                |_| {},
            )
            .await;

        match result {
            Err(ToolCallError::MaxIterationsReached { count }) => assert_eq!(count, 1),
            Ok(_) => panic!("expected max iterations error"),
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_streaming_tool_loop_respects_provider_timeout() {
        struct SlowStreamProvider;

        #[async_trait::async_trait]
        impl LlmProvider for SlowStreamProvider {
            fn provider(&self) -> Provider {
                Provider::Anthropic
            }

            fn model_id(&self) -> &str {
                "slow-model"
            }

            async fn chat(
                &self,
                _req: LlmRequest,
            ) -> Result<sunny_mind::LlmResponse, sunny_mind::LlmError> {
                Err(LlmError::InvalidResponse {
                    message: "unused".to_string(),
                })
            }

            async fn chat_stream(&self, _req: LlmRequest) -> Result<StreamResult, LlmError> {
                tokio::time::sleep(Duration::from_secs(60)).await;
                let events = vec![Ok(StreamEvent::Done)];
                Ok(Box::pin(tokio_stream::iter(events)))
            }
        }

        let loop_runner = StreamingToolLoop::new(
            Arc::new(SlowStreamProvider),
            ToolPolicy::default_ask(),
            3,
            mk_cancel_token(),
        )
        .with_provider_timeout(Duration::ZERO);

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|_, _, _, _| Ok("ok".to_string())),
                |_| {},
            )
            .await;

        match result {
            Err(ToolCallError::ProviderTimeout { timeout_secs }) => assert_eq!(timeout_secs, 0),
            Ok(_) => panic!("expected provider timeout error"),
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_streaming_tool_loop_respects_tool_timeout() {
        let provider = Arc::new(MockStreamProvider::new(vec![vec![
            Ok(mk_content("tool timeout")),
            Ok(mk_tool_start("call-1", "fs_read")),
            Ok(mk_tool_complete("call-1", "fs_read", "{}")),
            Ok(mk_done()),
        ]]));

        let loop_runner =
            StreamingToolLoop::new(provider, ToolPolicy::default_ask(), 3, mk_cancel_token())
                .with_tool_timeout(Duration::ZERO);

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|_, _, _, _| {
                    std::thread::sleep(Duration::from_millis(10));
                    Ok("ok".to_string())
                }),
                |_| {},
            )
            .await;

        match result {
            Err(ToolCallError::ToolTimeout {
                tool_name,
                timeout_secs,
            }) => {
                assert_eq!(tool_name, "fs_read");
                assert_eq!(timeout_secs, 0);
            }
            Ok(_) => panic!("expected tool timeout error"),
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }

    #[tokio::test]
    async fn test_streaming_tool_loop_continues_on_recoverable_tool_error() {
        let provider = Arc::new(MockStreamProvider::new(vec![
            vec![
                Ok(mk_tool_start("call-1", "fs_read")),
                Ok(mk_tool_complete("call-1", "fs_read", "{}")),
                Ok(mk_done()),
            ],
            vec![Ok(mk_content("result")), Ok(mk_done())],
        ]));
        let loop_runner = StreamingToolLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            4,
            mk_cancel_token(),
        );

        let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let executions_clone = executions.clone();
        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(move |_, _, _, _| {
                    let call = executions_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    if call == 0 {
                        Err(ToolError::DirectoryReadUnsupported {
                            path: "dir/".to_string(),
                        })
                    } else {
                        Ok("result".to_string())
                    }
                }),
                |_| {},
            )
            .await
            .expect("recoverable tool error should not abort streaming loop");

        assert_eq!(result.content, "result");
        assert_eq!(result.metrics.iterations, 2);
        let requests = provider.requests().await;
        assert_eq!(requests.len(), 2);
    }

    #[tokio::test]
    async fn test_streaming_tool_loop_fails_on_repeated_recoverable_error_streak() {
        let provider = Arc::new(MockStreamProvider::new(vec![
            vec![
                Ok(mk_tool_start("call-1", "fs_read")),
                Ok(mk_tool_complete("call-1", "fs_read", "{}")),
                Ok(mk_done()),
            ],
            vec![
                Ok(mk_tool_start("call-2", "fs_read")),
                Ok(mk_tool_complete("call-2", "fs_read", "{}")),
                Ok(mk_done()),
            ],
            vec![
                Ok(mk_tool_start("call-3", "fs_read")),
                Ok(mk_tool_complete("call-3", "fs_read", "{}")),
                Ok(mk_done()),
            ],
            vec![Ok(mk_content("unreachable")), Ok(mk_done())],
        ]));
        let loop_runner =
            StreamingToolLoop::new(provider, ToolPolicy::default_ask(), 10, mk_cancel_token());

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|_, _, _, _| {
                    Err(ToolError::DirectoryReadUnsupported {
                        path: "dir/".to_string(),
                    })
                }),
                |_| {},
            )
            .await;

        match result {
            Err(ToolCallError::RecoverableErrorStreak {
                tool_name,
                error_kind,
                count,
            }) => {
                assert_eq!(tool_name, "fs_read");
                assert_eq!(error_kind, "directory_read_unsupported");
                assert_eq!(count, 3);
            }
            Ok(_) => panic!("expected recoverable error streak failure"),
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }

    #[tokio::test]
    async fn test_streaming_tool_loop_resets_recoverable_error_streak_after_success() {
        let provider = Arc::new(MockStreamProvider::new(vec![
            vec![
                Ok(mk_tool_start("call-1", "fs_read")),
                Ok(mk_tool_complete("call-1", "fs_read", "{}")),
                Ok(mk_done()),
            ],
            vec![
                Ok(mk_tool_start("call-2", "fs_read")),
                Ok(mk_tool_complete("call-2", "fs_read", "{}")),
                Ok(mk_done()),
            ],
            vec![
                Ok(mk_tool_start("call-3", "fs_read")),
                Ok(mk_tool_complete("call-3", "fs_read", "{}")),
                Ok(mk_done()),
            ],
            vec![
                Ok(mk_tool_start("call-4", "fs_read")),
                Ok(mk_tool_complete("call-4", "fs_read", "{}")),
                Ok(mk_done()),
            ],
            vec![Ok(mk_content("done")), Ok(mk_done())],
        ]));
        let loop_runner =
            StreamingToolLoop::new(provider, ToolPolicy::default_ask(), 10, mk_cancel_token());

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|id, _, _, _| {
                    if id == "call-3" {
                        Ok("result".to_string())
                    } else {
                        Err(ToolError::DirectoryReadUnsupported {
                            path: "dir/".to_string(),
                        })
                    }
                }),
                |_| {},
            )
            .await
            .expect("recoverable error streak should reset after success");

        assert_eq!(result.content, "done");
        assert_eq!(result.metrics.iterations, 5);
        assert_eq!(result.metrics.total_tool_calls, 4);
    }

    #[tokio::test]
    async fn test_streaming_tool_loop_dedup_cache_hit() {
        let provider = Arc::new(MockStreamProvider::new(vec![
            vec![
                Ok(mk_content("first ")),
                Ok(mk_tool_start("call-1", "fs_read")),
                Ok(mk_tool_complete("call-1", "fs_read", "{\"path\":\"a.rs\"}")),
                Ok(mk_done()),
            ],
            vec![
                Ok(mk_content("second ")),
                Ok(mk_tool_start("call-2", "fs_read")),
                Ok(mk_tool_complete("call-2", "fs_read", "{\"path\":\"a.rs\"}")),
                Ok(mk_done()),
            ],
            vec![Ok(mk_content("done")), Ok(mk_done())],
        ]));

        let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let executions_clone = executions.clone();
        let loop_runner =
            StreamingToolLoop::new(provider, ToolPolicy::default_ask(), 5, mk_cancel_token())
                .with_dedup_tools(HashSet::from(["fs_read".to_string()]));

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(move |_, _, _, _| {
                    executions_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok("file content".to_string())
                }),
                |_| {},
            )
            .await
            .expect("streaming loop should reuse cached tool result");

        assert_eq!(result.content, "first second done");
        assert_eq!(result.metrics.total_tool_calls, 2);
        assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn test_streaming_tool_loop_no_dedup_non_eligible() {
        let provider = Arc::new(MockStreamProvider::new(vec![
            vec![
                Ok(mk_content("first ")),
                Ok(mk_tool_start("call-1", "fs_scan")),
                Ok(mk_tool_complete("call-1", "fs_scan", "{}")),
                Ok(mk_done()),
            ],
            vec![
                Ok(mk_content("second ")),
                Ok(mk_tool_start("call-2", "fs_scan")),
                Ok(mk_tool_complete("call-2", "fs_scan", "{}")),
                Ok(mk_done()),
            ],
            vec![Ok(mk_content("done")), Ok(mk_done())],
        ]));

        let executions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let executions_clone = executions.clone();
        let loop_runner =
            StreamingToolLoop::new(provider, ToolPolicy::default_ask(), 5, mk_cancel_token())
                .with_dedup_tools(HashSet::from(["fs_read".to_string()]));

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(move |_, _, _, _| {
                    executions_clone.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok("scan result".to_string())
                }),
                |_| {},
            )
            .await
            .expect("streaming loop should execute non-eligible tool twice");

        assert_eq!(result.content, "first second done");
        assert_eq!(result.metrics.total_tool_calls, 2);
        assert_eq!(executions.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
