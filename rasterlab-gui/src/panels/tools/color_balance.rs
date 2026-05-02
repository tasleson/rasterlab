use egui::Ui;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Color Balance ─────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("color_balance");
    let resp = header_for_tool(
        state.tools_force_open,
        "⚖  Color Balance",
        state.editing,
        EditingTool::ColorBalance,
    )
    .id_salt("color_balance")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::ColorBalance)
        {
            ui.disable();
        }
        let mut changed = false;
        let zone_labels = ["Shadows", "Midtones", "Highlights"];
        {
            ui.label("Cyan ↔ Red");
            egui::Grid::new("cb_cr_grid")
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (i, zone) in zone_labels.iter().enumerate() {
                        ui.label(*zone);
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut state.tools.cb_cyan_red[i], -1.0..=1.0)
                                    .step_by(0.01),
                            )
                            .changed();
                        ui.end_row();
                    }
                });
            ui.add_space(4.0);
            ui.label("Magenta ↔ Green");
            egui::Grid::new("cb_mg_grid")
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (i, zone) in zone_labels.iter().enumerate() {
                        ui.label(*zone);
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut state.tools.cb_magenta_green[i], -1.0..=1.0)
                                    .step_by(0.01),
                            )
                            .changed();
                        ui.end_row();
                    }
                });
            ui.add_space(4.0);
            ui.label("Yellow ↔ Blue");
            egui::Grid::new("cb_yb_grid")
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (i, zone) in zone_labels.iter().enumerate() {
                        ui.label(*zone);
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut state.tools.cb_yellow_blue[i], -1.0..=1.0)
                                    .step_by(0.01),
                            )
                            .changed();
                        ui.end_row();
                    }
                });
            ui.add_space(4.0);
        }
        if changed && has_image {
            state.update_cb_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply"))
                .clicked()
            {
                state.push_cb();
            }
            if state.tools.cb_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_cb_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_cb();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("color_balance".to_string(), !default_open);
    }
}
