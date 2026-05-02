use egui::Ui;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Sharpen ──────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("sharpen");
    let resp = header_for_tool(
        state.tools_force_open,
        "◈  Sharpen",
        state.editing,
        EditingTool::Sharpen,
    )
    .id_salt("sharpen")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::Sharpen)
        {
            ui.disable();
        }
        let changed = ui
            .add(
                egui::Slider::new(&mut state.tools.sharpen_strength, 0.0..=10.0)
                    .step_by(0.05)
                    .text("Strength"),
            )
            .changed();
        if changed && has_image {
            state.update_sharpen_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Sharpen"))
                .clicked()
            {
                state.push_sharpen();
            }
            if state.tools.sharpen_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_sharpen_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_sharpen();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("sharpen".to_string(), !default_open);
    }
}
