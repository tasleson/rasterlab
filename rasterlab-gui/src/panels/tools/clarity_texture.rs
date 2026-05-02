use egui::Ui;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Clarity / Texture ─────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("clarity_texture");
    let resp = header_for_tool(
        state.tools_force_open,
        "◈  Clarity / Texture",
        state.editing,
        EditingTool::ClarityTexture,
    )
    .id_salt("clarity_texture")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::ClarityTexture)
        {
            ui.disable();
        }
        let c_changed = ui
            .add(
                egui::Slider::new(&mut state.tools.clarity, -1.0..=1.0)
                    .step_by(0.01)
                    .text("Clarity"),
            )
            .changed();
        let t_changed = ui
            .add(
                egui::Slider::new(&mut state.tools.texture, -1.0..=1.0)
                    .step_by(0.01)
                    .text("Texture"),
            )
            .changed();
        if (c_changed || t_changed) && has_image {
            state.update_clarity_texture_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply"))
                .clicked()
            {
                state.push_clarity_texture();
            }
            if state.tools.clarity_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_clarity_texture_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_clarity_texture();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("clarity_texture".to_string(), !default_open);
    }
}
