use egui::{DragValue, Ui};

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Split Tone ────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("split_tone");
    let resp = header_for_tool(
        state.tools_force_open,
        "🎨  Split Tone",
        state.editing,
        EditingTool::SplitTone,
    )
    .id_salt("split_tone")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::SplitTone)
        {
            ui.disable();
        }
        let mut changed = false;

        egui::Grid::new("split_tone_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Shadow hue");
                changed |= ui
                    .add(
                        DragValue::new(&mut state.tools.split_shadow_hue)
                            .speed(1.0)
                            .range(0.0..=359.9_f32)
                            .suffix("°"),
                    )
                    .changed();
                ui.end_row();

                ui.label("Shadow sat");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut state.tools.split_shadow_sat, 0.0..=1.0)
                            .step_by(0.01),
                    )
                    .changed();
                ui.end_row();

                ui.label("Highlight hue");
                changed |= ui
                    .add(
                        DragValue::new(&mut state.tools.split_highlight_hue)
                            .speed(1.0)
                            .range(0.0..=359.9_f32)
                            .suffix("°"),
                    )
                    .changed();
                ui.end_row();

                ui.label("Highlight sat");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut state.tools.split_highlight_sat, 0.0..=1.0)
                            .step_by(0.01),
                    )
                    .changed();
                ui.end_row();

                ui.label("Balance");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut state.tools.split_balance, -1.0..=1.0).step_by(0.01),
                    )
                    .changed();
                ui.end_row();
            });

        if changed && has_image {
            state.update_split_preview();
        }

        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply"))
                .clicked()
            {
                state.push_split_tone();
            }
            if ui.button("Reset").clicked() {
                state.reset_split_tone();
            }
            if state.tools.split_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_split_preview();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("split_tone".to_string(), !default_open);
    }
}
