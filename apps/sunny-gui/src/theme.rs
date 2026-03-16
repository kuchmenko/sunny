#![allow(dead_code)]

//! Sunny brand theme — colors, fonts, and egui::Visuals.
//!
//! Brand palette: Charcoal + Gold + Cream with industrial warmth.
//! Typography: IBM Plex Sans (UI body), JetBrains Mono (code).

use egui::{Color32, FontData, FontDefinitions, FontFamily, Visuals};

// ── Brand colors ─────────────────────────────────────────────────────────────

/// Primary background color — deep industrial charcoal.
pub const CHARCOAL: Color32 = Color32::from_rgb(0x1F, 0x1D, 0x1A);

/// Warm off-white — primary text and content color.
pub const CREAM: Color32 = Color32::from_rgb(0xF2, 0xE6, 0xC9);

/// Sunny gold — primary accent, interactive elements.
pub const SUNNY_GOLD: Color32 = Color32::from_rgb(0xF2, 0xB2, 0x33);

/// Burnt orange — secondary warm accent.
pub const BURNT_ORANGE: Color32 = Color32::from_rgb(0xD9, 0x6A, 0x1B);

/// Signal red — error and deny states.
pub const SIGNAL_RED: Color32 = Color32::from_rgb(0xC7, 0x3A, 0x1D);

/// Steel gray — secondary text, status indicators.
pub const STEEL_GRAY: Color32 = Color32::from_rgb(0x6B, 0x65, 0x5B);

/// Dark charcoal variant — slightly lighter, for panels and message bubbles.
pub const PANEL_BG: Color32 = Color32::from_rgb(0x2A, 0x28, 0x24);

/// Highlight row / hover state.
pub const HOVER_BG: Color32 = Color32::from_rgb(0x35, 0x32, 0x2D);

// ── Font bytes ────────────────────────────────────────────────────────────────

const IBM_PLEX_SANS_REGULAR: &[u8] = include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf");

const IBM_PLEX_SANS_BOLD: &[u8] = include_bytes!("../assets/fonts/IBMPlexSans-Bold.ttf");

const JETBRAINS_MONO_REGULAR: &[u8] = include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf");

// ── Font loading ──────────────────────────────────────────────────────────────

/// Load Sunny brand fonts into the egui context.
///
/// Call this once during [`eframe::App`] creation in [`eframe::CreationContext`],
/// NOT in the update loop.
pub fn load_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();

    fonts.font_data.insert(
        "IBMPlexSans-Regular".to_owned(),
        FontData::from_static(IBM_PLEX_SANS_REGULAR).into(),
    );
    fonts.font_data.insert(
        "IBMPlexSans-Bold".to_owned(),
        FontData::from_static(IBM_PLEX_SANS_BOLD).into(),
    );
    fonts.font_data.insert(
        "JetBrainsMono-Regular".to_owned(),
        FontData::from_static(JETBRAINS_MONO_REGULAR).into(),
    );

    // Prioritize IBM Plex Sans for proportional text
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "IBMPlexSans-Regular".to_owned());

    // Use JetBrains Mono for monospace (code blocks)
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "JetBrainsMono-Regular".to_owned());

    ctx.set_fonts(fonts);
}

// ── Visuals ───────────────────────────────────────────────────────────────────

/// Build Sunny's custom dark theme using the brand palette.
///
/// Apply with `ctx.set_visuals(sunny_visuals())` during app creation.
pub fn sunny_visuals() -> Visuals {
    let mut v = Visuals::dark();

    // Window and panel backgrounds
    v.panel_fill = CHARCOAL;
    v.window_fill = PANEL_BG;
    v.extreme_bg_color = CHARCOAL;

    // Selection / highlight
    v.selection.bg_fill = SUNNY_GOLD.gamma_multiply(0.3);
    v.selection.stroke.color = SUNNY_GOLD;

    // Hyperlinks → gold
    v.hyperlink_color = SUNNY_GOLD;

    // Error text → signal red
    v.error_fg_color = SIGNAL_RED;

    // Widget styles
    v.widgets.noninteractive.bg_fill = PANEL_BG;
    v.widgets.noninteractive.fg_stroke.color = STEEL_GRAY;

    v.widgets.inactive.bg_fill = HOVER_BG;
    v.widgets.inactive.fg_stroke.color = CREAM;

    v.widgets.hovered.bg_fill = HOVER_BG;
    v.widgets.hovered.fg_stroke.color = CREAM;
    v.widgets.hovered.bg_stroke.color = SUNNY_GOLD;

    v.widgets.active.bg_fill = SUNNY_GOLD.gamma_multiply(0.2);
    v.widgets.active.fg_stroke.color = SUNNY_GOLD;
    v.widgets.active.bg_stroke.color = SUNNY_GOLD;

    // Faint bg (for code blocks, text edit backgrounds)
    v.faint_bg_color = PANEL_BG;

    v
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_charcoal_matches_brand_spec() {
        assert_eq!(CHARCOAL.r(), 0x1F, "CHARCOAL red channel mismatch");
        assert_eq!(CHARCOAL.g(), 0x1D, "CHARCOAL green channel mismatch");
        assert_eq!(CHARCOAL.b(), 0x1A, "CHARCOAL blue channel mismatch");
    }

    #[test]
    fn test_sunny_gold_matches_brand_spec() {
        assert_eq!(SUNNY_GOLD.r(), 0xF2, "SUNNY_GOLD red channel mismatch");
        assert_eq!(SUNNY_GOLD.g(), 0xB2, "SUNNY_GOLD green channel mismatch");
        assert_eq!(SUNNY_GOLD.b(), 0x33, "SUNNY_GOLD blue channel mismatch");
    }

    #[test]
    fn test_cream_matches_brand_spec() {
        assert_eq!(CREAM.r(), 0xF2, "CREAM red channel mismatch");
        assert_eq!(CREAM.g(), 0xE6, "CREAM green channel mismatch");
        assert_eq!(CREAM.b(), 0xC9, "CREAM blue channel mismatch");
    }

    #[test]
    fn test_burnt_orange_matches_brand_spec() {
        assert_eq!(BURNT_ORANGE.r(), 0xD9, "BURNT_ORANGE red channel mismatch");
        assert_eq!(
            BURNT_ORANGE.g(),
            0x6A,
            "BURNT_ORANGE green channel mismatch"
        );
        assert_eq!(BURNT_ORANGE.b(), 0x1B, "BURNT_ORANGE blue channel mismatch");
    }

    #[test]
    fn test_signal_red_matches_brand_spec() {
        assert_eq!(SIGNAL_RED.r(), 0xC7, "SIGNAL_RED red channel mismatch");
        assert_eq!(SIGNAL_RED.g(), 0x3A, "SIGNAL_RED green channel mismatch");
        assert_eq!(SIGNAL_RED.b(), 0x1D, "SIGNAL_RED blue channel mismatch");
    }

    #[test]
    fn test_steel_gray_matches_brand_spec() {
        assert_eq!(STEEL_GRAY.r(), 0x6B, "STEEL_GRAY red channel mismatch");
        assert_eq!(STEEL_GRAY.g(), 0x65, "STEEL_GRAY green channel mismatch");
        assert_eq!(STEEL_GRAY.b(), 0x5B, "STEEL_GRAY blue channel mismatch");
    }

    #[test]
    fn test_sunny_visuals_panel_fill_is_charcoal() {
        let v = sunny_visuals();
        assert_eq!(v.panel_fill, CHARCOAL, "panel_fill must be CHARCOAL");
    }

    #[test]
    fn test_sunny_visuals_hyperlink_is_gold() {
        let v = sunny_visuals();
        assert_eq!(
            v.hyperlink_color, SUNNY_GOLD,
            "hyperlinks must use SUNNY_GOLD"
        );
    }
}
