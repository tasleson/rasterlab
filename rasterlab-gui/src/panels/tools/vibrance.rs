use egui::Ui;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Vibrance ──────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("vibrance");
    let resp = header_for_tool(
        state.tools_force_open,
        "✦  Vibrance",
        state.editing,
        EditingTool::Vibrance,
    )
    .id_salt("vibrance")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::Vibrance)
        {
            ui.disable();
        }
        let changed = ui
            .add(egui::Slider::new(&mut state.tools.vibrance, -1.0..=1.0).step_by(0.01))
            .changed();
        if changed && has_image {
            state.update_vibrance_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply"))
                .clicked()
            {
                state.push_vibrance();
            }
            if state.tools.vibrance_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_vibrance_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_vibrance();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("vibrance".to_string(), !default_open);
    }
}
