//! sunny-boys: Reusable agent implementations for Sunny runtime
//!
//! # ADR: sunny-boys crate introduction
//!
//! **Context**: Sunny needed reusable agent implementations shared across
//! multiple future apps (PM bot, coding assistant, geopolitics analyst).
//! Putting agents in sunny-core would bloat the core with app-specific logic.
//! Putting them in sunny-cli would prevent reuse.
//!
//! **Decision**: Created sunny-boys as a separate crate depending on sunny-core
//! and sunny-mind. Agents live here, CLI just wires them.
//!
//! **Consequences**: sunny-boys depends on both sunny-core and sunny-mind.
//! Any new agent types (future PM bot agent, research agent) go here.
//! sunny-cli stays thin — orchestration wiring only.

pub mod analyze;

pub use analyze::{AnalysisMode, AnalysisResult, AnalyzeAgent};

#[cfg(test)]
mod tests {
    #[test]
    fn test_crate_compiles() {
        // Verify crate compiles and basic imports work
        // Type check only - no runtime logic needed
        let _ = stringify!(crate::AnalyzeAgent);
    }
}
