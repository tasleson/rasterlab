use egui::Ui;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Hue Shift ─────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("hue_shift");
    let resp = header_for_tool(
        state.tools_force_open,
        "🎡  Hue Shift",
        state.editing,
        EditingTool::HueShift,
    )
    .id_salt("hue_shift")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::HueShift)
        {
            ui.disable();
        }
        let changed = ui
            .add(
                egui::Slider::new(&mut state.tools.hue_degrees, -180.0..=180.0)
                    .text("Degrees")
                    .step_by(1.0),
            )
            .changed();
        if changed && has_image {
            state.update_hue_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply"))
                .clicked()
            {
                state.push_hue();
            }
            if state.tools.hue_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_hue_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_hue();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("hue_shift".to_string(), !default_open);
    }
}
