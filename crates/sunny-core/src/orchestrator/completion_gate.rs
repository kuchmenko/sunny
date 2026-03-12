use std::collections::HashMap;

use super::IntentKind;

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
) -> CompletionGateResult {
    match intent_kind {
        IntentKind::Analyze => evaluate_analyze_completion(request, response, metadata),
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
) -> CompletionGateResult {
    let mut details = HashMap::new();
    let citation_count = count_citations(response);
    let min_len_ok = response.trim().len() >= 120;
    let mode = metadata.get("mode").cloned().unwrap_or_default();
    let request_alignment = lexical_alignment_score(request, response);

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

    let passed = min_len_ok && request_alignment >= 0.12 && citation_count >= 1;
    let reason_code = if passed {
        "passed_analyze".to_string()
    } else if !min_len_ok {
        "failed_too_short".to_string()
    } else if request_alignment < 0.12 {
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

fn lexical_alignment_score(request: &str, response: &str) -> f32 {
    let request_terms = normalize_terms(request);
    let response_terms = normalize_terms(response);
    if request_terms.is_empty() || response_terms.is_empty() {
        return 0.0;
    }
    let overlap = request_terms
        .iter()
        .filter(|term| response_terms.contains(*term))
        .count();
    overlap as f32 / request_terms.len() as f32
}

fn normalize_terms(text: &str) -> std::collections::HashSet<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|term| term.len() >= 4)
        .map(|term| term.to_ascii_lowercase())
        .collect()
}

fn count_citations(response: &str) -> usize {
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
        if candidate.contains('/')
            && (candidate.ends_with(".rs")
                || candidate.ends_with(".toml")
                || candidate.ends_with(".md"))
        {
            count += 1;
        }
    }
    count += response.matches('`').count() / 2;
    count
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{evaluate_completion, IntentKind};

    #[test]
    fn test_analyze_gate_fails_for_low_alignment_and_no_citations() {
        let request = "Analyze planner and orchestrator code paths";
        let response = "This output is generic and ungrounded.";
        let result = evaluate_completion(IntentKind::Analyze, request, response, &HashMap::new());
        assert!(!result.passed);
    }

    #[test]
    fn test_analyze_gate_passes_for_generic_grounded_synthesis() {
        let request = "Analyze planner and orchestrator code";
        let response = "Planner flow in `crates/sunny-core/src/orchestrator/planner.rs` shapes staged decisions, while executor behavior in `crates/sunny-core/src/orchestrator/executor.rs` governs retries and state transitions for run stability.";
        let result = evaluate_completion(IntentKind::Analyze, request, response, &HashMap::new());
        assert!(result.passed);
    }
}
