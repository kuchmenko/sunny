//! Signal Margin — full-width plasma glow.
//!
//! Three sine waves with golden-ratio frequency ratios (φ ≈ 1.618) create a
//! quasi-periodic interference pattern: the waves never phase-align, so the
//! pattern never exactly repeats. Squaring the combined output produces bright
//! plasma peaks against wide dark gaps — structured forms rather than uniform
//! noise.
//!
//! The glow renders across the full terminal width. Both edges peak at max
//! amplitude; intensity falls to zero at the horizontal midpoint. Content
//! widgets that set explicit CHARCOAL backgrounds blend seamlessly (gradient
//! also converges to CHARCOAL at center). Widgets without explicit backgrounds
//! (block borders, hint bar, empty rows) reveal the warm glow beneath.

use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::ui::theme;

/// Golden ratio. Using φ as a frequency multiplier between the three waves
/// makes their relationships irrational — the pattern is quasi-periodic and
/// never tiles, unlike harmonics which repeat.
const PHI: f32 = 1.618_033_988_5;

/// Render full-width plasma glow. Only active when margins exist (≥ 6 cols).
///
/// `flash_t`: 1.0 = just dispatched, 0.0 = no flash.
pub fn render_margins(
    frame: &mut Frame,
    full_area: Rect,
    content_area: Rect,
    tick_count: usize,
    is_streaming: bool,
    is_focused: bool,
    flash_t: f32,
) {
    let left_w = content_area.x.saturating_sub(full_area.x);
    if left_w < 6 {
        return;
    }

    let base_amp: f32 = if is_streaming {
        0.55
    } else if is_focused {
        0.22
    } else {
        0.15
    };
    let amplitude = (base_amp + flash_t * flash_t * 0.45_f32).min(0.75);

    // t = 0 at each terminal edge (max glow), t = 1 at center (zero).
    let half = full_area.width as f32 / 2.0;

    for row in 0..full_area.height {
        let spans: Vec<Span> = (0..full_area.width)
            .map(|col| {
                let t_l = (col as f32 / half).min(1.0);
                let t_r = ((full_area.width - 1 - col) as f32 / half).min(1.0);
                // Quadratic envelope: bright at edges, zero at center.
                let h_env = ((1.0 - t_l).powi(2)).max((1.0 - t_r).powi(2));

                let color = if h_env < 0.005 {
                    // Center cells: skip plasma math, use exact CHARCOAL.
                    theme::CHARCOAL
                } else {
                    let p = plasma_t(col, row, tick_count, is_streaming);
                    let intensity = (amplitude * h_env * p).clamp(0.0, 1.0);
                    plasma_color(intensity)
                };

                Span::styled(" ", Style::default().bg(color))
            })
            .collect();

        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect::new(full_area.x, full_area.y + row, full_area.width, 1),
        );
    }
}

/// Three-wave plasma with golden-ratio frequency spacing.
///
/// Returns 0.0..=1.0. The squared output concentrates energy into sharp
/// bright peaks with wide dark gaps — "embers" rather than smooth bands.
///
/// Wave anatomy:
///   w1 — primary: wide vertical columns (~45 col period), upward travel,
///        slow rightward drift of the column pattern.
///   w2 — φ harmonic: narrower columns (~28 col period), opposite diagonal
///        lean, φ-scaled vertical and temporal speed.
///   w3 — broad undulation (~90 col period): shapes the large-scale envelope,
///        half speed, no drift.
fn plasma_t(col: u16, row: u16, tick: usize, is_streaming: bool) -> f32 {
    let c = col as f32;
    let r = row as f32;
    let t = tick as f32;
    let speed = if is_streaming { 0.070 } else { 0.020 };

    let w1 = (c * 0.140 + t * 0.004 + r * 0.300 - t * speed).sin();
    let w2 = (c * 0.140 * PHI - t * 0.003 + r * 0.300 * PHI - t * speed * PHI).sin();
    let w3 = (c * 0.070 + r * 0.180 - t * speed * 0.50).sin();

    // Weights sum to 1.0 → combined stays ≈ [-1, 1].
    let combined = w1 * 0.50 + w2 * 0.32 + w3 * 0.18;

    // Normalize then square: turns sine undulation into separated bright peaks.
    ((combined + 1.0) / 2.0).powi(2)
}

/// Fire-like 3-stop ramp: CHARCOAL → BURNT_ORANGE → SUNNY_GOLD.
/// Dark base transitions through warm orange before reaching gold peaks.
fn plasma_color(intensity: f32) -> Color {
    if intensity < 0.5 {
        theme::lerp(theme::CHARCOAL, theme::BURNT_ORANGE, intensity * 2.0)
    } else {
        theme::lerp(theme::BURNT_ORANGE, theme::SUNNY_GOLD, (intensity - 0.5) * 2.0)
    }
}
