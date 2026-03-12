use crate::agent::Capability;
use crate::orchestrator::intent::{Intent, IntentKind};

const DEFAULT_ANALYZE_KEYWORDS: &[&str] = &["analyze", "scan", "review"];

#[cfg(test)]
const QUERY_KEYWORDS: &[&str] = &["what", "how", "explain", "describe"];

/// Keywords that map to `IntentKind::Action`.
const DEFAULT_ACTION_KEYWORDS: &[&str] = &["create", "add", "modify", "delete", "update"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifierConfig {
    pub analyze_keywords: Vec<String>,
    pub action_keywords: Vec<String>,
}

impl ClassifierConfig {
    pub fn from_env() -> Self {
        let defaults = Self::default();

        Self {
            analyze_keywords: parse_keyword_list_from_env(
                "SUNNY_ANALYZE_KEYWORDS",
                defaults.analyze_keywords,
            ),
            action_keywords: parse_keyword_list_from_env(
                "SUNNY_ACTION_KEYWORDS",
                defaults.action_keywords,
            ),
        }
    }
}

impl Default for ClassifierConfig {
    fn default() -> Self {
        Self {
            analyze_keywords: DEFAULT_ANALYZE_KEYWORDS
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
            action_keywords: DEFAULT_ACTION_KEYWORDS
                .iter()
                .map(|value| (*value).to_string())
                .collect(),
        }
    }
}

fn parse_keyword_list_from_env(key: &str, default: Vec<String>) -> Vec<String> {
    std::env::var(key)
        .ok()
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(|token| token.to_ascii_lowercase())
                .collect::<Vec<_>>()
        })
        .filter(|keywords| !keywords.is_empty())
        .unwrap_or(default)
}

/// Keyword-based intent classifier.
///
/// Stateless classifier that maps user input to an [`Intent`] by scanning
/// for known keywords. Matching is case-insensitive and uses simple
/// `str::contains()`. Falls back to [`IntentKind::Query`] when no keyword
/// matches (safest default — read-only semantics).
#[derive(Debug, Clone)]
pub struct IntentClassifier {
    config: ClassifierConfig,
}

impl IntentClassifier {
    /// Create a new `IntentClassifier`.
    pub fn new(config: ClassifierConfig) -> Self {
        Self { config }
    }

    /// Classify user input into an [`Intent`].
    ///
    /// Scans `input` for keywords in priority order: Analyze → Action → Query.
    /// Returns `IntentKind::Query` as fallback when no keyword matches.
    /// Sets `required_capability` based on the matched kind.
    pub fn classify(&self, input: &str) -> Intent {
        let lower = input.to_lowercase();

        let kind = if self
            .config
            .analyze_keywords
            .iter()
            .any(|kw| lower.contains(kw))
        {
            IntentKind::Analyze
        } else if self
            .config
            .action_keywords
            .iter()
            .any(|kw| lower.contains(kw))
        {
            IntentKind::Action
        } else {
            IntentKind::Query
        };

        let capability_name = match kind {
            IntentKind::Analyze => "analyze",
            IntentKind::Query => "query",
            IntentKind::Action => "action",
        };

        Intent {
            kind,
            raw_input: input.to_string(),
            required_capability: Some(Capability(capability_name.to_string())),
        }
    }
}

impl Default for IntentClassifier {
    fn default() -> Self {
        Self::new(ClassifierConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classifier() -> IntentClassifier {
        IntentClassifier::default()
    }

    #[test]
    fn test_classifier_analyze_keyword() {
        let intent = classifier().classify("analyze the codebase");
        assert_eq!(intent.kind, IntentKind::Analyze);
        assert_eq!(
            intent.required_capability,
            Some(Capability("analyze".to_string()))
        );
    }

    #[test]
    fn test_classifier_scan_keyword() {
        let intent = classifier().classify("scan the directory for issues");
        assert_eq!(intent.kind, IntentKind::Analyze);
    }

    #[test]
    fn test_classifier_review_keyword() {
        let intent = classifier().classify("review this pull request");
        assert_eq!(intent.kind, IntentKind::Analyze);
    }

    #[test]
    fn test_classifier_what_keyword() {
        let intent = classifier().classify("what is the status?");
        assert_eq!(intent.kind, IntentKind::Query);
        assert_eq!(
            intent.required_capability,
            Some(Capability("query".to_string()))
        );
    }

    #[test]
    fn test_classifier_how_keyword() {
        let intent = classifier().classify("how does this work?");
        assert_eq!(intent.kind, IntentKind::Query);
    }

    #[test]
    fn test_classifier_explain_keyword() {
        let intent = classifier().classify("explain the architecture");
        assert_eq!(intent.kind, IntentKind::Query);
    }

    #[test]
    fn test_classifier_describe_keyword() {
        let intent = classifier().classify("describe the module layout");
        assert_eq!(intent.kind, IntentKind::Query);
    }

    #[test]
    fn test_classifier_create_keyword() {
        let intent = classifier().classify("create a new file");
        assert_eq!(intent.kind, IntentKind::Action);
        assert_eq!(
            intent.required_capability,
            Some(Capability("action".to_string()))
        );
    }

    #[test]
    fn test_classifier_add_keyword() {
        let intent = classifier().classify("add a new test case");
        assert_eq!(intent.kind, IntentKind::Action);
    }

    #[test]
    fn test_classifier_modify_keyword() {
        let intent = classifier().classify("modify the configuration");
        assert_eq!(intent.kind, IntentKind::Action);
    }

    #[test]
    fn test_classifier_delete_keyword() {
        let intent = classifier().classify("delete the old log files");
        assert_eq!(intent.kind, IntentKind::Action);
    }

    #[test]
    fn test_classifier_update_keyword() {
        let intent = classifier().classify("update the dependencies");
        assert_eq!(intent.kind, IntentKind::Action);
    }

    #[test]
    fn test_classifier_fallback_to_query() {
        let intent = classifier().classify("hello world");
        assert_eq!(intent.kind, IntentKind::Query);
        assert_eq!(
            intent.required_capability,
            Some(Capability("query".to_string()))
        );
    }

    #[test]
    fn test_classifier_empty_input_fallback() {
        let intent = classifier().classify("");
        assert_eq!(intent.kind, IntentKind::Query);
    }

    #[test]
    fn test_classifier_case_insensitive_uppercase() {
        let intent = classifier().classify("ANALYZE the code");
        assert_eq!(intent.kind, IntentKind::Analyze);
    }

    #[test]
    fn test_classifier_case_insensitive_mixed() {
        let intent = classifier().classify("Please Create a new service");
        assert_eq!(intent.kind, IntentKind::Action);
    }

    #[test]
    fn test_classifier_preserves_raw_input() {
        let input = "Analyze The Codebase";
        let intent = classifier().classify(input);
        assert_eq!(intent.raw_input, input);
    }

    #[test]
    fn test_classifier_analyze_priority_over_action() {
        let intent = classifier().classify("analyze and update the config");
        assert_eq!(intent.kind, IntentKind::Analyze);
    }

    #[test]
    fn test_classifier_analyze_priority_over_query() {
        let intent = classifier().classify("analyze what went wrong");
        assert_eq!(intent.kind, IntentKind::Analyze);
    }

    #[test]
    fn test_classifier_default() {
        let c: IntentClassifier = Default::default();
        let intent = c.classify("scan files");
        assert_eq!(intent.kind, IntentKind::Analyze);
    }

    #[test]
    fn test_classifier_all_query_keywords_produce_query() {
        let c = classifier();
        for kw in QUERY_KEYWORDS {
            let intent = c.classify(kw);
            assert_eq!(
                intent.kind,
                IntentKind::Query,
                "keyword '{kw}' should map to Query"
            );
        }
    }

    #[test]
    fn test_classifier_with_custom_keywords() {
        let classifier = IntentClassifier::new(ClassifierConfig {
            analyze_keywords: vec!["analyze".to_string(), "investigate".to_string()],
            action_keywords: vec!["create".to_string()],
        });

        let intent = classifier.classify("please investigate this issue");
        assert_eq!(intent.kind, IntentKind::Analyze);
    }
}
