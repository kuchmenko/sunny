use regex::RegexBuilder;
use tracing::info;

use crate::events::{EVENT_TOOL_EXEC_END, EVENT_TOOL_EXEC_START, OUTCOME_SUCCESS};

/// In-memory text search with pattern matching.
///
/// Operates on string content (no file I/O). Case-insensitive by default.
/// Returns matches directly — grep on valid content cannot fail.
pub struct TextGrep {
    pub max_matches: usize,
    pub case_sensitive: bool,
}

#[derive(Debug)]
pub struct GrepMatch {
    /// 1-based line number.
    pub line_number: usize,
    /// Full content of the matching line.
    pub line_content: String,
    /// Byte offset of the match start within the line.
    pub match_start: usize,
    pub context_before: Vec<String>,
    pub context_after: Vec<String>,
}

/// Result of a grep search. Always succeeds — no `Result` wrapper needed.
pub struct GrepResult {
    pub matches: Vec<GrepMatch>,
    /// `true` if `max_matches` was hit and results were truncated.
    pub truncated: bool,
    pub total_lines_searched: usize,
}

impl Default for TextGrep {
    fn default() -> Self {
        Self {
            max_matches: 100,
            case_sensitive: false,
        }
    }
}

impl TextGrep {
    /// Search `content` for lines containing `pattern`.
    ///
    /// Returns `GrepResult` directly — this operation cannot fail on valid input.
    pub fn search(&self, content: &str, pattern: &str) -> GrepResult {
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "text_grep", pattern = %pattern);

        let mut matches = Vec::new();
        let mut truncated = false;
        let mut total_lines_searched = 0;

        if pattern.is_empty() {
            info!(name: EVENT_TOOL_EXEC_END, tool_name = "text_grep", outcome = OUTCOME_SUCCESS, match_count = 0, truncated = false);
            return GrepResult {
                matches,
                truncated,
                total_lines_searched,
            };
        }

        let pattern_lower = pattern.to_lowercase();
        let compiled_regex = RegexBuilder::new(pattern)
            .case_insensitive(!self.case_sensitive)
            .size_limit(1 << 20)
            .build();

        for (idx, line) in content.lines().enumerate() {
            total_lines_searched += 1;

            let match_pos = match &compiled_regex {
                Ok(re) => re.find(line).map(|m| m.start()),
                Err(_) => {
                    if self.case_sensitive {
                        line.find(pattern)
                    } else {
                        line.to_lowercase().find(&pattern_lower)
                    }
                }
            };

            if let Some(pos) = match_pos {
                if matches.len() >= self.max_matches {
                    truncated = true;
                    break;
                }
                matches.push(GrepMatch {
                    line_number: idx + 1,
                    line_content: line.to_string(),
                    match_start: pos,
                    context_before: Vec::new(),
                    context_after: Vec::new(),
                });
            }
        }

        info!(name: EVENT_TOOL_EXEC_END, tool_name = "text_grep", outcome = OUTCOME_SUCCESS, match_count = matches.len(), truncated = truncated);

        GrepResult {
            matches,
            truncated,
            total_lines_searched,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_grep_finds_matching_lines() {
        let grep = TextGrep::default();
        let content = "hello world\nfoo bar\nhello again\nnothing here\nhello end";
        let result = grep.search(content, "hello");

        assert_eq!(result.matches.len(), 3);
        assert_eq!(result.matches[0].line_content, "hello world");
        assert_eq!(result.matches[1].line_content, "hello again");
        assert_eq!(result.matches[2].line_content, "hello end");
        assert!(!result.truncated);
    }

    #[test]
    fn test_grep_case_insensitive() {
        let grep = TextGrep::default();
        let content = "TODO fix this\ntodo also here\nToDo mixed case\nno match";
        let result = grep.search(content, "TODO");

        assert_eq!(result.matches.len(), 3);
        assert_eq!(result.matches[0].line_content, "TODO fix this");
        assert_eq!(result.matches[1].line_content, "todo also here");
        assert_eq!(result.matches[2].line_content, "ToDo mixed case");
    }

    #[test]
    fn test_grep_respects_max_matches() {
        let grep = TextGrep {
            max_matches: 10,
            case_sensitive: false,
        };
        let content: String = (0..100)
            .map(|i| format!("line {} match", i))
            .collect::<Vec<_>>()
            .join("\n");
        let result = grep.search(&content, "match");

        assert_eq!(result.matches.len(), 10);
        assert!(result.truncated);
    }

    #[test]
    fn test_grep_no_matches() {
        let grep = TextGrep::default();
        let content = "hello world\nfoo bar\nbaz qux";
        let result = grep.search(content, "zzzzz");

        assert!(result.matches.is_empty());
        assert!(!result.truncated);
        assert_eq!(result.total_lines_searched, 3);
    }

    #[test]
    fn test_grep_empty_content() {
        let grep = TextGrep::default();
        let result = grep.search("", "pattern");

        assert!(result.matches.is_empty());
        assert!(!result.truncated);
        assert_eq!(result.total_lines_searched, 0);
    }

    #[test]
    fn test_grep_returns_line_numbers() {
        let grep = TextGrep::default();
        let content = "no match\nfind me\nno match\nno match\nfind me again";
        let result = grep.search(content, "find");

        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.matches[0].line_number, 2);
        assert_eq!(result.matches[0].match_start, 0);
        assert_eq!(result.matches[1].line_number, 5);
        assert_eq!(result.matches[1].match_start, 0);
    }

    #[test]
    fn test_grep_regex_or_pattern() {
        let grep = TextGrep::default();
        let content = "fn foo\nstruct Bar\nimpl Baz\nlet x = 1";
        let result = grep.search(content, "fn|struct|impl");

        assert_eq!(result.matches.len(), 3);
    }

    #[test]
    fn test_grep_invalid_regex_fallback() {
        let grep = TextGrep::default();
        let content = "[unclosed bracket here\nother line";
        let result = grep.search(content, "[unclosed");

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].match_start, 0);
    }

    #[test]
    fn test_grep_regex_case_insensitive() {
        let grep = TextGrep {
            max_matches: 100,
            case_sensitive: false,
        };
        let content = "todo fix\nFIXME now\nTodo later\nnothing";
        let result = grep.search(content, "TODO|FIXME");

        assert_eq!(result.matches.len(), 3);
    }

    #[test]
    fn test_grep_regex_match_start() {
        let grep = TextGrep::default();
        let content = "hello world\nfoo bar_baz";
        let result = grep.search(content, "world");

        assert_eq!(result.matches.len(), 1);
        assert_eq!(result.matches[0].match_start, 6);
    }

    #[test]
    #[tracing_test::traced_test]
    fn test_grep_tracing() {
        let grep = TextGrep::default();
        let content = "hello world\nfoo bar\nhello again";
        let result = grep.search(content, "hello");

        assert_eq!(result.matches.len(), 2);
        assert!(!result.truncated);

        assert!(logs_contain("text_grep"));
        assert!(logs_contain("tool_name"));
        assert!(logs_contain("hello"));
        assert!(logs_contain("match_count"));
        assert!(logs_contain("truncated"));
        assert!(logs_contain("success"));
    }
}
