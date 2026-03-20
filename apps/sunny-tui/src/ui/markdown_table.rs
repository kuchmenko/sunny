//! Markdown table extraction and rendering.
//!
//! `tui-markdown` does not support tables — it discards them. This module
//! splits markdown content into table and non-table segments so that tables
//! can be rendered with box-drawing characters while the rest goes through
//! `tui-markdown` as before.

use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::Style,
    text::{Line, Span},
};

use crate::ui::theme;

// ── Types ────────────────────────────────────────────────────────────────

/// A styled text fragment within a table cell.
#[derive(Debug, Clone)]
struct StyledFragment {
    content: String,
    bold: bool,
    italic: bool,
    code: bool,
}

impl StyledFragment {
    /// Visible character width of this fragment.
    fn width(&self) -> usize {
        self.content.chars().count()
    }
}

/// A fully parsed table ready for rendering.
#[derive(Debug, Clone)]
pub struct ParsedTable {
    alignments: Vec<Alignment>,
    header: Vec<Vec<StyledFragment>>,
    rows: Vec<Vec<Vec<StyledFragment>>>,
}

/// A segment of markdown content — either regular markdown or a table.
#[derive(Debug)]
pub enum ContentSegment {
    Markdown(String),
    Table(ParsedTable),
}

// ── Extraction ───────────────────────────────────────────────────────────

/// Split markdown `content` into alternating Markdown / Table segments.
pub fn extract_tables(content: &str) -> Vec<ContentSegment> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let parser = Parser::new_ext(content, opts);

    let mut segments: Vec<ContentSegment> = Vec::new();
    // Byte offset of the end of the last consumed segment.
    let mut cursor: usize = 0;

    // Table-building state
    let mut in_table = false;
    let mut alignments: Vec<Alignment> = Vec::new();
    let mut header: Vec<Vec<StyledFragment>> = Vec::new();
    let mut rows: Vec<Vec<Vec<StyledFragment>>> = Vec::new();
    let mut current_row: Vec<Vec<StyledFragment>> = Vec::new();
    let mut current_cell: Vec<StyledFragment> = Vec::new();

    // Inline style stack
    let mut bold = false;
    let mut italic = false;
    let mut code = false;

    for (event, range) in parser.into_offset_iter() {
        match event {
            Event::Start(Tag::Table(aligns)) => {
                // Emit any preceding markdown.
                if cursor < range.start {
                    let text = &content[cursor..range.start];
                    if !text.trim().is_empty() {
                        segments.push(ContentSegment::Markdown(text.to_owned()));
                    }
                }
                in_table = true;
                alignments = aligns;
                header.clear();
                rows.clear();
            }
            Event::End(TagEnd::Table) => {
                let table = ParsedTable {
                    alignments: std::mem::take(&mut alignments),
                    header: std::mem::take(&mut header),
                    rows: std::mem::take(&mut rows),
                };
                segments.push(ContentSegment::Table(table));
                cursor = range.end;
                in_table = false;
            }

            // Row tracking
            Event::Start(Tag::TableHead) => {
                current_row.clear();
            }
            Event::End(TagEnd::TableHead) => {
                header = std::mem::take(&mut current_row);
            }
            Event::Start(Tag::TableRow) => {
                current_row.clear();
            }
            Event::End(TagEnd::TableRow) => {
                rows.push(std::mem::take(&mut current_row));
            }

            // Cell tracking
            Event::Start(Tag::TableCell) => {
                current_cell.clear();
                bold = false;
                italic = false;
                code = false;
            }
            Event::End(TagEnd::TableCell) => {
                current_row.push(std::mem::take(&mut current_cell));
            }

            // Inline styles
            Event::Start(Tag::Strong) if in_table => bold = true,
            Event::End(TagEnd::Strong) if in_table => bold = false,
            Event::Start(Tag::Emphasis) if in_table => italic = true,
            Event::End(TagEnd::Emphasis) if in_table => italic = false,
            Event::Code(text) if in_table => {
                current_cell.push(StyledFragment {
                    content: text.to_string(),
                    bold,
                    italic,
                    code: true,
                });
            }

            // Text inside a cell
            Event::Text(text) if in_table => {
                current_cell.push(StyledFragment {
                    content: text.to_string(),
                    bold,
                    italic,
                    code,
                });
            }

            // Everything else — not inside a table; just skip
            _ => {}
        }
    }

    // Trailing markdown after last table (or all content if no tables).
    if cursor < content.len() {
        let text = &content[cursor..];
        if !text.trim().is_empty() {
            segments.push(ContentSegment::Markdown(text.to_owned()));
        }
    }

    // If nothing was emitted (whitespace-only), preserve the original.
    if segments.is_empty() && !content.is_empty() {
        segments.push(ContentSegment::Markdown(content.to_owned()));
    }

    segments
}

// ── Rendering ────────────────────────────────────────────────────────────

/// Visible width of a cell's fragments.
fn cell_width(fragments: &[StyledFragment]) -> usize {
    fragments.iter().map(|f| f.width()).sum()
}

/// Render a parsed table into styled `Line`s with box-drawing borders.
pub fn render_table(table: &ParsedTable) -> Vec<Line<'static>> {
    let num_cols = table.alignments.len();
    if num_cols == 0 {
        return Vec::new();
    }

    // Compute column widths (minimum 1).
    let mut col_widths: Vec<usize> = vec![1; num_cols];
    for (i, cell) in table.header.iter().enumerate() {
        if i < num_cols {
            col_widths[i] = col_widths[i].max(cell_width(cell));
        }
    }
    for row in &table.rows {
        for (i, cell) in row.iter().enumerate() {
            if i < num_cols {
                col_widths[i] = col_widths[i].max(cell_width(cell));
            }
        }
    }

    let border = theme::table_border();
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Top border: ┌──┬──┐
    lines.push(build_horizontal_border(&col_widths, "┌", "┬", "┐", border));

    // Header row
    lines.push(build_row(
        &table.header,
        &col_widths,
        &table.alignments,
        border,
        theme::table_header(),
        num_cols,
    ));

    // Header separator: ├──┼──┤
    lines.push(build_horizontal_border(&col_widths, "├", "┼", "┤", border));

    // Body rows
    for row in &table.rows {
        lines.push(build_row(
            row,
            &col_widths,
            &table.alignments,
            border,
            theme::table_cell(),
            num_cols,
        ));
    }

    // Bottom border: └──┴──┘
    lines.push(build_horizontal_border(&col_widths, "└", "┴", "┘", border));

    lines
}

fn build_horizontal_border(
    col_widths: &[usize],
    left: &str,
    mid: &str,
    right: &str,
    style: Style,
) -> Line<'static> {
    let mut s = String::from(left);
    for (i, &w) in col_widths.iter().enumerate() {
        // +2 for 1-char padding on each side
        for _ in 0..w + 2 {
            s.push('─');
        }
        if i < col_widths.len() - 1 {
            s.push_str(mid);
        }
    }
    s.push_str(right);
    Line::from(Span::styled(s, style))
}

fn build_row(
    cells: &[Vec<StyledFragment>],
    col_widths: &[usize],
    alignments: &[Alignment],
    border_style: Style,
    default_text_style: Style,
    num_cols: usize,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled("│", border_style));

    for i in 0..num_cols {
        let w = col_widths[i];
        let alignment = alignments.get(i).copied().unwrap_or(Alignment::None);

        let cell_fragments = cells.get(i);
        let content_width: usize = cell_fragments.map_or(0, |c| cell_width(c));
        let pad_total = w.saturating_sub(content_width);

        let (pad_left, pad_right) = match alignment {
            Alignment::Center => (pad_total / 2, pad_total - pad_total / 2),
            Alignment::Right => (pad_total, 0),
            Alignment::Left | Alignment::None => (0, pad_total),
        };

        // Left padding (1 space + alignment padding)
        spans.push(Span::styled(
            format!(" {}", " ".repeat(pad_left)),
            border_style,
        ));

        // Cell content
        if let Some(fragments) = cell_fragments {
            for frag in fragments {
                let mut style = default_text_style;
                if frag.bold {
                    style = style.add_modifier(ratatui::style::Modifier::BOLD);
                }
                if frag.italic {
                    style = style.add_modifier(ratatui::style::Modifier::ITALIC);
                }
                if frag.code {
                    style = style.fg(theme::SUNNY_GOLD);
                }
                spans.push(Span::styled(frag.content.clone(), style));
            }
        }

        // Right padding (alignment padding + 1 space)
        spans.push(Span::styled(
            format!("{} ", " ".repeat(pad_right)),
            border_style,
        ));

        spans.push(Span::styled("│", border_style));
    }

    Line::from(spans)
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_no_tables() {
        let md = "# Hello\n\nSome paragraph text.\n";
        let segments = extract_tables(md);
        assert_eq!(segments.len(), 1);
        assert!(matches!(&segments[0], ContentSegment::Markdown(_)));
    }

    #[test]
    fn test_extract_single_table() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |\n";
        let segments = extract_tables(md);
        assert_eq!(segments.len(), 1);
        assert!(matches!(&segments[0], ContentSegment::Table(_)));
    }

    #[test]
    fn test_extract_mixed_content() {
        let md = "Before\n\n| A | B |\n|---|---|\n| 1 | 2 |\n\nAfter\n";
        let segments = extract_tables(md);
        assert_eq!(segments.len(), 3, "got: {segments:?}");
        assert!(matches!(&segments[0], ContentSegment::Markdown(_)));
        assert!(matches!(&segments[1], ContentSegment::Table(_)));
        assert!(matches!(&segments[2], ContentSegment::Markdown(_)));
    }

    #[test]
    fn test_extract_adjacent_tables() {
        let md = "| A |\n|---|\n| 1 |\n\n| B |\n|---|\n| 2 |\n";
        let segments = extract_tables(md);
        let table_count = segments
            .iter()
            .filter(|s| matches!(s, ContentSegment::Table(_)))
            .count();
        assert_eq!(table_count, 2);
    }

    #[test]
    fn test_render_basic_table() {
        let md = "| Name | Value |\n|------|-------|\n| foo  | bar   |\n";
        let segments = extract_tables(md);
        let table = match &segments[0] {
            ContentSegment::Table(t) => t,
            _ => panic!("expected table"),
        };
        let lines = render_table(table);
        let text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains('┌'), "missing top border");
        assert!(text.contains('┘'), "missing bottom corner");
        assert!(text.contains("Name"), "missing header text");
        assert!(text.contains("foo"), "missing cell text");
    }

    #[test]
    fn test_render_alignment() {
        let md = "| Left | Center | Right |\n|:-----|:------:|------:|\n| a    |   b    |     c |\n";
        let segments = extract_tables(md);
        let table = match &segments[0] {
            ContentSegment::Table(t) => t,
            _ => panic!("expected table"),
        };
        assert_eq!(table.alignments[0], Alignment::Left);
        assert_eq!(table.alignments[1], Alignment::Center);
        assert_eq!(table.alignments[2], Alignment::Right);

        let lines = render_table(table);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_render_header_bold() {
        let md = "| H1 |\n|----|\n| d1 |\n";
        let segments = extract_tables(md);
        let table = match &segments[0] {
            ContentSegment::Table(t) => t,
            _ => panic!("expected table"),
        };
        let lines = render_table(table);
        // Header row is line index 1 (after top border).
        let header_line = &lines[1];
        let header_span = header_line
            .spans
            .iter()
            .find(|s| s.content.contains("H1"))
            .expect("should find header text");
        assert!(
            header_span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD),
            "header should be bold"
        );
    }

    #[test]
    fn test_render_empty_cells() {
        let md = "| A | B |\n|---|---|\n|   |   |\n";
        let segments = extract_tables(md);
        let table = match &segments[0] {
            ContentSegment::Table(t) => t,
            _ => panic!("expected table"),
        };
        let lines = render_table(table);
        assert!(lines.len() >= 5, "should have border + header + sep + row + border");
    }

    #[test]
    fn test_render_inline_bold_in_cell() {
        let md = "| Col |\n|-----|\n| **strong** |\n";
        let segments = extract_tables(md);
        let table = match &segments[0] {
            ContentSegment::Table(t) => t,
            _ => panic!("expected table"),
        };
        let lines = render_table(table);
        // Body row is line index 3 (top border, header, separator, body).
        let body_line = &lines[3];
        let bold_span = body_line
            .spans
            .iter()
            .find(|s| s.content.contains("strong"))
            .expect("should find bold text");
        assert!(
            bold_span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD),
            "inline bold should be rendered bold"
        );
    }
}
