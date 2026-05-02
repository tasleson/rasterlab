use egui::{DragValue, Ui};

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Vignette ──────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("vignette");
    let resp = header_for_tool(
        state.tools_force_open,
        "◎  Vignette",
        state.editing,
        EditingTool::Vignette,
    )
    .id_salt("vignette")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::Vignette)
        {
            ui.disable();
        }
        let mut changed = false;
        egui::Grid::new("vignette_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Strength");
                changed |= ui
                    .add(
                        DragValue::new(&mut state.tools.vignette_strength)
                            .speed(0.01)
                            .range(0.0..=1.0),
                    )
                    .changed();
                ui.end_row();
                ui.label("Radius");
                changed |= ui
                    .add(
                        DragValue::new(&mut state.tools.vignette_radius)
                            .speed(0.01)
                            .range(0.0..=1.0),
                    )
                    .changed();
                ui.end_row();
                ui.label("Feather");
                changed |= ui
                    .add(
                        DragValue::new(&mut state.tools.vignette_feather)
                            .speed(0.01)
                            .range(0.0..=1.0),
                    )
                    .changed();
                ui.end_row();
            });
        if changed && has_image {
            state.update_vignette_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Vignette"))
                .clicked()
            {
                state.push_vignette();
            }
            if state.tools.vignette_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_vignette_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_vignette();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("vignette".to_string(), !default_open);
    }
}
