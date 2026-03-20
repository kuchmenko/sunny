//! Brand color palette for sunny-tui.
//!
//! Colors from .sisyphus/brand-kit.md mapped to terminal RGB.
//! Every widget pulls from here — no hardcoded colors elsewhere.

use ratatui::style::{Color, Modifier, Style};

// ── Brand palette ──────────────────────────────────────────────────────────

pub const CHARCOAL: Color = Color::Rgb(31, 29, 26);
pub const CREAM: Color = Color::Rgb(242, 230, 201);
pub const SUNNY_GOLD: Color = Color::Rgb(242, 178, 51);
pub const BURNT_ORANGE: Color = Color::Rgb(217, 106, 27);
pub const SIGNAL_RED: Color = Color::Rgb(199, 58, 29);
pub const STEEL_GRAY: Color = Color::Rgb(107, 101, 91);

// ── Semantic mappings ──────────────────────────────────────────────────────

pub const BORDER_ACTIVE: Color = SUNNY_GOLD;
pub const BORDER_INACTIVE: Color = STEEL_GRAY;
pub const TEXT_PRIMARY: Color = CREAM;
pub const TEXT_MUTED: Color = STEEL_GRAY;
pub const TEXT_ACCENT: Color = SUNNY_GOLD;
pub const ERROR: Color = SIGNAL_RED;
pub const SUCCESS: Color = Color::Rgb(106, 153, 85);

// ── Message & tool semantic colors ───────────────────────────────────────

pub const USER_ACCENT: Color = BURNT_ORANGE;
pub const TOOL_RUNNING: Color = SUNNY_GOLD;
pub const TOOL_DONE: Color = STEEL_GRAY;
pub const TOOL_SUCCESS: Color = SUCCESS;
pub const TOOL_FAIL: Color = SIGNAL_RED;
pub const STREAMING_CURSOR: Color = SUNNY_GOLD;

// ── Pre-built styles ───────────────────────────────────────────────────────

pub fn border_focused() -> Style {
    Style::default().fg(BORDER_ACTIVE)
}

pub fn border_unfocused() -> Style {
    Style::default().fg(BORDER_INACTIVE)
}

pub fn muted() -> Style {
    Style::default().fg(TEXT_MUTED)
}

pub fn accent() -> Style {
    Style::default()
        .fg(TEXT_ACCENT)
        .add_modifier(Modifier::BOLD)
}

pub fn hint() -> Style {
    Style::default()
        .fg(STEEL_GRAY)
        .add_modifier(Modifier::DIM)
}

// ── Message style functions ──────────────────────────────────────────────

pub fn user_label() -> Style {
    Style::default()
        .fg(USER_ACCENT)
        .add_modifier(Modifier::BOLD)
}

pub fn assistant_label() -> Style {
    Style::default()
        .fg(SUNNY_GOLD)
        .add_modifier(Modifier::BOLD)
}

pub fn system_label() -> Style {
    Style::default()
        .fg(STEEL_GRAY)
        .add_modifier(Modifier::BOLD)
}

pub fn thinking() -> Style {
    Style::default()
        .fg(STEEL_GRAY)
        .add_modifier(Modifier::ITALIC)
}

pub fn tool_running_name() -> Style {
    Style::default()
        .fg(TOOL_RUNNING)
        .add_modifier(Modifier::BOLD)
}

pub fn tool_done_name() -> Style {
    Style::default()
        .fg(TOOL_DONE)
        .add_modifier(Modifier::DIM)
}

pub fn tool_fail_name() -> Style {
    Style::default()
        .fg(TOOL_FAIL)
        .add_modifier(Modifier::BOLD)
}

pub fn separator_line() -> Style {
    Style::default().fg(STEEL_GRAY).add_modifier(Modifier::DIM)
}

pub fn timestamp() -> Style {
    Style::default().fg(STEEL_GRAY)
}

pub fn streaming_cursor() -> Style {
    Style::default().fg(STREAMING_CURSOR)
}

// ── Animation helpers ─────────────────────────────────────────────────────

/// Lerp between two Color::Rgb values. Non-RGB colors snap at t=0.5.
pub fn lerp(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => Color::Rgb(
            (r1 as f32 + (r2 as f32 - r1 as f32) * t).round() as u8,
            (g1 as f32 + (g2 as f32 - g1 as f32) * t).round() as u8,
            (b1 as f32 + (b2 as f32 - b1 as f32) * t).round() as u8,
        ),
        _ => if t < 0.5 { a } else { b },
    }
}

// ── Table styles ─────────────────────────────────────────────────────

pub fn table_border() -> Style {
    Style::default().fg(STEEL_GRAY).add_modifier(Modifier::DIM)
}

pub fn table_header() -> Style {
    Style::default().fg(CREAM).add_modifier(Modifier::BOLD)
}

pub fn table_cell() -> Style {
    Style::default().fg(CREAM)
}
