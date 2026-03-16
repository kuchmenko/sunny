use std::cell::RefCell;

use egui::{CornerRadius, FontFamily, Frame, Margin, RichText, Stroke, Ui};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};

use crate::bridge::{DisplayMessage, DisplayRole};
use crate::theme::{
    BURNT_ORANGE, CHARCOAL, CREAM, HOVER_BG, PANEL_BG, SIGNAL_RED, STEEL_GRAY, SUNNY_GOLD,
};

thread_local! {
    static COMMONMARK_CACHE: RefCell<CommonMarkCache> = RefCell::new(CommonMarkCache::default());
}

pub fn render_message(ui: &mut Ui, msg: &DisplayMessage) {
    match msg.role {
        DisplayRole::User => render_user_message(ui, msg),
        DisplayRole::Assistant => render_assistant_message(ui, msg),
        DisplayRole::System => render_system_message(ui, msg),
        DisplayRole::Tool => render_tool_result_message(ui, msg),
    }
}

fn render_user_message(ui: &mut Ui, msg: &DisplayMessage) {
    ui.add_space(4.0);

    Frame::new()
        .fill(HOVER_BG)
        .inner_margin(Margin::symmetric(12, 8))
        .corner_radius(CornerRadius::same(6))
        .stroke(Stroke::new(1.0, STEEL_GRAY.gamma_multiply(0.4)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("You").size(11.0).color(STEEL_GRAY).strong());
                ui.add_space(6.0);
                if let Some(ts) = &msg.timestamp {
                    ui.label(
                        RichText::new(ts.format("%H:%M").to_string())
                            .size(10.0)
                            .color(STEEL_GRAY.gamma_multiply(0.6)),
                    );
                }
            });
            ui.add_space(4.0);
            ui.label(RichText::new(&msg.content).size(14.0).color(CREAM));
        });

    ui.add_space(4.0);
}

fn render_assistant_message(ui: &mut Ui, msg: &DisplayMessage) {
    ui.add_space(4.0);

    Frame::new()
        .fill(PANEL_BG)
        .inner_margin(Margin::symmetric(12, 8))
        .corner_radius(CornerRadius::same(6))
        .stroke(Stroke::new(1.5, SUNNY_GOLD.gamma_multiply(0.3)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Chief Sanya")
                        .size(11.0)
                        .color(SUNNY_GOLD)
                        .strong(),
                );
                ui.add_space(6.0);
                if let Some(ts) = &msg.timestamp {
                    ui.label(
                        RichText::new(ts.format("%H:%M").to_string())
                            .size(10.0)
                            .color(STEEL_GRAY.gamma_multiply(0.6)),
                    );
                }
            });
            ui.add_space(4.0);

            if msg.content.is_empty() && msg.is_streaming {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Thinking...")
                            .size(12.0)
                            .color(SUNNY_GOLD.gamma_multiply(0.85))
                            .italics(),
                    );
                });
            } else {
                if msg.is_streaming {
                    ui.label(RichText::new(&msg.content).size(14.0).color(CREAM));
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("live")
                                .size(11.0)
                                .color(SIGNAL_RED.gamma_multiply(0.9))
                                .strong(),
                        );
                        ui.label(
                            RichText::new("▌")
                                .size(14.0)
                                .color(SUNNY_GOLD.gamma_multiply(0.8)),
                        );
                    });
                } else {
                    ui.scope(|ui| {
                        ui.visuals_mut().override_text_color = Some(CREAM);
                        COMMONMARK_CACHE.with(|cache| {
                            let mut cache = cache.borrow_mut();
                            CommonMarkViewer::new().show(ui, &mut cache, &msg.content);
                        });
                    });
                }
            }
        });

    ui.add_space(4.0);
}

fn render_system_message(ui: &mut Ui, msg: &DisplayMessage) {
    ui.add_space(2.0);
    ui.vertical_centered(|ui| {
        ui.label(
            RichText::new(&msg.content)
                .size(12.0)
                .color(STEEL_GRAY)
                .italics(),
        );
    });
    ui.add_space(2.0);
}

fn render_tool_result_message(ui: &mut Ui, msg: &DisplayMessage) {
    ui.add_space(2.0);

    Frame::new()
        .fill(CHARCOAL)
        .inner_margin(Margin::symmetric(8, 4))
        .corner_radius(CornerRadius::same(4))
        .stroke(Stroke::new(1.0, BURNT_ORANGE.gamma_multiply(0.35)))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("Tool")
                        .size(10.0)
                        .color(BURNT_ORANGE)
                        .strong(),
                );
            });
            ui.add_space(2.0);
            ui.label(
                RichText::new(&msg.content)
                    .size(12.0)
                    .color(STEEL_GRAY)
                    .family(FontFamily::Monospace),
            );
        });

    ui.add_space(2.0);
}
