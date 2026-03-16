//! Tool call display widget — collapsible tool invocation details.

use egui::{Color32, Frame, RichText, Stroke, Ui};

use crate::bridge::ToolCallDisplay;
use crate::theme::{CHARCOAL, CREAM, PANEL_BG, SIGNAL_RED, STEEL_GRAY, SUNNY_GOLD};

/// Green for completed tool calls (brand complement color).
const TOOL_SUCCESS: Color32 = Color32::from_rgb(0x4A, 0x9A, 0x58);

/// Render a collapsible tool call display.
///
/// Shows the tool name and execution status in the header.
/// Click to expand/collapse arguments and result.
pub fn render_tool_call(ui: &mut Ui, tool: &mut ToolCallDisplay) {
    let status_color = tool_status_color(tool);

    Frame::new()
        .fill(PANEL_BG)
        .inner_margin(egui::Margin {
            left: 0,
            right: 0,
            top: 0,
            bottom: 0,
        })
        .corner_radius(4)
        .stroke(Stroke::new(2.0, status_color))
        .show(ui, |ui| {
            // Header row
            ui.horizontal(|ui| {
                // Collapse toggle
                let symbol = if tool.collapsed { "▶" } else { "▼" };
                if ui
                    .button(RichText::new(symbol).size(10.0).color(STEEL_GRAY))
                    .clicked()
                {
                    tool.collapsed = !tool.collapsed;
                }

                // Tool name
                ui.label(
                    RichText::new(&tool.name)
                        .size(12.0)
                        .color(STEEL_GRAY)
                        .strong(),
                );

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let status_text = if tool.result.is_some() {
                        "done"
                    } else {
                        "running"
                    };
                    ui.label(RichText::new(status_text).size(10.0).color(status_color));
                });
            });

            // Expanded body
            if !tool.collapsed {
                ui.separator();

                // Arguments section
                if !tool.arguments.is_empty() {
                    Frame::new()
                        .fill(CHARCOAL)
                        .inner_margin(egui::Margin {
                            left: 8,
                            right: 8,
                            top: 8,
                            bottom: 8,
                        })
                        .corner_radius(2)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("args")
                                    .size(10.0)
                                    .color(STEEL_GRAY.gamma_multiply(0.7)),
                            );
                            ui.add_space(2.0);
                            ui.label(
                                RichText::new(&tool.arguments)
                                    .size(12.0)
                                    .color(CREAM.gamma_multiply(0.8))
                                    .family(egui::FontFamily::Monospace),
                            );
                        });
                }

                // Result section
                if let Some(result) = &tool.result {
                    ui.add_space(4.0);
                    Frame::new()
                        .fill(CHARCOAL)
                        .inner_margin(egui::Margin {
                            left: 8,
                            right: 8,
                            top: 8,
                            bottom: 8,
                        })
                        .corner_radius(2)
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("result")
                                    .size(10.0)
                                    .color(TOOL_SUCCESS.gamma_multiply(0.8)),
                            );
                            ui.add_space(2.0);
                            // Truncate long results for display
                            let display_result = if result.len() > 500 {
                                format!("{}…", &result[..500])
                            } else {
                                result.clone()
                            };
                            ui.label(
                                RichText::new(display_result)
                                    .size(12.0)
                                    .color(CREAM.gamma_multiply(0.7))
                                    .family(egui::FontFamily::Monospace),
                            );
                        });
                }
            }
        });

    ui.add_space(2.0);
}

/// Determine the status indicator color for a tool call.
fn tool_status_color(tool: &ToolCallDisplay) -> Color32 {
    if let Some(result) = &tool.result {
        // Error results start with "Error:" or contain "error"
        if result.starts_with("Error:") || result.starts_with("error:") {
            SIGNAL_RED
        } else {
            TOOL_SUCCESS
        }
    } else {
        // Still running
        SUNNY_GOLD
    }
}
