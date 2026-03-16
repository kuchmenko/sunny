//! Session sidebar widget — session list, switch, and new session button.

use egui::{Frame, RichText, Stroke, Ui};

use crate::app::SunnyApp;
use crate::bridge::GuiToAgent;
use crate::theme::{CHARCOAL, CREAM, HOVER_BG, PANEL_BG, STEEL_GRAY, SUNNY_GOLD};

/// Render the session sidebar panel content.
///
/// Shows a "New Session" button at the top, followed by a scrollable list
/// of all saved sessions. The current session is highlighted in gold.
/// Clicking a session sends a `SwitchSession` command to the bridge.
pub fn render_session_sidebar(ui: &mut Ui, app: &mut SunnyApp) {
    ui.add_space(8.0);

    // New Session button
    let new_btn = ui.add_sized(
        [ui.available_width(), 28.0],
        egui::Button::new(RichText::new("+ New Session").size(13.0).color(CHARCOAL))
            .fill(SUNNY_GOLD)
            .stroke(Stroke::NONE),
    );
    if new_btn.clicked() {
        let _ = app.tx.try_send(GuiToAgent::NewSession);
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);

    // Session list
    if app.sessions.is_empty() {
        ui.vertical_centered(|ui| {
            ui.add_space(20.0);
            ui.label(RichText::new("No sessions").size(12.0).color(STEEL_GRAY));
        });
        return;
    }

    let sessions = app.sessions.clone();
    let current_id = app.current_session_id.clone();

    egui::ScrollArea::vertical()
        .id_salt("session_sidebar_scroll")
        .show(ui, |ui| {
            for session in &sessions {
                let is_current = current_id
                    .as_deref()
                    .map(|id| id == session.id)
                    .unwrap_or(false);

                let bg = if is_current { HOVER_BG } else { PANEL_BG };
                let title_color = if is_current { SUNNY_GOLD } else { CREAM };

                let row_response = Frame::NONE
                    .fill(bg)
                    .inner_margin(egui::Margin::symmetric(8, 6))
                    .corner_radius(egui::CornerRadius::same(4))
                    .stroke(if is_current {
                        Stroke::new(1.0, SUNNY_GOLD.linear_multiply(0.5))
                    } else {
                        Stroke::NONE
                    })
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());

                        // Session title
                        let title = session.title.as_deref().unwrap_or("Untitled");
                        ui.label(RichText::new(title).size(13.0).color(title_color));

                        ui.add_space(2.0);

                        // Metadata row
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(format!("{} tokens", session.message_count))
                                    .size(10.0)
                                    .color(STEEL_GRAY),
                            );
                            ui.add_space(4.0);
                            let date_str =
                                session.updated_at.get(..10).unwrap_or(&session.updated_at);
                            ui.label(
                                RichText::new(date_str)
                                    .size(10.0)
                                    .color(STEEL_GRAY.linear_multiply(0.7)),
                            );
                        });
                    });

                if row_response
                    .response
                    .interact(egui::Sense::click())
                    .clicked()
                {
                    let _ = app
                        .tx
                        .try_send(GuiToAgent::SwitchSession(session.id.clone()));
                }

                ui.add_space(2.0);
            }
        });
}
