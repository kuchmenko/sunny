use std::collections::HashMap;
use std::sync::Arc;

use tracing::warn;

use sunny_mind::LlmProvider;
use sunny_tasks::ModelsConfig;

/// Registry mapping model strings to provider instances.
///
/// Resolves a task category (quick/standard/deep) or explicit model name to
/// the appropriate `LlmProvider` via the configured `ModelsConfig`.
/// Falls back to the default model or any available provider when the target
/// model has no credentials.
pub struct ProviderRegistry {
    /// model_id string → provider instance
    providers: HashMap<String, Arc<dyn LlmProvider>>,
    models_config: ModelsConfig,
}

impl ProviderRegistry {
    pub fn new(models_config: ModelsConfig) -> Self {
        Self {
            providers: HashMap::new(),
            models_config,
        }
    }

    /// Register a provider under its model ID.
    pub fn register(&mut self, provider: Arc<dyn LlmProvider>) {
        self.providers
            .insert(provider.model_id().to_string(), provider);
    }

    /// Register a provider under an explicit model string key.
    pub fn register_with_key(&mut self, model: &str, provider: Arc<dyn LlmProvider>) {
        self.providers.insert(model.to_string(), provider);
    }

    /// Resolve a task category to a provider.
    ///
    /// - `Some(category)` → look up model string via `ModelsConfig::resolve_category`,
    ///   then find the matching provider.
    /// - `None` → use the default model.
    ///
    /// Falls back to the default model's provider, then to any registered provider,
    /// and finally returns `None` if the registry is completely empty.
    pub fn resolve_for_category(&self, category: Option<&str>) -> Option<Arc<dyn LlmProvider>> {
        let model = match category {
            Some(cat) => self.models_config.resolve_category(cat),
            None => &self.models_config.default,
        };

        // Exact model match.
        if let Some(provider) = self.providers.get(model) {
            return Some(Arc::clone(provider));
        }

        // Fall back to default model.
        let default_model = &self.models_config.default;
        if model != default_model {
            if let Some(provider) = self.providers.get(default_model) {
                warn!(
                    requested_model = model,
                    fallback_model = %default_model,
                    "requested model not available, falling back to default"
                );
                return Some(Arc::clone(provider));
            }
        }

        // Fall back to any registered provider.
        if let Some(provider) = self.providers.values().next() {
            warn!(
                requested_model = model,
                "no matching provider for model, using first available"
            );
            return Some(Arc::clone(provider));
        }

        None
    }

    pub fn resolve_for_role_effort(
        &self,
        _role_str: &str,
        effort_str: &str,
    ) -> (Option<Arc<dyn LlmProvider>>, Option<u32>) {
        let thinking_budget = match effort_str {
            "moderate" => Some(4000),
            "high" => Some(16000),
            "critical" => Some(32000),
            _ => None,
        };

        (self.resolve_for_category(None), thinking_budget)
    }

    /// Number of registered providers.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.providers.len()
    }

    /// True if no providers are registered.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use async_trait::async_trait;
    use sunny_mind::{
        LlmError, LlmRequest, LlmResponse, ModelId, Provider, StreamEvent, StreamResult, TokenUsage,
    };
    use tokio::sync::Mutex;

    use super::*;

    struct MockProvider {
        model: String,
        streams: Mutex<VecDeque<Vec<Result<StreamEvent, LlmError>>>>,
    }

    impl MockProvider {
        fn new(model: &str) -> Self {
            Self {
                model: model.to_string(),
                streams: Mutex::new(VecDeque::new()),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for MockProvider {
        fn provider(&self) -> Provider {
            Provider::Anthropic
        }
        fn model_id(&self) -> &str {
            &self.model
        }
        async fn chat(&self, _request: LlmRequest) -> Result<LlmResponse, LlmError> {
            Ok(LlmResponse {
                content: "mock".to_string(),
                usage: TokenUsage {
                    input_tokens: 1,
                    output_tokens: 1,
                    total_tokens: 2,
                },
                finish_reason: "stop".to_string(),
                provider: Provider::Anthropic,
                model_id: ModelId(self.model.clone()),
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

    fn make_default_config() -> ModelsConfig {
        ModelsConfig {
            quick: "claude-haiku-4-5".into(),
            standard: "claude-sonnet-4-6".into(),
            deep: "gpt-5.4".into(),
            default: "claude-sonnet-4-6".into(),
        }
    }

    #[test]
    fn test_models_config_defaults() {
        let config = ModelsConfig::default();
        assert_eq!(config.quick, "claude-haiku-4-5");
        assert_eq!(config.standard, "claude-sonnet-4-6");
        assert_eq!(config.deep, "gpt-5.4");
        assert_eq!(config.default, "claude-sonnet-4-6");
    }

    #[test]
    fn test_models_config_resolve_category() {
        let config = make_default_config();
        assert_eq!(config.resolve_category("quick"), "claude-haiku-4-5");
        assert_eq!(config.resolve_category("standard"), "claude-sonnet-4-6");
        assert_eq!(config.resolve_category("deep"), "gpt-5.4");
        assert_eq!(config.resolve_category("unknown"), "claude-sonnet-4-6");
    }

    #[test]
    fn test_registry_resolve_for_category() {
        let mut registry = ProviderRegistry::new(make_default_config());
        let sonnet = Arc::new(MockProvider::new("claude-sonnet-4-6"));
        let haiku = Arc::new(MockProvider::new("claude-haiku-4-5"));
        let gpt = Arc::new(MockProvider::new("gpt-5.4"));

        registry.register(Arc::clone(&sonnet) as Arc<dyn LlmProvider>);
        registry.register(Arc::clone(&haiku) as Arc<dyn LlmProvider>);
        registry.register(Arc::clone(&gpt) as Arc<dyn LlmProvider>);

        let resolved = registry.resolve_for_category(Some("quick")).unwrap();
        assert_eq!(resolved.model_id(), "claude-haiku-4-5");

        let resolved = registry.resolve_for_category(Some("standard")).unwrap();
        assert_eq!(resolved.model_id(), "claude-sonnet-4-6");

        let resolved = registry.resolve_for_category(Some("deep")).unwrap();
        assert_eq!(resolved.model_id(), "gpt-5.4");
    }

    #[test]
    fn test_registry_missing_provider_falls_back() {
        let mut registry = ProviderRegistry::new(make_default_config());
        // Only register sonnet (the default). Don't register gpt-5.4.
        let sonnet = Arc::new(MockProvider::new("claude-sonnet-4-6"));
        registry.register(Arc::clone(&sonnet) as Arc<dyn LlmProvider>);

        // deep → gpt-5.4 not available → fallback to default (sonnet).
        let resolved = registry.resolve_for_category(Some("deep")).unwrap();
        assert_eq!(resolved.model_id(), "claude-sonnet-4-6");
    }

    #[test]
    fn test_registry_none_category_uses_default() {
        let mut registry = ProviderRegistry::new(make_default_config());
        let sonnet = Arc::new(MockProvider::new("claude-sonnet-4-6"));
        registry.register(Arc::clone(&sonnet) as Arc<dyn LlmProvider>);

        let resolved = registry.resolve_for_category(None).unwrap();
        assert_eq!(resolved.model_id(), "claude-sonnet-4-6");
    }

    #[test]
    fn test_registry_empty_returns_none() {
        let registry = ProviderRegistry::new(make_default_config());
        assert!(registry.resolve_for_category(Some("quick")).is_none());
    }

    #[test]
    fn test_resolve_for_role_effort_returns_thinking_budget() {
        let mut registry = ProviderRegistry::new(make_default_config());
        let sonnet = Arc::new(MockProvider::new("claude-sonnet-4-6"));
        registry.register(Arc::clone(&sonnet) as Arc<dyn LlmProvider>);

        let (provider, budget) = registry.resolve_for_role_effort("executor", "high");
        assert!(provider.is_some());
        assert_eq!(provider.unwrap().model_id(), "claude-sonnet-4-6");
        assert_eq!(budget, Some(16000));

        let (_, budget_low) = registry.resolve_for_role_effort("planner", "low");
        assert_eq!(budget_low, None);
    }

    #[test]
    fn test_resolve_for_role_effort_unknown_effort_uses_default_provider() {
        let mut registry = ProviderRegistry::new(make_default_config());
        let sonnet = Arc::new(MockProvider::new("claude-sonnet-4-6"));
        registry.register(Arc::clone(&sonnet) as Arc<dyn LlmProvider>);

        let (provider, budget) = registry.resolve_for_role_effort("verifier", "unknown");
        assert_eq!(provider.unwrap().model_id(), "claude-sonnet-4-6");
        assert_eq!(budget, None);
    }
}
