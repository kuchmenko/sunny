use crate::orchestrator::IntentKind;
use crate::orchestrator::WorkspaceExtensions;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub struct GateConfig {
    min_response_length: usize,
    min_alignment_score: f32,
    min_citation_count: usize,
    min_term_length: usize,
}

impl GateConfig {
    pub fn from_env() -> GateConfig {
        let default = GateConfig::with_defaults();
        let min_length = std::env::var("SUNNY_GATE_MIN_LENGTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default.min_response_length);
        let min_alignment = std::env::var("SUNNY_GATE_MIN_ALIGNMENT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default.min_alignment_score);
        let min_citations = std::env::var("SUNNY_GATE_MIN_CITATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default.min_citation_count);
        let min_term = std::env::var("SUNNY_GATE_MIN_TERM_LENGTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(default.min_term_length);

        GateConfig {
            min_response_length: min_length,
            min_alignment_score: min_alignment,
            min_citation_count: min_citations,
            min_term_length: min_term,
        }
    }

    pub fn with_defaults() -> GateConfig {
        GateConfig {
            min_response_length: 120,
            min_alignment_score: 0.12,
            min_citation_count: 1,
            min_term_length: 4,
        }
    }
}

impl Default for GateConfig {
    fn default() -> Self {
        GateConfig::with_defaults()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionGateResult {
    pub passed: bool,
    pub reason_code: String,
    pub details: HashMap<String, String>,
}

pub fn evaluate_completion(
    intent_kind: IntentKind,
    request: &str,
    response: &str,
    metadata: &HashMap<String, String>,
    config: &GateConfig,
    extensions: &WorkspaceExtensions,
) -> CompletionGateResult {
    match intent_kind {
        IntentKind::Analyze => {
            evaluate_analyze_completion(request, response, metadata, config, extensions)
        }
        IntentKind::Query | IntentKind::Action => CompletionGateResult {
            passed: true,
            reason_code: "passed_non_analyze".to_string(),
            details: HashMap::new(),
        },
    }
}

fn evaluate_analyze_completion(
    request: &str,
    response: &str,
    metadata: &HashMap<String, String>,
    config: &GateConfig,
    extensions: &WorkspaceExtensions,
) -> CompletionGateResult {
    let mut details = HashMap::new();
    let citation_count = count_citations(response, extensions);
    let min_len_ok = response.trim().len() >= config.min_response_length;
    let mode = metadata.get("mode").cloned().unwrap_or_default();
    let request_alignment = lexical_alignment_score(request, response, config);

    details.insert(
        "response_len".to_string(),
        response.trim().len().to_string(),
    );
    details.insert("citation_count".to_string(), citation_count.to_string());
    details.insert("mode".to_string(), mode);
    details.insert(
        "request_alignment".to_string(),
        format!("{request_alignment:.3}"),
    );

    let passed = min_len_ok
        && request_alignment >= config.min_alignment_score
        && citation_count >= config.min_citation_count;
    let reason_code = if passed {
        "passed_analyze".to_string()
    } else if !min_len_ok {
        "failed_too_short".to_string()
    } else if request_alignment < config.min_alignment_score {
        "failed_low_request_alignment".to_string()
    } else {
        "failed_missing_citations".to_string()
    };

    CompletionGateResult {
        passed,
        reason_code,
        details,
    }
}

fn lexical_alignment_score(request: &str, response: &str, config: &GateConfig) -> f32 {
    let request_terms = normalize_terms(request, config.min_term_length);
    let response_terms = normalize_terms(response, config.min_term_length);
    if request_terms.is_empty() || response_terms.is_empty() {
        return 0.0;
    }
    let overlap = request_terms
        .iter()
        .filter(|term| response_terms.contains(*term))
        .count();
    overlap as f32 / request_terms.len() as f32
}

fn normalize_terms(text: &str, min_term_length: usize) -> std::collections::HashSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() >= min_term_length)
        .map(|term| term.to_ascii_lowercase())
        .collect()
}

fn count_citations(response: &str, extensions: &WorkspaceExtensions) -> usize {
    let mut count = 0usize;
    for token in response.split_whitespace() {
        let candidate = token.trim_matches(|ch: char| {
            ch == ','
                || ch == ';'
                || ch == ':'
                || ch == ')'
                || ch == '('
                || ch == '"'
                || ch == '\''
                || ch == '`'
        });
        if candidate.contains('/') && extensions.is_code_file(candidate) {
            count += 1;
        }
    }
    count + response.matches('`').count() / 2
}

#[cfg(test)]
mod tests {
    use super::count_citations;
    use super::GateConfig;
    use crate::orchestrator::WorkspaceExtensions;

    #[test]
    fn test_gate_config_defaults() {
        let cfg = GateConfig::default();
        assert_eq!(cfg.min_response_length, 120);
        assert_eq!(cfg.min_alignment_score, 0.12);
        assert_eq!(cfg.min_citation_count, 1);
        assert_eq!(cfg.min_term_length, 4);
    }

    #[test]
    fn test_gate_config_custom_thresholds() {
        // emulate environment-based config
        std::env::set_var("SUNNY_GATE_MIN_LENGTH", "150");
        std::env::set_var("SUNNY_GATE_MIN_ALIGNMENT", "0.25");
        std::env::set_var("SUNNY_GATE_MIN_CITATIONS", "2");
        std::env::set_var("SUNNY_GATE_MIN_TERM_LENGTH", "5");
        let cfg = GateConfig::from_env();
        assert_eq!(cfg.min_response_length, 150);
        assert_eq!(cfg.min_alignment_score, 0.25);
        assert_eq!(cfg.min_citation_count, 2);
        assert_eq!(cfg.min_term_length, 5);
        // clear env for other tests
        std::env::remove_var("SUNNY_GATE_MIN_LENGTH");
        std::env::remove_var("SUNNY_GATE_MIN_ALIGNMENT");
        std::env::remove_var("SUNNY_GATE_MIN_CITATIONS");
        std::env::remove_var("SUNNY_GATE_MIN_TERM_LENGTH");
    }

    #[test]
    fn test_citations_count_python() {
        let extensions = WorkspaceExtensions::common_extensions();
        let response = "See src/main.py and app/utils/helpers.py for the implementation.";

        assert_eq!(count_citations(response, &extensions), 2);
    }

    #[test]
    fn test_citations_count_js() {
        let extensions = WorkspaceExtensions::common_extensions();
        let response = "Frontend updates are in src/app.ts and web/components/button.tsx";

        assert_eq!(count_citations(response, &extensions), 2);
    }
}
