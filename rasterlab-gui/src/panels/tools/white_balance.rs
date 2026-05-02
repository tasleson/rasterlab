use egui::Ui;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── White Balance ─────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("white_balance");
    let resp = header_for_tool(
        state.tools_force_open,
        "🌡  White Balance",
        state.editing,
        EditingTool::WhiteBalance,
    )
    .id_salt("white_balance")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::WhiteBalance)
        {
            ui.disable();
        }
        let mut changed = false;
        egui::Grid::new("wb_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Temperature");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut state.tools.wb_temperature, -1.0..=1.0)
                            .step_by(0.01),
                    )
                    .changed();
                ui.end_row();
                ui.label("Tint");
                changed |= ui
                    .add(egui::Slider::new(&mut state.tools.wb_tint, -1.0..=1.0).step_by(0.01))
                    .changed();
                ui.end_row();
            });
        if changed && has_image {
            state.update_wb_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply"))
                .clicked()
            {
                state.push_wb();
            }
            if state.tools.wb_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_wb_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_wb();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("white_balance".to_string(), !default_open);
    }
}
