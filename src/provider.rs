use async_trait::async_trait;
use tkach::{LlmProvider, ProviderError, ProviderEventStream, Request, Response};

#[derive(Default)]
pub struct NoopProvider {}

#[async_trait]
impl LlmProvider for NoopProvider {
    async fn complete(&self, _request: Request) -> Result<Response, ProviderError> {
        Err(ProviderError::Other("Noop".to_string()))
    }

    async fn stream(&self, _request: Request) -> Result<ProviderEventStream, ProviderError> {
        Err(ProviderError::Other("Noop".to_string()))
    }
}
