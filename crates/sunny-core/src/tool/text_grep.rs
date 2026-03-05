/// In-memory text search with pattern matching.
///
/// Operates on string content (no file I/O). Case-insensitive by default.
/// Returns matches directly — grep on valid content cannot fail.
pub struct TextGrep {
    pub max_matches: usize,
    pub case_sensitive: bool,
}

/// A single matching line from a grep search.
pub struct GrepMatch {
    /// 1-based line number.
    pub line_number: usize,
    /// Full content of the matching line.
    pub line_content: String,
    /// Byte offset of the match start within the line.
    pub match_start: usize,
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
        let mut matches = Vec::new();
        let mut truncated = false;
        let mut total_lines_searched = 0;

        if pattern.is_empty() {
            return GrepResult {
                matches,
                truncated,
                total_lines_searched,
            };
        }

        let pattern_lower = pattern.to_lowercase();

        for (idx, line) in content.lines().enumerate() {
            total_lines_searched += 1;

            let match_pos = if self.case_sensitive {
                line.find(pattern)
            } else {
                line.to_lowercase().find(&pattern_lower)
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
                });
            }
        }

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
}
