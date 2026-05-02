use egui::Ui;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Brightness / Contrast ─────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("brightness_contrast");
    let resp = header_for_tool(
        state.tools_force_open,
        "☀  Brightness / Contrast",
        state.editing,
        EditingTool::BrightnessContrast,
    )
    .id_salt("brightness_contrast")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::BrightnessContrast)
        {
            ui.disable();
        }
        let mut changed = false;
        egui::Grid::new("bc_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Brightness");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut state.tools.bc_brightness, -1.0..=1.0).step_by(0.01),
                    )
                    .changed();
                ui.end_row();
                ui.label("Contrast");
                changed |= ui
                    .add(egui::Slider::new(&mut state.tools.bc_contrast, -1.0..=1.0).step_by(0.01))
                    .changed();
                ui.end_row();
            });
        if changed && has_image {
            state.update_bc_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply"))
                .clicked()
            {
                state.push_bc();
            }
            if state.tools.bc_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_bc_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_bc();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("brightness_contrast".to_string(), !default_open);
    }
}
