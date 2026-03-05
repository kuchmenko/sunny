//! Output formatting for analyze command results

use serde::{Deserialize, Serialize};

/// Output mode indicating whether LLM enrichment was used
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputMode {
    /// Analysis was enriched with LLM provider
    LlmEnriched,
    /// Analysis used tool-only fallback (no LLM)
    ToolOnlyFallback,
}

impl OutputMode {
    /// Get the mode as a string marker
    pub fn as_str(&self) -> &'static str {
        match self {
            OutputMode::LlmEnriched => "LLM_ENRICHED",
            OutputMode::ToolOnlyFallback => "TOOL_ONLY_FALLBACK",
        }
    }
}

/// Analysis output structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisOutput {
    /// Mode of analysis (LLM-enriched or tool-only)
    pub mode: OutputMode,
    /// Summary of analysis
    pub summary: String,
    /// Number of files analyzed
    pub file_count: usize,
    /// Total size in bytes
    pub total_size_bytes: u64,
    /// Language statistics: (extension, count)
    pub language_stats: Vec<(String, usize)>,
    /// Optional LLM-generated summary
    pub llm_summary: Option<String>,
}

/// Format output as plain text
pub fn format_text(output: &AnalysisOutput) -> String {
    let mut result = String::new();
    result.push_str(&format!("Mode: {}\n", output.mode.as_str()));
    result.push_str(&format!("Summary: {}\n", output.summary));
    result.push_str(&format!("Files: {}\n", output.file_count));
    result.push_str(&format!("Total Size: {} bytes\n", output.total_size_bytes));

    if !output.language_stats.is_empty() {
        result.push_str("Languages:\n");
        for (ext, count) in &output.language_stats {
            result.push_str(&format!("  {}: {}\n", ext, count));
        }
    }

    if let Some(llm_summary) = &output.llm_summary {
        result.push_str(&format!("LLM Summary: {}\n", llm_summary));
        result.push_str("Note: code snippets were sent to cloud LLM provider for analysis\n");
    }

    result
}

/// Format output as JSON
pub fn format_json(output: &AnalysisOutput) -> String {
    serde_json::to_string_pretty(output).unwrap_or_else(|_| "{}".to_string())
}

/// Format output as pretty human-readable text
pub fn format_pretty(output: &AnalysisOutput) -> String {
    let mut result = String::new();
    result.push_str("╔════════════════════════════════════════╗\n");
    result.push_str("║         Code Analysis Report           ║\n");
    result.push_str("╚════════════════════════════════════════╝\n\n");

    result.push_str(&format!("📊 Mode: {}\n", output.mode.as_str()));
    result.push_str(&format!("📝 Summary: {}\n", output.summary));
    result.push_str(&format!("📁 Files: {}\n", output.file_count));
    result.push_str(&format!(
        "💾 Total Size: {} bytes\n",
        output.total_size_bytes
    ));

    if !output.language_stats.is_empty() {
        result.push_str("\n🔤 Languages:\n");
        for (ext, count) in &output.language_stats {
            result.push_str(&format!("   • .{}: {} files\n", ext, count));
        }
    }

    if let Some(llm_summary) = &output.llm_summary {
        result.push_str(&format!("\n🤖 LLM Analysis:\n{}\n", llm_summary));
        result.push_str("\n⚠️  Note: code snippets were sent to cloud LLM provider for analysis\n");
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_output() -> AnalysisOutput {
        AnalysisOutput {
            mode: OutputMode::LlmEnriched,
            summary: "Test codebase".to_string(),
            file_count: 10,
            total_size_bytes: 5000,
            language_stats: vec![("rs".to_string(), 8), ("toml".to_string(), 2)],
            llm_summary: Some("Well-structured code".to_string()),
        }
    }

    #[test]
    fn test_format_text_includes_mode() {
        let output = sample_output();
        let formatted = format_text(&output);
        assert!(formatted.contains("LLM_ENRICHED"));
    }

    #[test]
    fn test_format_text_includes_cloud_warning() {
        let output = sample_output();
        let formatted = format_text(&output);
        assert!(formatted.contains("cloud LLM provider"));
    }

    #[test]
    fn test_format_json_is_valid() {
        let output = sample_output();
        let formatted = format_json(&output);
        let parsed: serde_json::Value = serde_json::from_str(&formatted).expect("valid JSON");
        assert!(parsed.is_object());
    }

    #[test]
    fn test_format_pretty_includes_mode() {
        let output = sample_output();
        let formatted = format_pretty(&output);
        assert!(formatted.contains("LLM_ENRICHED"));
    }

    #[test]
    fn test_output_mode_as_str() {
        assert_eq!(OutputMode::LlmEnriched.as_str(), "LLM_ENRICHED");
        assert_eq!(OutputMode::ToolOnlyFallback.as_str(), "TOOL_ONLY_FALLBACK");
    }
}
