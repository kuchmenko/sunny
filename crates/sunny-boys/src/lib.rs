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
pub mod background;
pub mod codebase;
pub mod critique;
pub mod delegate;
pub mod events;
pub mod explore;
pub mod git_tools;
pub mod oracle;
pub mod registry;
pub mod review;
pub mod tavily;
pub(crate) mod timeouts;
pub mod tool_loop;

pub use analyze::{AnalysisMode, AnalysisResult, AnalyzeAgent};
pub use background::{BackgroundError, BackgroundTaskManager, TaskId, TaskResult, TaskStatus};
pub use codebase::WorkspaceReadAgent;
pub use critique::CritiqueAgent;
pub use delegate::DelegateAgent;
pub use explore::ExploreAgent;
pub use git_tools::{GitDiff, GitLog, GitStatus};
pub use oracle::OracleAgent;
pub use registry::build_boys_registry;
pub use review::ReviewAgent;
pub use tavily::TavilySearch;
pub use tool_loop::{ToolCallError, ToolCallLoop, ToolCallMetrics, ToolCallResult};

#[cfg(test)]
mod tests {
    #[test]
    fn test_crate_compiles() {
        // Verify crate compiles and basic imports work
        // Type check only - no runtime logic needed
        let _ = stringify!(crate::AnalyzeAgent);
    }
}
