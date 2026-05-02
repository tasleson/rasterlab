use egui::Ui;

use super::shared::header;
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── LUT (Color Grading) ───────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("lut");
    let resp = header(state.tools_force_open, "🎞  LUT / Color Grading")
        .id_salt("lut")
        .default_open(default_open)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }
            if ui.button("Load .cube LUT…").clicked() {
                state.tools.lut_dialog_requested = true;
            }
            if !state.tools.lut_name.is_empty() {
                ui.label(format!("Loaded: {}", state.tools.lut_name));
                let changed = ui
                    .add(
                        egui::Slider::new(&mut state.tools.lut_strength, 0.0..=1.0)
                            .step_by(0.01)
                            .text("Strength"),
                    )
                    .changed();
                if changed && has_image {
                    state.update_lut_preview();
                }
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(has_image, egui::Button::new("Apply LUT"))
                        .clicked()
                    {
                        state.push_lut();
                    }
                    if state.tools.lut_preview_active
                        && ui
                            .add_enabled(has_image, egui::Button::new("Cancel"))
                            .clicked()
                    {
                        state.cancel_lut_preview();
                    }
                    if ui.button("Reset").clicked() {
                        state.reset_lut();
                    }
                });
            } else {
                ui.label("No LUT loaded.");
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("lut".to_string(), !default_open);
    }
}
