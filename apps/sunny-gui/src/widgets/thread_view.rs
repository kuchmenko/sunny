use egui::{vec2, Button, Frame, Key, Margin, RichText, ScrollArea, Stroke, TextEdit, Ui};

use crate::app::SunnyApp;
use crate::bridge::{DisplayMessage, GuiToAgent};
use crate::theme::{CHARCOAL, CREAM, HOVER_BG, PANEL_BG, STEEL_GRAY, SUNNY_GOLD};
use crate::widgets::{message::render_message, tool_call::render_tool_call};

#[allow(dead_code)]
pub fn render_thread_view(ui: &mut Ui, app: &mut SunnyApp) {
    let input_height = 60.0;
    let available = ui.available_rect_before_wrap();
    let scroll_height = (available.height() - input_height - 12.0).max(100.0);

    ScrollArea::vertical()
        .id_salt("thread_scroll")
        .max_height(scroll_height)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add_space(8.0);

            if app.messages.is_empty() {
                ui.vertical_centered(|ui| {
                    ui.add_space(40.0);
                    ui.label(
                        RichText::new("Ask Chief Sanya anything")
                            .size(16.0)
                            .color(STEEL_GRAY),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new("Type below and press Enter to start")
                            .size(12.0)
                            .color(STEEL_GRAY.gamma_multiply(0.6)),
                    );
                });
            } else {
                for msg in &mut app.messages {
                    render_message(ui, msg);

                    for tool in &mut msg.tool_calls {
                        render_tool_call(ui, tool);
                    }
                }
            }

            if app.is_streaming
                && app
                    .messages
                    .last()
                    .map(|msg| !msg.is_streaming)
                    .unwrap_or(true)
            {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Chief Sanya is thinking...")
                            .size(12.0)
                            .color(STEEL_GRAY),
                    );
                });
            }

            ui.add_space(8.0);
        });

    ui.add_space(6.0);

    Frame::new()
        .fill(PANEL_BG)
        .inner_margin(Margin::symmetric(8, 8))
        .stroke(Stroke::new(1.0, HOVER_BG))
        .show(ui, |ui| {
            let send_enabled = !app.is_streaming && !app.current_input.trim().is_empty();
            let mut send_now = false;
            let mut input_id = None;

            ui.horizontal(|ui| {
                let input_width = (ui.available_width() - 84.0).max(120.0);
                let input_response = ui.add_sized(
                    [input_width, 36.0],
                    TextEdit::singleline(&mut app.current_input)
                        .hint_text("Message Chief Sanya...")
                        .text_color(CREAM),
                );
                input_id = Some(input_response.id);

                let enter_pressed =
                    input_response.lost_focus() && ui.input(|input| input.key_pressed(Key::Enter));

                let send_response = ui.add_enabled(
                    send_enabled,
                    Button::new(RichText::new("Send").color(CHARCOAL))
                        .fill(if send_enabled { SUNNY_GOLD } else { STEEL_GRAY })
                        .min_size(vec2(72.0, 36.0)),
                );

                if send_response.clicked() || (enter_pressed && send_enabled) {
                    send_now = true;
                }
            });

            if send_now {
                let text = app.current_input.trim().to_string();
                if !text.is_empty() {
                    app.messages.push(DisplayMessage::user(text.clone()));
                    let _ = app.tx.try_send(GuiToAgent::SendMessage(text));
                    app.current_input.clear();
                    if let Some(input_id) = input_id {
                        ui.memory_mut(|mem| mem.request_focus(input_id));
                    }
                }
            }
        });
}
