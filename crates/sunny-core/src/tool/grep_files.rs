use std::path::PathBuf;

use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{
    BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch,
};
use ignore::WalkBuilder;
use regex::RegexBuilder as StdRegexBuilder;
use tracing::info;

use crate::events::{EVENT_TOOL_EXEC_END, EVENT_TOOL_EXEC_START, OUTCOME_SUCCESS};
use crate::tool::error::ToolError;
use crate::tool::path_guard::PathGuard;
use crate::tool::text_grep::GrepMatch;

#[derive(Debug)]
pub struct GrepFileMatch {
    pub path: PathBuf,
    pub matches: Vec<GrepMatch>,
}

pub struct GrepFiles {
    guard: PathGuard,
}

struct LineMatcher {
    regex: Option<regex::Regex>,
    pattern_lower: String,
}

impl LineMatcher {
    fn new(pattern: &str) -> Self {
        Self {
            regex: StdRegexBuilder::new(pattern)
                .case_insensitive(true)
                .size_limit(1 << 20)
                .build()
                .ok(),
            pattern_lower: pattern.to_lowercase(),
        }
    }

    fn match_start(&self, line: &str) -> usize {
        self.regex
            .as_ref()
            .and_then(|regex| regex.find(line).map(|mat| mat.start()))
            .unwrap_or_else(|| line.to_lowercase().find(&self.pattern_lower).unwrap_or(0))
    }
}

struct MatchCollector<'a> {
    line_matcher: &'a LineMatcher,
    matches: Vec<GrepMatch>,
    pending_before: Vec<String>,
    max_matches: usize,
}

impl<'a> MatchCollector<'a> {
    fn new(line_matcher: &'a LineMatcher, max_matches: usize) -> Self {
        Self {
            line_matcher,
            matches: Vec::new(),
            pending_before: Vec::new(),
            max_matches,
        }
    }

    fn into_matches(self) -> Vec<GrepMatch> {
        self.matches
    }

    fn normalize_line(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes)
            .trim_end_matches(['\r', '\n'])
            .to_string()
    }
}

impl Sink for MatchCollector<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
        if self.matches.len() >= self.max_matches {
            return Ok(false);
        }

        let line = Self::normalize_line(mat.bytes());
        self.matches.push(GrepMatch {
            line_number: mat.line_number().unwrap_or(0) as usize,
            line_content: line.clone(),
            match_start: self.line_matcher.match_start(&line),
            context_before: std::mem::take(&mut self.pending_before),
            context_after: Vec::new(),
        });

        Ok(self.matches.len() < self.max_matches)
    }

    fn context(
        &mut self,
        _searcher: &Searcher,
        context: &SinkContext<'_>,
    ) -> Result<bool, Self::Error> {
        let line = Self::normalize_line(context.bytes());

        match context.kind() {
            SinkContextKind::Before => self.pending_before.push(line),
            SinkContextKind::After => {
                if let Some(last_match) = self.matches.last_mut() {
                    last_match.context_after.push(line);
                }
            }
            SinkContextKind::Other => {}
        }

        Ok(true)
    }

    fn context_break(&mut self, _searcher: &Searcher) -> Result<bool, Self::Error> {
        self.pending_before.clear();
        Ok(true)
    }
}

impl GrepFiles {
    pub fn new(root: PathBuf) -> Result<Self, ToolError> {
        let guard = PathGuard::new(root)?;
        Ok(Self { guard })
    }

    pub fn search(
        &self,
        path: &str,
        pattern: &str,
        max_results: Option<usize>,
    ) -> Result<Vec<GrepFileMatch>, ToolError> {
        info!(name: EVENT_TOOL_EXEC_START, tool_name = "grep_files", path = path, pattern = %pattern);

        if pattern.is_empty() {
            info!(name: EVENT_TOOL_EXEC_END, tool_name = "grep_files", outcome = OUTCOME_SUCCESS, file_matches = 0, total_matches = 0);
            return Ok(Vec::new());
        }

        let max = max_results.unwrap_or(100);
        let root_path = self.guard.resolve(path)?;
        let mut results = Vec::new();
        let mut total_matches = 0usize;
        let line_matcher = LineMatcher::new(pattern);
        let matcher = build_regex_matcher(pattern)?;
        let mut searcher = SearcherBuilder::new()
            .before_context(3)
            .after_context(3)
            .binary_detection(BinaryDetection::quit(0))
            .line_number(true)
            .build();

        let walker = WalkBuilder::new(&root_path).standard_filters(true).build();

        for entry in walker.flatten() {
            if !entry
                .file_type()
                .map(|file_type| file_type.is_file())
                .unwrap_or(false)
            {
                continue;
            }

            let remaining = max.saturating_sub(total_matches);
            if remaining == 0 {
                break;
            }

            let entry_path = entry.path();
            let mut collector = MatchCollector::new(&line_matcher, remaining);

            if searcher
                .search_path(&matcher, entry_path, &mut collector)
                .is_err()
            {
                continue;
            }

            let file_matches = collector.into_matches();
            if file_matches.is_empty() {
                continue;
            }

            total_matches += file_matches.len();
            results.push(GrepFileMatch {
                path: entry_path.to_path_buf(),
                matches: file_matches,
            });
        }

        info!(name: EVENT_TOOL_EXEC_END, tool_name = "grep_files", outcome = OUTCOME_SUCCESS, file_matches = results.len(), total_matches = total_matches);

        Ok(results)
    }
}

fn build_regex_matcher(pattern: &str) -> Result<RegexMatcher, ToolError> {
    let mut builder = RegexMatcherBuilder::new();
    builder.case_insensitive(true);

    builder
        .build(pattern)
        .or_else(|_| builder.build(&regex::escape(pattern)))
        .map_err(|source| ToolError::ExecutionFailed {
            source: Box::new(source),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_grep_files_recursive_finds_matches() {
        let dir = tempdir().expect("create temp dir");
        let src_dir = dir.path().join("src");
        fs::create_dir(&src_dir).expect("create src dir");

        fs::write(
            src_dir.join("main.rs"),
            "fn main() {\n    println!(\"hello\");\n}",
        )
        .expect("write main.rs");
        fs::write(
            src_dir.join("lib.rs"),
            "fn lib_func() {\n    println!(\"lib\");\n}",
        )
        .expect("write lib.rs");

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files
            .search(".", "fn ", None)
            .expect("search succeeds");

        assert_eq!(results.len(), 2, "should find 2 files with matches");
        assert!(results.iter().any(|r| r.path.ends_with("main.rs")));
        assert!(results.iter().any(|r| r.path.ends_with("lib.rs")));
    }

    #[test]
    fn test_grep_files_max_results_respected() {
        let dir = tempdir().expect("create temp dir");

        for i in 0..3 {
            fs::write(
                dir.path().join(format!("file{i}.txt")),
                "match\nmatch\nmatch\nmatch\nmatch",
            )
            .expect("write file");
        }

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files
            .search(".", "match", Some(5))
            .expect("search succeeds");

        let total_matches: usize = results.iter().map(|r| r.matches.len()).sum();
        assert!(
            total_matches <= 5,
            "total matches {} should not exceed max_results 5",
            total_matches
        );
    }

    #[test]
    fn test_grep_files_skips_binary() {
        let dir = tempdir().expect("create temp dir");

        fs::write(dir.path().join("text.txt"), "match here").expect("write text file");

        let mut binary_data = b"some text".to_vec();
        binary_data.push(0u8);
        binary_data.extend_from_slice(b"more text with match");
        fs::write(dir.path().join("binary.bin"), &binary_data).expect("write binary file");

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files
            .search(".", "match", None)
            .expect("search succeeds");

        assert_eq!(results.len(), 1, "should find only 1 file (text.txt)");
        assert!(results[0].path.ends_with("text.txt"));
    }

    #[test]
    fn test_grep_files_respects_gitignore() {
        let dir = tempdir().expect("create temp dir");

        fs::write(dir.path().join("keep.txt"), "match here").expect("write keep.txt");
        fs::write(dir.path().join("ignore.log"), "match here").expect("write ignore.log");
        fs::write(dir.path().join(".gitignore"), "*.log\n").expect("write .gitignore");

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files
            .search(".", "match", None)
            .expect("search succeeds");

        assert!(
            results.iter().any(|r| r.path.ends_with("keep.txt")),
            "should find matches in keep.txt"
        );
    }

    #[test]
    fn test_grep_files_empty_pattern() {
        let dir = tempdir().expect("create temp dir");
        fs::write(dir.path().join("file.txt"), "some content").expect("write file");

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files.search(".", "", None).expect("search succeeds");

        assert_eq!(results.len(), 0, "empty pattern should find no matches");
    }

    #[test]
    fn test_grep_files_no_matches() {
        let dir = tempdir().expect("create temp dir");
        fs::write(dir.path().join("file.txt"), "hello world").expect("write file");

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files
            .search(".", "zzzzz", None)
            .expect("search succeeds");

        assert_eq!(results.len(), 0, "should find no matches");
    }

    #[test]
    fn test_grep_files_returns_context_lines() {
        let dir = tempdir().expect("create temp dir");
        fs::write(
            dir.path().join("file.txt"),
            "before one\nbefore two\nneedle here\nafter one\nafter two",
        )
        .expect("write file");

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files
            .search(".", "needle", None)
            .expect("search succeeds");

        let grep_match = &results[0].matches[0];
        assert_eq!(grep_match.context_before, vec!["before one", "before two"]);
        assert_eq!(grep_match.context_after, vec!["after one", "after two"]);
    }

    #[test]
    fn test_grep_files_backward_compat() {
        let dir = tempdir().expect("create temp dir");
        fs::write(dir.path().join("file.txt"), "Alpha\nBeta\nalpha again").expect("write file");

        let grep_files = GrepFiles::new(dir.path().to_path_buf()).expect("create GrepFiles");
        let results = grep_files
            .search(".", "alpha", None)
            .expect("search succeeds");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].matches.len(), 2);
        assert_eq!(results[0].matches[0].line_number, 1);
        assert_eq!(results[0].matches[0].line_content, "Alpha");
        assert_eq!(results[0].matches[0].match_start, 0);
        assert_eq!(results[0].matches[1].line_number, 3);
    }
}
