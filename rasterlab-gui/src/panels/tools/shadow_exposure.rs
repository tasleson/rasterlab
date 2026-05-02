use egui::Ui;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Shadow Exposure ───────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("shadow_exposure");
    let resp = header_for_tool(
        state.tools_force_open,
        "🌑  Shadow Exposure",
        state.editing,
        EditingTool::ShadowExposure,
    )
    .id_salt("shadow_exposure")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::ShadowExposure)
        {
            ui.disable();
        }
        let mut changed = false;
        egui::Grid::new("shadow_exp_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("EV");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut state.tools.shadow_ev, -3.0..=3.0)
                            .step_by(0.05)
                            .suffix(" stops"),
                    )
                    .on_hover_text("Exposure adjustment applied only in the shadows")
                    .changed();
                ui.end_row();
                ui.label("Falloff");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut state.tools.shadow_falloff, 0.5..=4.0).step_by(0.05),
                    )
                    .on_hover_text(
                        "Higher values restrict the effect to deeper shadows;\n\
                             lower values reach further into the midtones",
                    )
                    .changed();
                ui.end_row();
            });
        if changed && has_image {
            state.update_shadow_exp_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply"))
                .clicked()
            {
                state.push_shadow_exp();
            }
            if state.tools.shadow_exp_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_shadow_exp_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_shadow_exp();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("shadow_exposure".to_string(), !default_open);
    }
}
