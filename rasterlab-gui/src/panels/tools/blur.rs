use egui::{DragValue, Ui};

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Blur ──────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("blur");
    let resp = header_for_tool(
        state.tools_force_open,
        "≋  Blur",
        state.editing,
        EditingTool::Blur,
    )
    .id_salt("blur")
    .default_open(default_open)
    .show(ui, |ui| {
        if state.editing.is_some_and(|s| s.tool != EditingTool::Blur) {
            ui.disable();
        }
        let changed = ui
            .horizontal(|ui| {
                ui.label("Radius (σ):");
                ui.add(
                    DragValue::new(&mut state.tools.blur_radius)
                        .speed(0.1)
                        .range(0.1..=100.0_f32)
                        .suffix(" px"),
                )
                .changed()
            })
            .inner;
        if changed && has_image {
            state.update_blur_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Blur"))
                .clicked()
            {
                state.push_blur();
            }
            if state.tools.blur_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_blur_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_blur();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("blur".to_string(), !default_open);
    }
}
