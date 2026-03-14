//! Token budget tracking — hybrid API usage + estimation.

use std::sync::Arc;

use sunny_mind::{ChatMessage, TokenCounter, TokenUsage};

/// Tracks token consumption for a chat session, using both API-reported
/// usage and provider-specific estimation for unsaved messages.
pub struct TokenBudget {
    max_tokens: u32,
    consumed_tokens: u32,
    counter: Arc<dyn TokenCounter>,
}

impl TokenBudget {
    /// Create a new budget with the given limit and token counter.
    pub fn new(max_tokens: u32, counter: Arc<dyn TokenCounter>) -> Self {
        Self {
            max_tokens,
            consumed_tokens: 0,
            counter,
        }
    }

    /// Record actual API-reported token usage (accumulated).
    pub fn record_usage(&mut self, usage: &TokenUsage) {
        self.consumed_tokens = self.consumed_tokens.saturating_add(usage.total_tokens);
    }

    /// Estimate total tokens across messages using the provider counter.
    pub fn estimate_messages(&self, messages: &[ChatMessage]) -> u32 {
        messages
            .iter()
            .map(|m| self.counter.count_tokens(&m.content))
            .sum()
    }

    /// Utilization ratio: consumed / max (may exceed 1.0).
    pub fn utilization(&self) -> f32 {
        if self.max_tokens == 0 {
            return 0.0;
        }
        self.consumed_tokens as f32 / self.max_tokens as f32
    }

    /// Returns true when context compaction should be triggered (>80% utilized).
    pub fn should_compact(&self) -> bool {
        self.utilization() > 0.8
    }

    /// Available tokens (saturating at 0 if over budget).
    pub fn available_tokens(&self) -> u32 {
        self.max_tokens.saturating_sub(self.consumed_tokens)
    }

    /// Reset counters after compaction.
    pub fn reset(&mut self) {
        self.consumed_tokens = 0;
    }

    /// Current consumed token count.
    pub fn consumed_tokens(&self) -> u32 {
        self.consumed_tokens
    }

    /// Maximum token budget.
    pub fn max_tokens(&self) -> u32 {
        self.max_tokens
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sunny_mind::CharHeuristicCounter;

    fn make_budget(max: u32) -> TokenBudget {
        TokenBudget::new(max, Arc::new(CharHeuristicCounter))
    }

    #[test]
    fn test_token_budget_new() {
        let budget = make_budget(200_000);
        assert_eq!(budget.max_tokens(), 200_000);
        assert_eq!(budget.consumed_tokens(), 0);
        assert!(!budget.should_compact());
    }

    #[test]
    fn test_token_budget_record_usage() {
        let mut budget = make_budget(200_000);
        budget.record_usage(&TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            total_tokens: 150,
        });
        assert_eq!(budget.consumed_tokens(), 150);
    }

    #[test]
    fn test_token_budget_compaction_threshold() {
        let mut budget = make_budget(200_000);
        // Record 170k tokens (85% of 200k)
        budget.record_usage(&TokenUsage {
            input_tokens: 100_000,
            output_tokens: 70_000,
            total_tokens: 170_000,
        });
        assert!(
            budget.should_compact(),
            "should compact at >80% utilization"
        );
    }

    #[test]
    fn test_token_budget_below_threshold() {
        let mut budget = make_budget(200_000);
        // Record 100k tokens (50% of 200k)
        budget.record_usage(&TokenUsage {
            input_tokens: 60_000,
            output_tokens: 40_000,
            total_tokens: 100_000,
        });
        assert!(!budget.should_compact(), "should not compact below 80%");
    }

    #[test]
    fn test_token_budget_reset() {
        let mut budget = make_budget(200_000);
        budget.record_usage(&TokenUsage {
            input_tokens: 100_000,
            output_tokens: 70_000,
            total_tokens: 170_000,
        });
        assert!(budget.should_compact());
        budget.reset();
        assert_eq!(budget.consumed_tokens(), 0);
        assert!(!budget.should_compact());
    }

    #[test]
    fn test_token_budget_available_tokens() {
        let mut budget = make_budget(200_000);
        budget.record_usage(&TokenUsage {
            input_tokens: 50_000,
            output_tokens: 30_000,
            total_tokens: 80_000,
        });
        assert_eq!(budget.available_tokens(), 120_000);
    }

    #[test]
    fn test_token_budget_estimate_messages() {
        let budget = make_budget(200_000);
        let messages = vec![
            ChatMessage {
                role: sunny_mind::ChatRole::User,
                content: "hello world".to_string(), // 11 chars / 4 = 2
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
            ChatMessage {
                role: sunny_mind::ChatRole::Assistant,
                content: "response here".to_string(), // 13 chars / 4 = 3
                tool_calls: None,
                tool_call_id: None,
                reasoning_content: None,
            },
        ];
        let estimate = budget.estimate_messages(&messages);
        assert_eq!(estimate, 2 + 3); // 11/4 + 13/4
    }
}
