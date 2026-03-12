//! Output formatting for analyze command results

use std::collections::HashMap;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptIssue {
    pub level: String,
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptOutput {
    pub request_id: String,
    pub plan_id: String,
    pub intent_kind: String,
    pub required_capability: Option<String>,
    pub dry_run: bool,
    pub step_count: usize,
    pub steps_completed: usize,
    pub steps_failed: usize,
    pub steps_skipped: usize,
    pub outcome: String,
    pub response: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<PromptIssue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<PromptIssue>,
    pub metadata: HashMap<String, String>,
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

pub fn format_prompt_text(output: &PromptOutput) -> String {
    let mut result = String::new();
    result.push_str(&format!("Request ID: {}\n", output.request_id));
    result.push_str(&format!("Plan ID: {}\n", output.plan_id));
    result.push_str(&format!("Intent: {}\n", output.intent_kind));
    result.push_str(&format!("Outcome: {}\n", output.outcome));
    result.push_str(&format!("Dry Run: {}\n", output.dry_run));
    result.push_str(&format!("Steps: {}\n", output.step_count));
    result.push_str(&format!("Completed: {}\n", output.steps_completed));
    result.push_str(&format!("Failed: {}\n", output.steps_failed));
    result.push_str(&format!("Skipped: {}\n", output.steps_skipped));

    if let Some(capability) = &output.required_capability {
        result.push_str(&format!("Capability: {}\n", capability));
    }

    if let Some(response) = &output.response {
        result.push_str(&format!("Response: {}\n", response));
    }

    if !output.warnings.is_empty() {
        result.push_str("Warnings:\n");
        for warning in &output.warnings {
            result.push_str(&format!(
                "  [{}] {}: {}\n",
                warning.code, warning.level, warning.message
            ));
            if let Some(hint) = &warning.hint {
                result.push_str(&format!("    Hint: {}\n", hint));
            }
        }
    }

    if let Some(error) = &output.error {
        result.push_str("Error:\n");
        result.push_str(&format!(
            "  [{}] {}: {}\n",
            error.code, error.level, error.message
        ));
        if let Some(hint) = &error.hint {
            result.push_str(&format!("  Hint: {}\n", hint));
        }
    }

    result
}

pub fn format_prompt_json(output: &PromptOutput) -> String {
    serde_json::to_string_pretty(output).unwrap_or_else(|_| "{}".to_string())
}

pub fn format_prompt_pretty(output: &PromptOutput) -> String {
    let mut result = String::new();
    result.push_str("╔════════════════════════════════════════╗\n");
    result.push_str("║          Prompt Execution              ║\n");
    result.push_str("╚════════════════════════════════════════╝\n\n");

    result.push_str(&format!("🧭 Intent: {}\n", output.intent_kind));
    result.push_str(&format!("📋 Plan ID: {}\n", output.plan_id));
    result.push_str(&format!("🧾 Request ID: {}\n", output.request_id));
    result.push_str(&format!("✅ Outcome: {}\n", output.outcome));
    result.push_str(&format!("🧪 Dry Run: {}\n", output.dry_run));
    result.push_str(&format!(
        "📊 Steps: total={} completed={} failed={} skipped={}\n",
        output.step_count, output.steps_completed, output.steps_failed, output.steps_skipped
    ));

    if let Some(capability) = &output.required_capability {
        result.push_str(&format!("🔧 Capability: {}\n", capability));
    }

    if let Some(response) = &output.response {
        result.push_str(&format!("\n\u{1f4ac} Response:\n{}\n", response));
    }

    if !output.warnings.is_empty() {
        result.push_str("\n⚠️ Warnings:\n");
        for warning in &output.warnings {
            result.push_str(&format!("   • [{}] {}\n", warning.code, warning.message));
            if let Some(hint) = &warning.hint {
                result.push_str(&format!("     ↳ Hint: {}\n", hint));
            }
        }
    }

    if let Some(error) = &output.error {
        result.push_str(&format!("\n❌ Error [{}]: {}\n", error.code, error.message));
        if let Some(hint) = &error.hint {
            result.push_str(&format!("💡 Hint: {}\n", hint));
        }
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

    fn sample_prompt_output() -> PromptOutput {
        PromptOutput {
            request_id: "req-1".to_string(),
            plan_id: "plan-1".to_string(),
            intent_kind: "analyze".to_string(),
            required_capability: Some("analyze".to_string()),
            dry_run: false,
            step_count: 1,
            steps_completed: 1,
            steps_failed: 0,
            steps_skipped: 0,
            outcome: "success".to_string(),
            response: Some("ok".to_string()),
            warnings: Vec::new(),
            error: None,
            metadata: HashMap::new(),
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

    #[test]
    fn test_format_prompt_text_includes_plan_id() {
        let output = sample_prompt_output();
        let formatted = format_prompt_text(&output);
        assert!(formatted.contains("Plan ID: plan-1"));
    }

    #[test]
    fn test_format_prompt_json_is_valid() {
        let output = sample_prompt_output();
        let formatted = format_prompt_json(&output);
        let parsed: serde_json::Value = serde_json::from_str(&formatted).expect("valid JSON");
        assert_eq!(parsed["plan_id"].as_str(), Some("plan-1"));
        assert_eq!(parsed["outcome"].as_str(), Some("success"));
    }

    #[test]
    fn test_format_prompt_pretty_includes_header() {
        let output = sample_prompt_output();
        let formatted = format_prompt_pretty(&output);
        assert!(formatted.contains("Prompt Execution"));
        assert!(formatted.contains("plan-1"));
    }

    #[test]
    fn test_format_prompt_text_large_response_not_truncated() {
        let mut output = sample_prompt_output();
        let long = "x".repeat(5000);
        output.response = Some(long.clone());

        let formatted = format_prompt_text(&output);
        assert!(
            formatted.contains(&long),
            "full response must appear in output"
        );
        assert!(!formatted.contains("Response truncated."));
    }
}
