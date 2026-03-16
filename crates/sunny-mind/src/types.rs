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
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
    /// Tool invocations requested by the model; populated on `Assistant` messages that call
    /// tools. Each provider adapter converts these to its own wire format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Correlates a `Tool` role result with its originating call; must match [`ToolCall::id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Internal chain-of-thought produced by thinking models (e.g. kimi-k2.5).
    /// Must be echoed back verbatim in the assistant message for subsequent turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDefinition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<u32>,
}

/// Tool metadata exposed to the provider wire protocol.
///
/// `parameters` should contain a JSON Schema object that the target provider can
/// forward to model tool/function-calling APIs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// A tool invocation requested by a provider.
///
/// `arguments` contains the serialized JSON payload returned by the provider for
/// the requested tool, and `execution_depth` tracks nested tool-loop recursion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
    #[serde(default)]
    pub execution_depth: usize,
}

/// The rendered output associated with a prior tool call.
///
/// `tool_call_id` must match the originating [`ToolCall::id`] so providers can
/// correlate returned content with the requested invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallResult {
    pub tool_call_id: String,
    pub content: String,
}

/// Tool invocation policy requested from the provider.
///
/// `Auto` lets the model decide, `None` forbids tool use, `Required` requests a
/// mandatory tool call for providers that support it, and `Specific(name)` asks
/// the provider adapter to force the named tool when the wire format allows it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolChoice {
    Auto,
    None,
    Required,
    Specific(String),
}

/// Token accounting reported by the provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

impl TokenUsage {
    /// Return the derived total token count from input and output values.
    pub fn total(&self) -> u32 {
        self.input_tokens + self.output_tokens
    }
}

/// Provider response payload returned from a chat request.
///
/// `tool_calls` is populated when the provider chooses to invoke tools rather
/// than returning only assistant text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmResponse {
    pub content: String,
    pub usage: TokenUsage,
    pub finish_reason: String,
    pub provider_id: ProviderId,
    pub model_id: ModelId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    /// Internal chain-of-thought produced by thinking models (e.g. kimi-k2.5).
    /// Preserved from the provider response so it can be echoed back in follow-up turns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
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
