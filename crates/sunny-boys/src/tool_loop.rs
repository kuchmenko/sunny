use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sunny_core::tool::{ToolError, ToolPolicy};
use sunny_mind::{ChatMessage, ChatRole, LlmError, LlmProvider, LlmRequest, LlmResponse};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tracing::{info_span, Instrument};

use crate::timeouts::{tool_call_timeout, tool_provider_timeout};
/// Event name emitted when the tool-call loop exits because cancellation was requested.
pub const EVENT_TOOL_CANCELLED: &str = "tool.exec.cancelled";

/// Synchronous tool executor used by [`ToolCallLoop`] to run approved tool calls.
pub type ToolExecutor = dyn Fn(&str, &str, &str, usize) -> Result<String, ToolError> + Send + Sync;

/// Executes bounded provider/tool iterations while enforcing policy, timeout, and cancellation.
pub struct ToolCallLoop<P: LlmProvider + ?Sized> {
    provider: Arc<P>,
    policy: ToolPolicy,
    max_iterations: usize,
    cancel: CancellationToken,
    tool_timeout: Duration,
    provider_timeout: Duration,
}

/// Errors that can terminate a [`ToolCallLoop`] run.
#[derive(thiserror::Error, Debug)]
pub enum ToolCallError {
    #[error("tool not allowed by policy: {tool_name}")]
    PolicyViolation { tool_name: String },

    #[error("max tool-call iterations reached: {count}")]
    MaxIterationsReached { count: usize },

    #[error("llm request failed: {source}")]
    Llm { source: LlmError },

    #[error("tool execution failed: {source}")]
    ToolExecution { source: ToolError },

    #[error(
        "recoverable tool error repeated {count} times for {tool_name} ({error_kind}); aborting loop"
    )]
    RecoverableErrorStreak {
        tool_name: String,
        error_kind: String,
        count: usize,
    },

    #[error("tool execution timed out after {timeout_secs}s: {tool_name}")]
    ToolTimeout {
        tool_name: String,
        timeout_secs: u64,
    },

    #[error("provider response timed out after {timeout_secs}s")]
    ProviderTimeout { timeout_secs: u64 },

    #[error("tool call loop cancelled")]
    Cancelled,
}

/// Aggregate metrics collected across a single [`ToolCallLoop`] execution.
#[derive(Debug, Clone, Default)]
pub struct ToolCallMetrics {
    pub iterations: usize,
    pub total_tool_calls: usize,
    pub tools_by_name: HashMap<String, usize>,
}

/// Final provider response and metrics returned from a [`ToolCallLoop`] run.
pub struct ToolCallResult {
    pub response: LlmResponse,
    pub metrics: ToolCallMetrics,
}

const MAX_RECOVERABLE_ERROR_STREAK: usize = 3;

fn is_recoverable_tool_error(err: &ToolError) -> bool {
    matches!(err, ToolError::DirectoryReadUnsupported { .. })
}

fn recoverable_tool_error_payload(err: &ToolError) -> String {
    serde_json::json!({
        "ok": false,
        "recoverable": true,
        "error_kind": "directory_read_unsupported",
        "message": err.to_string(),
    })
    .to_string()
}

fn recoverable_error_kind(err: &ToolError) -> &'static str {
    match err {
        ToolError::DirectoryReadUnsupported { .. } => "directory_read_unsupported",
        _ => "unknown",
    }
}

fn recoverable_error_fingerprint(tool_name: &str, err: &ToolError) -> String {
    match err {
        ToolError::DirectoryReadUnsupported { path } => {
            format!("{tool_name}:directory_read_unsupported:{path}")
        }
        _ => format!("{tool_name}:unknown"),
    }
}

impl<P: LlmProvider + ?Sized> ToolCallLoop<P> {
    /// Create a tool-call loop with explicit provider, policy, iteration limit, and cancellation.
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
        }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn with_tool_timeout(mut self, timeout: Duration) -> Self {
        self.tool_timeout = timeout;
        self
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn with_provider_timeout(mut self, timeout: Duration) -> Self {
        self.provider_timeout = timeout;
        self
    }

    /// Run the provider/tool loop until the model returns a final response or an error occurs.
    pub async fn run(
        &self,
        request: LlmRequest,
        tool_executor: Arc<ToolExecutor>,
        initial_depth: usize,
    ) -> Result<ToolCallResult, ToolCallError> {
        let mut current_request = request;
        let mut metrics = ToolCallMetrics::default();
        let mut depth = initial_depth;
        let mut last_recoverable_error: Option<String> = None;
        let mut recoverable_error_streak = 0usize;

        loop {
            if self.cancel.is_cancelled() {
                return Err(ToolCallError::Cancelled);
            }

            if metrics.iterations >= self.max_iterations {
                return Err(ToolCallError::MaxIterationsReached {
                    count: metrics.iterations,
                });
            }

            let iteration = metrics.iterations;
            let iteration_result = async {
                metrics.iterations += 1;

                let response = tokio::select! {
                    _ = self.cancel.cancelled() => return Err(ToolCallError::Cancelled),
                    response = timeout(self.provider_timeout, self.provider.chat(current_request.clone())) => {
                        match response {
                            Ok(response) => response.map_err(|source| ToolCallError::Llm { source })?,
                            Err(_) => return Err(ToolCallError::ProviderTimeout {
                                timeout_secs: self.provider_timeout.as_secs(),
                            }),
                        }
                    }
                };

                let Some(tool_calls) = response
                    .tool_calls
                    .clone()
                    .filter(|calls| !calls.is_empty())
                else {
                    return Ok::<_, ToolCallError>((response, None));
                };

                metrics.total_tool_calls += tool_calls.len();
                for tool_call in &tool_calls {
                    *metrics
                        .tools_by_name
                        .entry(tool_call.name.clone())
                        .or_insert(0) += 1;
                }

                let mut tool_results = Vec::with_capacity(tool_calls.len());
                for mut tool_call in tool_calls {
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
                    let tool_result = timeout(self.tool_timeout, async move {
                        tokio::task::spawn_blocking(move || {
                            executor(&call_id, &call_name, &call_arguments, depth)
                        })
                        .await
                        .map_err(|join_err| {
                            ToolError::ExecutionFailed {
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
                        Ok(Ok(content)) => {
                            last_recoverable_error = None;
                            recoverable_error_streak = 0;
                            tool_results.push((tool_call, content));
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

                Ok((response, Some(tool_results)))
            }
            .instrument(info_span!(
                "tool_call_iteration",
                iteration,
                depth,
                event = sunny_core::orchestrator::events::EVENT_TOOL_EXEC_DEPTH,
            ))
            .await?;

            let (response, tool_results) = iteration_result;
            let Some(tool_results) = tool_results else {
                return Ok(ToolCallResult { response, metrics });
            };

            current_request.messages.push(ChatMessage {
                role: ChatRole::Assistant,
                content: response.content.clone(),
                tool_calls: response.tool_calls.clone(),
                tool_call_id: None,
                reasoning_content: response.reasoning_content.clone(),
            });
            current_request
                .messages
                .extend(tool_results.iter().map(|(tc, result)| ChatMessage {
                    role: ChatRole::Tool,
                    content: result.clone(),
                    tool_call_id: Some(tc.id.clone()),
                    tool_calls: None,
                    reasoning_content: None,
                }));

            depth += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;
    use std::time::Duration;

    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    use sunny_core::tool::{ToolError, ToolPolicy};
    use sunny_mind::{
        ChatMessage, ChatRole, LlmError, LlmProvider, LlmRequest, LlmResponse, ModelId, ProviderId,
        TokenUsage, ToolCall,
    };

    use super::{ToolCallError, ToolCallLoop};

    struct MockProvider {
        responses: Mutex<VecDeque<LlmResponse>>,
        requests: Mutex<Vec<LlmRequest>>,
    }

    impl MockProvider {
        fn new(responses: Vec<LlmResponse>) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from(responses)),
                requests: Mutex::new(Vec::new()),
            }
        }

        async fn requests(&self) -> Vec<LlmRequest> {
            self.requests.lock().await.clone()
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        fn provider_id(&self) -> &str {
            "mock"
        }

        fn model_id(&self) -> &str {
            "mock-model"
        }

        async fn chat(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
            self.requests.lock().await.push(req);

            self.responses
                .lock()
                .await
                .pop_front()
                .ok_or_else(|| LlmError::InvalidResponse {
                    message: "no mock response configured".to_string(),
                })
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
        }
    }

    fn mk_response(content: &str, tool_calls: Option<Vec<ToolCall>>) -> LlmResponse {
        LlmResponse {
            content: content.to_string(),
            usage: TokenUsage {
                input_tokens: 10,
                output_tokens: 5,
                total_tokens: 15,
            },
            finish_reason: "stop".to_string(),
            provider_id: ProviderId("mock".to_string()),
            model_id: ModelId("mock-model".to_string()),
            tool_calls,
            reasoning_content: None,
        }
    }

    fn mk_tool_call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: arguments.to_string(),
            execution_depth: 0,
        }
    }

    fn mk_cancel_token() -> CancellationToken {
        CancellationToken::new()
    }

    #[tokio::test]
    async fn test_tool_call_loop_no_tools() {
        let provider = Arc::new(MockProvider::new(vec![mk_response("done", None)]));
        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            3,
            mk_cancel_token(),
        );

        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool_calls_seen = call_count.clone();
        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(move |_, _, _, _depth| {
                    tool_calls_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok("unused".to_string())
                }),
                0,
            )
            .await
            .expect("loop should return first LLM response");

        assert_eq!(result.response.content, "done");
        assert_eq!(result.metrics.iterations, 1);
        assert_eq!(result.metrics.total_tool_calls, 0);
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 0);

        let requests = provider.requests().await;
        assert_eq!(requests.len(), 1);
    }

    #[tokio::test]
    async fn test_tool_call_loop_with_allowed_tool() {
        let provider = Arc::new(MockProvider::new(vec![
            mk_response(
                "calling tool",
                Some(vec![mk_tool_call(
                    "call-1",
                    "fs_read",
                    "{\"path\":\"a.rs\"}",
                )]),
            ),
            mk_response("final", None),
        ]));

        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            3,
            mk_cancel_token(),
        );

        let executed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let executed_seen = executed.clone();
        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(move |id, name, args, _depth| {
                    executed_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    assert_eq!(id, "call-1");
                    assert_eq!(name, "fs_read");
                    assert_eq!(args, "{\"path\":\"a.rs\"}");
                    Ok("file content".to_string())
                }),
                0,
            )
            .await
            .expect("loop should execute allowed tool and finish");

        assert_eq!(result.response.content, "final");
        assert_eq!(result.metrics.iterations, 2);
        assert_eq!(result.metrics.total_tool_calls, 1);
        assert_eq!(
            result.metrics.tools_by_name.get("fs_read"),
            Some(&1usize),
            "should track fs_read usage"
        );
        assert_eq!(executed.load(std::sync::atomic::Ordering::SeqCst), 1);

        let requests = provider.requests().await;
        assert_eq!(requests.len(), 2);
        let last_message = requests[1]
            .messages
            .last()
            .expect("second request should contain appended tool result");
        assert_eq!(last_message.role, ChatRole::Tool);
        assert_eq!(last_message.tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(last_message.content, "file content");
        // The second-to-last message is the assistant message that carried the tool call.
        let assistant_msg = &requests[1].messages[requests[1].messages.len() - 2];
        assert_eq!(assistant_msg.role, ChatRole::Assistant);
        let tcs = assistant_msg
            .tool_calls
            .as_ref()
            .expect("assistant message must carry tool_calls");
        assert_eq!(tcs[0].id, "call-1");
        assert_eq!(tcs[0].name, "fs_read");
    }

    #[tokio::test]
    async fn test_tool_call_loop_policy_violation() {
        let provider = Arc::new(MockProvider::new(vec![mk_response(
            "calling forbidden tool",
            Some(vec![mk_tool_call("call-1", "exec", "{}")]),
        )]));
        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            3,
            mk_cancel_token(),
        );

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|_, _, _, _| Ok("should not run".to_string())),
                0,
            )
            .await;

        match result {
            Err(ToolCallError::PolicyViolation { tool_name }) => {
                assert_eq!(tool_name, "exec");
            }
            Ok(_) => panic!("expected policy violation"),
            Err(other) => panic!("unexpected error variant: {other}"),
        }

        let requests = provider.requests().await;
        assert_eq!(requests.len(), 1);
    }

    #[tokio::test]
    async fn test_tool_call_loop_max_iterations() {
        let provider = Arc::new(MockProvider::new(vec![
            mk_response(
                "first tool",
                Some(vec![mk_tool_call(
                    "call-1",
                    "fs_read",
                    "{\"path\":\"a.rs\"}",
                )]),
            ),
            mk_response(
                "second tool",
                Some(vec![mk_tool_call(
                    "call-2",
                    "fs_read",
                    "{\"path\":\"b.rs\"}",
                )]),
            ),
        ]));
        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            1,
            mk_cancel_token(),
        );

        let executed = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let executed_seen = executed.clone();
        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(move |_, _, _, _| {
                    executed_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok("ok".to_string())
                }),
                0,
            )
            .await;

        match result {
            Err(ToolCallError::MaxIterationsReached { count }) => {
                assert_eq!(count, 1);
            }
            Ok(_) => panic!("expected max iteration error"),
            Err(other) => panic!("unexpected error variant: {other}"),
        }

        assert_eq!(executed.load(std::sync::atomic::Ordering::SeqCst), 1);
        let requests = provider.requests().await;
        assert_eq!(
            requests.len(),
            1,
            "should only make 1 request before hitting max_iterations"
        );
    }

    #[tokio::test]
    async fn test_tool_call_loop_respects_cancellation() {
        let provider = Arc::new(MockProvider::new(vec![mk_response(
            "calling tool",
            Some(vec![mk_tool_call("call-1", "fs_read", "{}")]),
        )]));
        let cancel_token = CancellationToken::new();
        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            3,
            cancel_token.clone(),
        );

        // Cancel before running
        cancel_token.cancel();

        let result = loop_runner
            .run(mk_request(), Arc::new(|_, _, _, _| Ok("ok".to_string())), 0)
            .await;

        match result {
            Err(ToolCallError::Cancelled) => {}
            Ok(_) => panic!("expected cancellation error"),
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }

    struct SlowProvider;

    #[async_trait::async_trait]
    impl LlmProvider for SlowProvider {
        fn provider_id(&self) -> &str {
            "slow"
        }

        fn model_id(&self) -> &str {
            "slow-model"
        }

        async fn chat(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok(mk_response("done", None))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_tool_call_loop_cancels_during_provider_request() {
        let cancel_token = CancellationToken::new();
        let loop_runner = ToolCallLoop::new(
            Arc::new(SlowProvider),
            ToolPolicy::default_ask(),
            3,
            cancel_token.clone(),
        );

        let run = tokio::spawn(async move {
            loop_runner
                .run(mk_request(), Arc::new(|_, _, _, _| Ok("ok".to_string())), 0)
                .await
        });

        tokio::task::yield_now().await;
        cancel_token.cancel();

        match run.await.expect("join run task") {
            Err(ToolCallError::Cancelled) => {}
            Ok(_) => panic!("expected cancellation error"),
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }

    #[tokio::test]
    async fn test_tool_call_loop_tracks_metrics() {
        let provider = Arc::new(MockProvider::new(vec![
            mk_response(
                "first",
                Some(vec![
                    mk_tool_call("call-1", "fs_read", "{}"),
                    mk_tool_call("call-2", "fs_scan", "{}"),
                ]),
            ),
            mk_response(
                "second",
                Some(vec![mk_tool_call("call-3", "fs_read", "{}")]),
            ),
            mk_response("final", None),
        ]));

        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            5,
            mk_cancel_token(),
        );

        let result = loop_runner
            .run(mk_request(), Arc::new(|_, _, _, _| Ok("ok".to_string())), 0)
            .await
            .expect("should succeed");

        assert_eq!(result.metrics.iterations, 3);
        assert_eq!(result.metrics.total_tool_calls, 3);
        assert_eq!(result.metrics.tools_by_name.get("fs_read"), Some(&2usize));
        assert_eq!(result.metrics.tools_by_name.get("fs_scan"), Some(&1usize));
    }

    #[tokio::test]
    async fn test_tool_call_loop_tracks_depth() {
        let provider = Arc::new(MockProvider::new(vec![
            mk_response("first", Some(vec![mk_tool_call("call-1", "fs_read", "{}")])),
            mk_response(
                "second",
                Some(vec![mk_tool_call("call-2", "fs_read", "{}")]),
            ),
            mk_response("done", None),
        ]));

        let observed_depths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let depths_clone = observed_depths.clone();

        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            5,
            mk_cancel_token(),
        );

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(move |_, _, _, depth| {
                    depths_clone
                        .lock()
                        .expect("lock observed_depths")
                        .push(depth);
                    Ok("ok".to_string())
                }),
                0,
            )
            .await
            .expect("should succeed");

        assert_eq!(result.response.content, "done");
        let depths = observed_depths.lock().expect("lock observed_depths");
        assert_eq!(*depths, vec![0, 1]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_tool_call_loop_respects_timeout() {
        let provider = Arc::new(MockProvider::new(vec![mk_response(
            "calling slow tool",
            Some(vec![mk_tool_call("call-1", "fs_read", "{}")]),
        )]));

        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            3,
            mk_cancel_token(),
        )
        .with_tool_timeout(std::time::Duration::ZERO);

        let result = loop_runner
            .run(mk_request(), Arc::new(|_, _, _, _| Ok("ok".to_string())), 0)
            .await;

        match result {
            Err(ToolCallError::ToolTimeout {
                tool_name,
                timeout_secs,
            }) => {
                assert_eq!(tool_name, "fs_read");
                assert_eq!(timeout_secs, 0);
            }
            Ok(_) => panic!("expected timeout error"),
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_tool_call_loop_respects_provider_timeout() {
        let loop_runner = ToolCallLoop::new(
            Arc::new(SlowProvider),
            ToolPolicy::default_ask(),
            3,
            mk_cancel_token(),
        )
        .with_provider_timeout(Duration::ZERO);

        let result = loop_runner
            .run(mk_request(), Arc::new(|_, _, _, _| Ok("ok".to_string())), 0)
            .await;

        match result {
            Err(ToolCallError::ProviderTimeout { timeout_secs }) => {
                assert_eq!(timeout_secs, 0);
            }
            Ok(_) => panic!("expected provider timeout error"),
            Err(other) => panic!("unexpected error variant: {other}"),
        }
    }

    #[tokio::test]
    async fn test_tool_call_loop_depth_in_events() {
        let provider = Arc::new(MockProvider::new(vec![
            mk_response("first", Some(vec![mk_tool_call("call-1", "fs_read", "{}")])),
            mk_response(
                "second",
                Some(vec![mk_tool_call("call-2", "fs_scan", "{}")]),
            ),
            mk_response("third", Some(vec![mk_tool_call("call-3", "fs_read", "{}")])),
            mk_response("final", None),
        ]));

        let max_depth = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_depth_clone = max_depth.clone();

        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            10,
            mk_cancel_token(),
        );

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(move |_, _, _, depth| {
                    max_depth_clone.fetch_max(depth, std::sync::atomic::Ordering::SeqCst);
                    Ok("result".to_string())
                }),
                0,
            )
            .await
            .expect("should succeed");

        assert_eq!(result.response.content, "final");
        assert_eq!(
            max_depth.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "max depth should be 2 (3 tool iterations: 0, 1, 2)"
        );
    }

    #[tokio::test]
    async fn test_tool_call_loop_continues_on_recoverable_tool_error() {
        let provider = Arc::new(MockProvider::new(vec![
            mk_response("first", Some(vec![mk_tool_call("call-1", "fs_read", "{}")])),
            mk_response("done", None),
        ]));

        let loop_runner = ToolCallLoop::new(
            provider.clone(),
            ToolPolicy::default_ask(),
            3,
            mk_cancel_token(),
        );

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|_, _, _, _| {
                    Err(ToolError::DirectoryReadUnsupported {
                        path: "/tmp".to_string(),
                    })
                }),
                0,
            )
            .await
            .expect("recoverable tool error should not abort loop");

        assert_eq!(result.response.content, "done");
        assert_eq!(result.metrics.iterations, 2);
        assert_eq!(result.metrics.total_tool_calls, 1);
    }

    #[tokio::test]
    async fn test_tool_call_loop_fails_on_repeated_recoverable_error_streak() {
        let provider = Arc::new(MockProvider::new(vec![
            mk_response("first", Some(vec![mk_tool_call("call-1", "fs_read", "{}")])),
            mk_response(
                "second",
                Some(vec![mk_tool_call("call-2", "fs_read", "{}")]),
            ),
            mk_response("third", Some(vec![mk_tool_call("call-3", "fs_read", "{}")])),
            mk_response("unreachable", None),
        ]));

        let loop_runner =
            ToolCallLoop::new(provider, ToolPolicy::default_ask(), 10, mk_cancel_token());

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|_, _, _, _| {
                    Err(ToolError::DirectoryReadUnsupported {
                        path: "/tmp".to_string(),
                    })
                }),
                0,
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
    async fn test_tool_call_loop_resets_recoverable_error_streak_after_success() {
        let provider = Arc::new(MockProvider::new(vec![
            mk_response("first", Some(vec![mk_tool_call("call-1", "fs_read", "{}")])),
            mk_response(
                "second",
                Some(vec![mk_tool_call("call-2", "fs_read", "{}")]),
            ),
            mk_response("third", Some(vec![mk_tool_call("call-3", "fs_read", "{}")])),
            mk_response("done", None),
        ]));

        let loop_runner =
            ToolCallLoop::new(provider, ToolPolicy::default_ask(), 10, mk_cancel_token());

        let result = loop_runner
            .run(
                mk_request(),
                Arc::new(|id, _, _, _| {
                    if id == "call-2" {
                        Ok("content".to_string())
                    } else {
                        Err(ToolError::DirectoryReadUnsupported {
                            path: "/tmp".to_string(),
                        })
                    }
                }),
                0,
            )
            .await
            .expect("streak should reset after successful tool call");

        assert_eq!(result.response.content, "done");
        assert_eq!(result.metrics.iterations, 4);
        assert_eq!(result.metrics.total_tool_calls, 3);
    }
}
