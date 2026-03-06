use std::sync::Arc;

use sunny_core::tool::{ToolError, ToolPolicy};
use sunny_mind::{ChatMessage, ChatRole, LlmError, LlmProvider, LlmRequest, LlmResponse};

pub struct ToolCallLoop<P: LlmProvider> {
    provider: Arc<P>,
    policy: ToolPolicy,
    max_iterations: usize,
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
}

impl<P: LlmProvider> ToolCallLoop<P> {
    pub fn new(provider: Arc<P>, policy: ToolPolicy, max_iterations: usize) -> Self {
        Self {
            provider,
            policy,
            max_iterations,
        }
    }

    pub async fn run(
        &self,
        request: LlmRequest,
        tool_executor: &dyn Fn(&str, &str, &str) -> Result<String, ToolError>,
    ) -> Result<LlmResponse, ToolCallError> {
        let mut current_request = request;
        let mut iteration_count = 0;

        loop {
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
                return Ok(response);
            };

            if iteration_count >= self.max_iterations {
                return Err(ToolCallError::MaxIterationsReached {
                    count: iteration_count,
                });
            }
            iteration_count += 1;

            let mut tool_results = Vec::with_capacity(tool_calls.len());
            for tool_call in tool_calls {
                if !self.policy.is_allowed(&tool_call.name) {
                    return Err(ToolCallError::PolicyViolation {
                        tool_name: tool_call.name,
                    });
                }

                let content = tool_executor(&tool_call.id, &tool_call.name, &tool_call.arguments)
                    .map_err(|source| ToolCallError::ToolExecution { source })?;
                tool_results.push((tool_call, content));
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

    #[tokio::test]
    async fn test_tool_call_loop_no_tools() {
        let provider = Arc::new(MockProvider::new(vec![mk_response("done", None)]));
        let loop_runner = ToolCallLoop::new(provider.clone(), ToolPolicy::default_ask(), 3);

        let call_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tool_calls_seen = call_count.clone();
        let result = loop_runner
            .run(mk_request(), &move |_, _, _| {
                tool_calls_seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok("unused".to_string())
            })
            .await
            .expect("loop should return first LLM response");

        assert_eq!(result.content, "done");
        assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 0);

        let requests = provider.requests().await;
        assert_eq!(requests.len(), 1);
    }

    #[tokio::test]
    async fn test_tool_call_loop_with_allowed_tool() {
        let provider = Arc::new(MockProvider::new(vec![
            mk_response(
                "calling tool",
                Some(vec![mk_tool_call("call-1", "fs_read", "{\"path\":\"a.rs\"}")]),
            ),
            mk_response("final", None),
        ]));

        let loop_runner = ToolCallLoop::new(provider.clone(), ToolPolicy::default_ask(), 3);

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

        assert_eq!(result.content, "final");
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
        let loop_runner = ToolCallLoop::new(provider.clone(), ToolPolicy::default_ask(), 3);

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
                Some(vec![mk_tool_call("call-1", "fs_read", "{\"path\":\"a.rs\"}")]),
            ),
            mk_response(
                "second tool",
                Some(vec![mk_tool_call("call-2", "fs_read", "{\"path\":\"b.rs\"}")]),
            ),
        ]));
        let loop_runner = ToolCallLoop::new(provider.clone(), ToolPolicy::default_ask(), 1);

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
        assert_eq!(requests.len(), 2);
    }
}
