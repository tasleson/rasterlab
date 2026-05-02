use egui::Ui;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Saturation ────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("saturation");
    let resp = header_for_tool(
        state.tools_force_open,
        "🎨  Saturation",
        state.editing,
        EditingTool::Saturation,
    )
    .id_salt("saturation")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::Saturation)
        {
            ui.disable();
        }
        let changed = ui
            .add(egui::Slider::new(&mut state.tools.saturation, 0.0..=4.0).step_by(0.01))
            .changed();
        if changed && has_image {
            state.update_sat_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply"))
                .clicked()
            {
                state.push_saturation();
            }
            if state.tools.sat_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_sat_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_saturation();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("saturation".to_string(), !default_open);
    }
}
