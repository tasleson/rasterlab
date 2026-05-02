use egui::Ui;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Sepia ─────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("sepia");
    let resp = header_for_tool(
        state.tools_force_open,
        "🟫  Sepia",
        state.editing,
        EditingTool::Sepia,
    )
    .id_salt("sepia")
    .default_open(default_open)
    .show(ui, |ui| {
        if state.editing.is_some_and(|s| s.tool != EditingTool::Sepia) {
            ui.disable();
        }
        let changed = ui
            .add(egui::Slider::new(&mut state.tools.sepia_strength, 0.0..=1.0).step_by(0.01))
            .changed();
        if changed && has_image {
            state.update_sepia_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Sepia"))
                .clicked()
            {
                state.push_sepia();
            }
            if state.tools.sepia_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_sepia_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_sepia();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("sepia".to_string(), !default_open);
    }
}
