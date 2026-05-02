use egui::{DragValue, Ui};

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Denoise ───────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("denoise");
    let resp = header_for_tool(
        state.tools_force_open,
        "◌  Denoise",
        state.editing,
        EditingTool::Denoise,
    )
    .id_salt("denoise")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::Denoise)
        {
            ui.disable();
        }
        let mut changed = false;
        egui::Grid::new("denoise_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Strength:");
                changed |= ui
                    .add(
                        DragValue::new(&mut state.tools.denoise_strength)
                            .speed(0.01)
                            .range(0.01..=1.0_f32),
                    )
                    .changed();
                ui.end_row();
                ui.label("Radius:");
                changed |= ui
                    .add(
                        DragValue::new(&mut state.tools.denoise_radius)
                            .speed(1)
                            .range(1..=10_u32)
                            .suffix(" px"),
                    )
                    .changed();
                ui.end_row();
            });
        if changed && has_image {
            state.update_denoise_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Denoise"))
                .clicked()
            {
                state.push_denoise();
            }
            if state.tools.denoise_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_denoise_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_denoise();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("denoise".to_string(), !default_open);
    }
}
