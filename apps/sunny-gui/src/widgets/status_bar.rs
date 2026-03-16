//! Status bar widget — bottom panel with token budget and shift status.

use egui::RichText;

use crate::app::SunnyApp;
use crate::theme::{SIGNAL_RED, STEEL_GRAY, SUNNY_GOLD};

/// Render the status bar in the bottom panel.
///
/// Shows shift status (streaming vs idle), session context, and token budget.
pub fn render_status_bar(ui: &mut egui::Ui, app: &SunnyApp) {
    ui.horizontal(|ui| {
        ui.set_min_height(24.0);

        // Left: shift status
        let (status_text, status_color) = if app.is_streaming {
            ("● Shift active", SUNNY_GOLD)
        } else {
            ("○ Crew idle", STEEL_GRAY)
        };
        ui.label(RichText::new(status_text).size(11.0).color(status_color));

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(12.0);

        // Center: session ID (abbreviated)
        if let Some(ref session_id) = app.current_session_id {
            let short_id = if session_id.len() > 8 {
                &session_id[..8]
            } else {
                session_id
            };
            ui.label(
                RichText::new(format!("session: {short_id}…"))
                    .size(11.0)
                    .color(STEEL_GRAY.gamma_multiply(0.7)),
            );
        }

        // Right-aligned: token budget
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let used_k = app.token_used / 1000;
            let total_k = app.token_total / 1000;

            let token_color = if app.token_used > app.token_total * 8 / 10 {
                SIGNAL_RED
            } else {
                STEEL_GRAY
            };

            ui.label(
                RichText::new(format!("{used_k}K / {total_k}K tokens"))
                    .size(11.0)
                    .color(token_color),
            );
        });
    });
}
