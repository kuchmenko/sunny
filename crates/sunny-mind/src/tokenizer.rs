//! Token counting abstractions for different LLM providers.
//!
//! Provides a trait-based interface for counting tokens across different LLM providers,
//! with fallback heuristic and exact tiktoken-based implementations.



/// Abstracts token counting across different LLM providers.
///
/// Implementations must be `Send + Sync` for use in `Arc<dyn TokenCounter>`.
pub trait TokenCounter: Send + Sync {
    /// Count the number of tokens in the given text.
    fn count_tokens(&self, text: &str) -> u32;
}

/// Fallback token counter using character-based heuristic.
///
/// Estimates tokens as approximately 1 token per 4 characters.
/// Used for providers without public tokenizers (e.g., Claude models).
#[derive(Clone, Debug)]
pub struct CharHeuristicCounter;

impl TokenCounter for CharHeuristicCounter {
    fn count_tokens(&self, text: &str) -> u32 {
        (text.len() as u32) / 4
    }
}

/// Exact token counting using tiktoken-rs for OpenAI-compatible models.
///
/// Provides accurate token counts for GPT-3.5, GPT-4, and other OpenAI models.
/// Returns `None` for unsupported models (e.g., Claude models).
#[derive(Clone, Debug)]
pub struct TiktokenCounter {
    bpe: tiktoken_rs::CoreBPE,
}

impl TiktokenCounter {
    /// Create a TiktokenCounter for the given model.
    ///
    /// Returns `None` if the model is not supported by tiktoken-rs.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// if let Some(counter) = TiktokenCounter::for_model("gpt-4") {
    ///     let tokens = counter.count_tokens("hello world");
    /// }
    /// ```
    pub fn for_model(model: &str) -> Option<Self> {
        tiktoken_rs::get_bpe_from_model(model)
            .ok()
            .map(|bpe| Self { bpe })
    }
}

impl TokenCounter for TiktokenCounter {
    fn count_tokens(&self, text: &str) -> u32 {
        self.bpe.encode_with_special_tokens(text).len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn test_char_heuristic_returns_quarter_length() {
        let counter = CharHeuristicCounter;
        // "hello world" = 11 chars, 11 / 4 = 2
        assert_eq!(counter.count_tokens("hello world"), 2);
        // "a" = 1 char, 1 / 4 = 0
        assert_eq!(counter.count_tokens("a"), 0);
        // "abcdefgh" = 8 chars, 8 / 4 = 2
        assert_eq!(counter.count_tokens("abcdefgh"), 2);
        // "abcdefghijklmnop" = 16 chars, 16 / 4 = 4
        assert_eq!(counter.count_tokens("abcdefghijklmnop"), 4);
    }

    #[test]
    fn test_char_heuristic_empty_string() {
        let counter = CharHeuristicCounter;
        assert_eq!(counter.count_tokens(""), 0);
    }

    #[test]
    fn test_tiktoken_returns_some_for_gpt4() {
        let counter = TiktokenCounter::for_model("gpt-4");
        assert!(counter.is_some(), "gpt-4 should be supported by tiktoken");
    }

    #[test]
    fn test_tiktoken_returns_some_for_gpt35() {
        let counter = TiktokenCounter::for_model("gpt-3.5-turbo");
        assert!(
            counter.is_some(),
            "gpt-3.5-turbo should be supported by tiktoken"
        );
    }

    #[test]
    fn test_tiktoken_returns_none_for_claude() {
        let counter = TiktokenCounter::for_model("claude-sonnet-4-6");
        assert!(
            counter.is_none(),
            "claude models should not be supported by tiktoken"
        );
    }

    #[test]
    fn test_tiktoken_returns_none_for_unknown_model() {
        let counter = TiktokenCounter::for_model("unknown-model-xyz");
        assert!(counter.is_none(), "unknown models should return None");
    }

    #[test]
    fn test_tiktoken_counts_tokens_for_gpt4() {
        if let Some(counter) = TiktokenCounter::for_model("gpt-4") {
            let tokens = counter.count_tokens("hello world");
            // tiktoken should count this as 2 tokens
            assert_eq!(tokens, 2);
        }
    }

    #[test]
    fn test_token_counter_trait_object() {
        let heuristic: Arc<dyn TokenCounter> = Arc::new(CharHeuristicCounter);
        assert_eq!(heuristic.count_tokens("hello world"), 2);

        if let Some(tiktoken) = TiktokenCounter::for_model("gpt-4") {
            let tiktoken_obj: Arc<dyn TokenCounter> = Arc::new(tiktoken);
            let tokens = tiktoken_obj.count_tokens("hello world");
            assert!(tokens > 0);
        }
    }
}
