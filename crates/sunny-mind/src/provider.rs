use crate::error::LlmError;
use crate::types::{LlmRequest, LlmResponse};

/// LLM provider contract. Implementations must be `Send + Sync` for `Arc<dyn LlmProvider>`
/// and should support tool definitions/tool call responses when available.
#[async_trait::async_trait]
pub trait LlmProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    fn model_id(&self) -> &str;
    async fn chat(&self, req: LlmRequest) -> Result<LlmResponse, LlmError>;
}
