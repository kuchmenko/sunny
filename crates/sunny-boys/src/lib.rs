//! sunny-boys: Tool loops and git tools for Sunny chat runtime
//!
//! # ADR: sunny-boys slim crate
//!
//! **Context**: Sunny chat needs reusable tool loop infrastructure and git
//! tools shared across contexts. Keeping these separate from sunny-cli
//! allows future reuse without CLI coupling.
//!
//! **Decision**: sunny-boys retains only tool loops (streaming and basic),
//! git tools, and timeout configuration. All multi-agent implementations
//! removed in scorched-earth cleanup.
//!
//! **Consequences**: sunny-boys is now a slim utility crate. CLI stays thin.

pub mod git_tools;
pub mod streaming_tool_loop;
pub(crate) mod timeouts;
pub mod tool_loop;

pub use git_tools::{GitDiff, GitLog, GitStatus};
pub use streaming_tool_loop::{StreamingToolLoop, StreamingToolMetrics, StreamingToolResult};
pub use tool_loop::{ToolCallError, ToolCallLoop, ToolCallMetrics, ToolCallResult};

#[cfg(test)]
mod tests {
    #[test]
    fn test_crate_compiles() {
        // Verify crate compiles and basic imports work
        let _ = stringify!(crate::StreamingToolLoop);
    }
}
