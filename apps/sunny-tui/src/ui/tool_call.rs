//! Tool call widget — Option C rendering (spinner → collapse → expand).
//!
//! Running state: "⟳ tool_name  args_preview..."
//! Completed collapsed: "▸ tool_name  ✓ summary"
//! Completed expanded: "▾ tool_name  ✓ summary" + indented result
//! Failed: "▸ tool_name  ✗ error_message" (red)

#![allow(dead_code)]

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
};

use crate::thread::{ToolCallDisplay, ToolCallStatus};

/// Spinner animation frames cycling through on each tick.
pub const SPINNER_FRAMES: &[&str] = &["⟳", "○", "◔", "◑", "◕", "●"];

/// Render a tool call into ratatui `Text` lines for display in the thread view.
///
/// `tick_count` drives the spinner animation (mod SPINNER_FRAMES.len()).
/// Returns one or more `Line` items that can be collected into a `Text`.
pub fn render_tool_call(tc: &ToolCallDisplay, tick_count: usize) -> Text<'static> {
    match tc.status {
        ToolCallStatus::Running => render_running(tc, tick_count),
        ToolCallStatus::Completed => {
            if tc.collapsed {
                render_completed_collapsed(tc)
            } else {
                render_completed_expanded(tc)
            }
        }
        ToolCallStatus::Failed => render_failed(tc),
    }
}

fn render_running(tc: &ToolCallDisplay, tick_count: usize) -> Text<'static> {
    let spinner = SPINNER_FRAMES[tick_count % SPINNER_FRAMES.len()];
    let line = Line::from(vec![
        Span::styled(
            format!("{spinner} "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            tc.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            tc.args_preview.clone(),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    Text::from(line)
}

fn render_completed_collapsed(tc: &ToolCallDisplay) -> Text<'static> {
    let summary = generate_summary(tc);
    let line = Line::from(vec![
        Span::styled("▸ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            tc.name.clone(),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw("  "),
        Span::styled("✓ ", Style::default().fg(Color::Green)),
        Span::styled(summary, Style::default().fg(Color::DarkGray)),
    ]);
    Text::from(line)
}

fn render_completed_expanded(tc: &ToolCallDisplay) -> Text<'static> {
    let summary = generate_summary(tc);
    let mut lines = vec![Line::from(vec![
        Span::styled("▾ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            tc.name.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("✓ ", Style::default().fg(Color::Green)),
        Span::styled(summary, Style::default().fg(Color::DarkGray)),
    ])];

    // Add indented result content (up to first 20 lines to avoid overflow)
    if let Some(result) = &tc.result {
        for (i, result_line) in result.lines().enumerate() {
            if i >= 20 {
                lines.push(Line::from(Span::styled(
                    "  … (truncated)",
                    Style::default().fg(Color::DarkGray),
                )));
                break;
            }
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(result_line.to_owned(), Style::default().fg(Color::Gray)),
            ]));
        }
    }

    Text::from(lines)
}

fn render_failed(tc: &ToolCallDisplay) -> Text<'static> {
    let error = tc.result.as_deref().unwrap_or("unknown error").to_owned();
    let line = Line::from(vec![
        Span::styled("▸ ", Style::default().fg(Color::Red)),
        Span::styled(
            tc.name.clone(),
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("✗ ", Style::default().fg(Color::Red)),
        Span::styled(error, Style::default().fg(Color::Red)),
    ]);
    Text::from(line)
}

/// Return a short human-readable display name for a tool.
pub fn tool_display_name(name: &str) -> &str {
    match name {
        "fs_read" => "read",
        "fs_write" => "write",
        "fs_edit" => "edit",
        "fs_scan" => "scan",
        "fs_glob" => "glob",
        "shell_exec" => "shell",
        "text_grep" | "grep_files" => "grep",
        "git_log" => "git-log",
        "git_diff" => "git-diff",
        "git_status" => "git-status",
        "git_commit" => "git-commit",
        "git_branch" => "git-branch",
        "git_checkout" => "git-checkout",
        "lsp_goto_definition" => "lsp-def",
        "lsp_find_references" => "lsp-refs",
        "lsp_diagnostics" => "lsp-diag",
        "lsp_symbols" => "lsp-sym",
        "lsp_rename" => "lsp-rename",
        "codebase_search" => "search",
        "interview" => "interview",
        "task_create" => "task-create",
        "task_list" => "task-list",
        "task_get" => "task-get",
        "task_complete" => "task-complete",
        "task_fail" => "task-fail",
        "task_ask_human" => "ask-human",
        "task_claim_paths" => "claim-paths",
        "task_request_replan" => "replan",
        _ => name,
    }
}

/// Extract a key argument from a tool call's JSON arguments for preview display.
pub fn extract_key_arg(name: &str, arguments: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(arguments).unwrap_or(serde_json::Value::Null);
    let raw = match name {
        "fs_read" | "fs_write" | "fs_edit" | "fs_scan" | "fs_glob" | "text_grep" | "grep_files" => {
            parsed["path"].as_str().unwrap_or("").to_string()
        }
        "shell_exec" => {
            let cmd = parsed["command"].as_str().unwrap_or("");
            let truncated: String = cmd.chars().take(40).collect();
            truncated
        }
        "codebase_search" => parsed["query"].as_str().unwrap_or("").to_string(),
        "task_create" => parsed["title"].as_str().unwrap_or("").to_string(),
        "task_get" | "task_complete" | "task_fail" => {
            parsed["task_id"].as_str().unwrap_or("").to_string()
        }
        _ => String::new(),
    };
    if raw.is_empty() { raw } else { format!("({})", raw) }
}

/// Generate a per-tool-type human-readable summary of the result.
pub fn generate_summary(tc: &ToolCallDisplay) -> String {
    let result = match &tc.result {
        Some(r) => r.as_str(),
        None => return "completed".to_owned(),
    };

    match tc.name.as_str() {
        "fs_read" => {
            let lines = result.lines().count();
            format!("{lines} lines")
        }
        "fs_write" | "fs_edit" => {
            let lines = result.lines().count();
            format!("wrote {lines} lines")
        }
        "text_grep" | "grep_files" => {
            let matches = result.lines().count();
            format!("{matches} matches")
        }
        "shell_exec" => {
            // Extract exit code and first line of output
            let first_line = result.lines().next().unwrap_or("(empty)");
            let first_line: String = first_line.chars().take(50).collect();
            first_line
        }
        name if name.starts_with("plan_") => {
            // Plan tools — show inline result text
            let truncated: String = result.chars().take(60).collect();
            if result.len() > 60 {
                format!("{truncated}…")
            } else {
                truncated
            }
        }
        _ => {
            // Default: truncate to 60 chars
            let truncated: String = result.chars().take(60).collect();
            if result.len() > 60 {
                format!("{truncated}…")
            } else {
                truncated
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::{ToolCallDisplay, ToolCallStatus};

    fn make_running(name: &str, args: &str) -> ToolCallDisplay {
        ToolCallDisplay::new_running("id1", name, args)
    }

    fn make_completed(name: &str, args: &str, result: &str) -> ToolCallDisplay {
        ToolCallDisplay::new_running("id1", name, args).complete(result)
    }

    fn make_failed(name: &str, error: &str) -> ToolCallDisplay {
        ToolCallDisplay::new_running("id1", name, "{}").fail(error)
    }

    #[test]
    fn test_render_running_shows_spinner() {
        let tc = make_running("shell_exec", "ls -la");
        let text = render_tool_call(&tc, 0);
        let content = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(content.contains(SPINNER_FRAMES[0]));
        assert!(content.contains("shell_exec"));
    }

    #[test]
    fn test_render_running_spinner_advances_on_tick() {
        let tc = make_running("shell_exec", "{}");
        for (i, frame) in SPINNER_FRAMES.iter().enumerate() {
            let text = render_tool_call(&tc, i);
            let content = text
                .lines
                .iter()
                .flat_map(|l| l.spans.iter())
                .map(|s| s.content.as_ref())
                .collect::<Vec<_>>()
                .join("");
            assert!(
                content.contains(frame),
                "tick {} should show frame {}",
                i,
                frame
            );
        }
    }

    #[test]
    fn test_render_completed_collapsed_shows_checkmark() {
        let tc = make_completed("fs_read", "{}", "line1\nline2\nline3");
        let text = render_tool_call(&tc, 0);
        let content = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(content.contains("▸"));
        assert!(content.contains("✓"));
        assert!(
            content.contains("3 lines"),
            "fs_read should show line count, got: {}",
            content
        );
    }

    #[test]
    fn test_render_completed_expanded_shows_result_lines() {
        let mut tc = make_completed("fs_read", "{}", "line1\nline2");
        tc.collapsed = false;
        let text = render_tool_call(&tc, 0);
        let all_content = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref().to_owned())
            .collect::<Vec<_>>()
            .join("");
        assert!(all_content.contains("▾"));
        assert!(all_content.contains("line1"));
    }

    #[test]
    fn test_render_failed_shows_x_in_red() {
        let tc = make_failed("shell_exec", "exit code 127");
        let text = render_tool_call(&tc, 0);
        let content = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("");
        assert!(content.contains("✗"));
        assert!(content.contains("exit code 127"));
        // Check red style applied
        let has_red = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.style.fg == Some(Color::Red));
        assert!(has_red, "failed tool call should have red spans");
    }

    #[test]
    fn test_generate_summary_fs_read() {
        let tc = make_completed("fs_read", "{}", "a\nb\nc");
        assert_eq!(generate_summary(&tc), "3 lines");
    }

    #[test]
    fn test_generate_summary_grep() {
        let tc = make_completed("text_grep", "{}", "match1\nmatch2");
        assert_eq!(generate_summary(&tc), "2 matches");
    }

    #[test]
    fn test_generate_summary_shell_exec() {
        let tc = make_completed("shell_exec", "{}", "hello world\nmore output");
        let summary = generate_summary(&tc);
        assert!(summary.contains("hello world"));
    }

    #[test]
    fn test_generate_summary_default_truncates() {
        let long_result = "x".repeat(100);
        let tc = make_completed("unknown_tool", "{}", &long_result);
        let summary = generate_summary(&tc);
        assert!(summary.chars().count() <= 61);
        assert!(summary.ends_with('…'));
    }

    #[test]
    fn test_spinner_frames_are_6() {
        assert_eq!(SPINNER_FRAMES.len(), 6);
    }

    #[test]
    fn test_expanded_result_truncated_at_20_lines() {
        let result = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut tc = make_completed("fs_read", "{}", &result);
        tc.collapsed = false;
        let text = render_tool_call(&tc, 0);
        // Should have header line + 20 content lines + 1 truncation line = 22 lines max
        assert!(
            text.lines.len() <= 22,
            "should truncate at 20 result lines, got {}",
            text.lines.len()
        );
        let last_content = text
            .lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .last()
            .map(|s| s.content.as_ref().to_owned())
            .unwrap_or_default();
        assert!(last_content.contains("truncated") || text.lines.len() <= 21);
    }

    #[test]
    fn test_render_running_status_variant_constructed() {
        let status = ToolCallStatus::Running;
        assert_eq!(status, ToolCallStatus::Running);
    }
}
