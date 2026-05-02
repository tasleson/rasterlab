use egui::{DragValue, Ui};

use super::shared::header;
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Perspective ───────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("perspective");
    let resp = header(state.tools_force_open, "⬡  Perspective")
        .id_salt("perspective")
        .default_open(default_open)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }

            let mut changed = false;

            // Vertical keystone slider.
            changed |= ui
                .add(
                    egui::Slider::new(&mut state.tools.perspective_vertical, -100.0..=100.0)
                        .text("Vertical")
                        .step_by(0.5),
                )
                .changed();

            // Horizontal keystone slider.
            changed |= ui
                .add(
                    egui::Slider::new(&mut state.tools.perspective_horizontal, -100.0..=100.0)
                        .text("Horizontal")
                        .step_by(0.5),
                )
                .changed();

            // Scale slider — zoom in to hide empty border areas.
            changed |= ui
                .add(
                    egui::Slider::new(&mut state.tools.perspective_scale, 100.0..=150.0)
                        .text("Scale")
                        .step_by(0.1)
                        .suffix("%"),
                )
                .changed();

            if changed && has_image {
                state.update_perspective_preview();
            }

            // Grid spacing controls.
            ui.horizontal(|ui| {
                ui.label("Grid");
                ui.add(
                    DragValue::new(&mut state.tools.perspective_grid_cols)
                        .speed(0.1)
                        .range(1..=24_u32)
                        .prefix("cols: "),
                );
                ui.add(
                    DragValue::new(&mut state.tools.perspective_grid_rows)
                        .speed(0.1)
                        .range(1..=24_u32)
                        .prefix("rows: "),
                );
            });

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_perspective();
                }
                if state.tools.perspective_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_perspective_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_perspective();
                }
            });
            ui.checkbox(
                &mut state.tools.perspective_crop,
                "Crop to rectangle after apply",
            );

            ui.label(
                egui::RichText::new("Use the grid to align straight lines in the image.")
                    .small()
                    .color(egui::Color32::from_gray(140)),
            );
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("perspective".to_string(), !default_open);
    }
}
