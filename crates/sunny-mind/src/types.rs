use serde::{Deserialize, Serialize};

/// Unique identifier for an LLM provider (e.g. "openai", "kimi").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProviderId(pub String);

/// Unique identifier for a model within a provider (e.g. "gpt-4o", "moonshot-v1-8k").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ModelId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmRequest {
    pub messages: Vec<ChatMessage>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

impl TokenUsage {
    pub fn total(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: String,
    pub usage: TokenUsage,
    pub finish_reason: String,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
}

/// Policy for routing requests across LLM providers.
///
/// TODO: Add `Fallback(Vec<ProviderId>)` variant for failover routing
/// TODO: Add `CostOptimized { budget_limit: f64 }` variant for cost-aware routing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ProviderRoutingPolicy {
    PrimaryOnly,
}

/// Cost and rate-limit economics for an LLM provider.
///
/// TODO: Implement cost estimation from TokenUsage
/// TODO: Integrate with ProviderRoutingPolicy::CostOptimized
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderEconomics {
    pub input_cost_per_1m: Option<f64>,
    pub output_cost_per_1m: Option<f64>,
    pub rpm_limit: Option<u32>,
}
