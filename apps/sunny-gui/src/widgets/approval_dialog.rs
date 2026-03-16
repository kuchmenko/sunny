//! Approval dialog modal — shown when the agent requests capability approval.

use egui::{Align2, RichText};

use crate::app::SunnyApp;
use crate::theme::{CHARCOAL, CREAM, PANEL_BG, SIGNAL_RED, STEEL_GRAY, SUNNY_GOLD};

/// Render the approval request modal overlay.
///
/// Shows a centered modal window when `app.pending_approval` is set.
/// "Approve" sends `true`, "Deny" sends `false` via the pending approvals channel.
/// Clears `pending_approval` after the user responds.
pub fn render_approval_dialog(ctx: &egui::Context, app: &mut SunnyApp) {
    let Some(ref request) = app.pending_approval else {
        return;
    };

    // Clone data we need before the mutable borrow
    let req_id = request.id.clone();
    let tool_name = request.tool.clone();
    let command = request.command.clone();
    let reason = request.reason.clone();

    let mut approved: Option<bool> = None;

    egui::Window::new("Permission Required")
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
        .min_width(400.0)
        .max_width(600.0)
        .frame(egui::Frame::window(&ctx.style()).fill(PANEL_BG))
        .show(ctx, |ui| {
            ui.add_space(8.0);

            ui.horizontal(|ui| {
                ui.label(RichText::new("⚠").size(20.0).color(SUNNY_GOLD));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!("Tool: {tool_name}"))
                        .size(14.0)
                        .color(CREAM)
                        .strong(),
                );
            });

            ui.add_space(8.0);

            // Command
            egui::Frame::new()
                .fill(CHARCOAL)
                .inner_margin(egui::Margin::same(8))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(&command)
                            .size(12.0)
                            .color(CREAM.gamma_multiply(0.9))
                            .family(egui::FontFamily::Monospace),
                    );
                });

            ui.add_space(8.0);

            // Reason
            ui.label(RichText::new("Reason:").size(11.0).color(STEEL_GRAY));
            ui.label(
                RichText::new(&reason)
                    .size(12.0)
                    .color(CREAM.gamma_multiply(0.8)),
            );

            ui.add_space(16.0);

            // Buttons
            ui.horizontal(|ui| {
                let approve_btn = ui.add_sized(
                    [120.0, 32.0],
                    egui::Button::new(RichText::new("Approve").color(CHARCOAL)).fill(SUNNY_GOLD),
                );
                if approve_btn.clicked() {
                    approved = Some(true);
                }

                ui.add_space(12.0);

                let deny_btn = ui.add_sized(
                    [120.0, 32.0],
                    egui::Button::new(RichText::new("Deny").color(CREAM)).fill(SIGNAL_RED),
                );
                if deny_btn.clicked() {
                    approved = Some(false);
                }
            });

            ui.add_space(8.0);
        });

    // Send response if user clicked a button
    if let Some(decision) = approved {
        // Use a blocking thread to send the response via the pending approvals map
        let pending = app.pending_approvals.clone();
        let id_clone = req_id.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build runtime");
            rt.block_on(async move {
                let mut pending = pending.lock().await;
                if let Some(tx) = pending.remove(&id_clone) {
                    let _ = tx.send(decision);
                }
            });
        });
        // Clear the pending approval
        app.pending_approval = None;
    }
}
