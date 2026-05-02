use egui::Ui;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Highlights & Shadows ──────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("highlights_shadows");
    let resp = header_for_tool(
        state.tools_force_open,
        "◑  Highlights / Shadows",
        state.editing,
        EditingTool::HighlightsShadows,
    )
    .id_salt("highlights_shadows")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::HighlightsShadows)
        {
            ui.disable();
        }
        let mut changed = false;
        egui::Grid::new("hl_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Highlights");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut state.tools.hl_highlights, -1.0..=1.0).step_by(0.01),
                    )
                    .changed();
                ui.end_row();
                ui.label("Shadows");
                changed |= ui
                    .add(egui::Slider::new(&mut state.tools.hl_shadows, -1.0..=1.0).step_by(0.01))
                    .changed();
                ui.end_row();
            });
        if changed && has_image {
            state.update_hl_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply"))
                .clicked()
            {
                state.push_hl();
            }
            if state.tools.hl_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_hl_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_hl();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("highlights_shadows".to_string(), !default_open);
    }
}
