use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use sunny_core::tool::{ToolError, ToolPolicy};
use sunny_mind::{ChatMessage, ChatRole, LlmError, LlmProvider, LlmRequest, LlmResponse};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

const TOOL_TIMEOUT_SECS: u64 = 30;

pub struct ToolCallLoop<P: LlmProvider + ?Sized> {
    provider: Arc<P>,
    policy: ToolPolicy,
    max_iterations: usize,
    cancel: CancellationToken,
}

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

    #[error("tool execution timed out after {timeout_secs}s: {tool_name}")]
    ToolTimeout {
        tool_name: String,
        timeout_secs: u64,
    },

    #[error("tool call loop cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, Default)]
pub struct ToolCallMetrics {
    pub iterations: usize,
    pub total_tool_calls: usize,
    pub tools_by_name: HashMap<String, usize>,
}

pub struct ToolCallResult {
    pub response: LlmResponse,
    pub metrics: ToolCallMetrics,
}

impl<P: LlmProvider + ?Sized> ToolCallLoop<P> {
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
        }
    }

    pub async fn run(
        &self,
        request: LlmRequest,
        tool_executor: &(dyn Fn(&str, &str, &str) -> Result<String, ToolError> + Send + Sync),
    ) -> Result<ToolCallResult, ToolCallError> {
        let mut current_request = request;
        let mut metrics = ToolCallMetrics::default();

        loop {
            // Check for cancellation
            if self.cancel.is_cancelled() {
                return Err(ToolCallError::Cancelled);
            }

            // Check max iterations before processing
            if metrics.iterations >= self.max_iterations {
                return Err(ToolCallError::MaxIterationsReached {
                    count: metrics.iterations,
                });
            }

            metrics.iterations += 1;

            let response = self
                .provider
                .chat(current_request.clone())
                .await
                .map_err(|source| ToolCallError::Llm { source })?;

            let Some(tool_calls) = response
                .tool_calls
                .clone()
                .filter(|calls| !calls.is_empty())
            else {
                return Ok(ToolCallResult { response, metrics });
            };

            metrics.total_tool_calls += tool_calls.len();
            for tool_call in &tool_calls {
                *metrics
                    .tools_by_name
                    .entry(tool_call.name.clone())
                    .or_insert(0) += 1;
            }

            let mut tool_results = Vec::with_capacity(tool_calls.len());
            for tool_call in tool_calls {
                if !self.policy.is_allowed(&tool_call.name) {
                    return Err(ToolCallError::PolicyViolation {
                        tool_name: tool_call.name,
                    });
                }

                // Execute tool with timeout
                let tool_result = timeout(Duration::from_secs(TOOL_TIMEOUT_SECS), async {
                    tool_executor(&tool_call.id, &tool_call.name, &tool_call.arguments)
                })
                .await;

                match tool_result {
                    Ok(Ok(content)) => {
                        tool_results.push((tool_call, content));
                    }
                    Ok(Err(source)) => {
                        return Err(ToolCallError::ToolExecution { source });
                    }
                    Err(_) => {
                        return Err(ToolCallError::ToolTimeout {
                            tool_name: tool_call.name,
                            timeout_secs: TOOL_TIMEOUT_SECS,
                        });
                    }
                }
            }

            current_request.messages.push(ChatMessage {
                role: ChatRole::Assistant,
                content: response.content,
            });
            current_request.messages.push(ChatMessage {
                role: ChatRole::User,
                content: render_tool_results(&tool_results),
            });
        }
    }
}

fn render_tool_results(results: &[(sunny_mind::ToolCall, String)]) -> String {
    let mut rendered = String::from("Tool results:\n");
    for (tool_call, result) in results {
        rendered.push_str(&format!(
            "- id={} name={} arguments={} result={}\n",
            tool_call.id, tool_call.name, tool_call.arguments, result
        ));
    }
    rendered
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Arc;

    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    use sunny_core::tool::ToolPolicy;
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
            .run(mk_request(), &move |_, _, _| {
                tool_calls_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok("unused".to_string())
            })
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
            .run(mk_request(), &move |id, name, args| {
                executed_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                assert_eq!(id, "call-1");
                assert_eq!(name, "fs_read");
                assert_eq!(args, "{\"path\":\"a.rs\"}");
                Ok("file content".to_string())
            })
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
        assert!(last_message.content.contains("file content"));
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
            .run(mk_request(), &|_, _, _| Ok("should not run".to_string()))
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
            .run(mk_request(), &move |_, _, _| {
                executed_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok("ok".to_string())
            })
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
            .run(mk_request(), &|_, _, _| Ok("ok".to_string()))
            .await;

        match result {
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
            .run(mk_request(), &|_, _, _| Ok("ok".to_string()))
            .await
            .expect("should succeed");

        assert_eq!(result.metrics.iterations, 3);
        assert_eq!(result.metrics.total_tool_calls, 3);
        assert_eq!(result.metrics.tools_by_name.get("fs_read"), Some(&2usize));
        assert_eq!(result.metrics.tools_by_name.get("fs_scan"), Some(&1usize));
    }
}
