use egui::{Color32, Ui};

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Faux HDR ──────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("faux_hdr");
    let resp = header_for_tool(
        state.tools_force_open,
        "◈  Faux HDR",
        state.editing,
        EditingTool::FauxHdr,
    )
    .id_salt("faux_hdr")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::FauxHdr)
        {
            ui.disable();
        }
        ui.label(
            egui::RichText::new("Exposure fusion from ±1 stop virtual brackets")
                .small()
                .color(Color32::from_gray(140)),
        );
        ui.add_space(2.0);
        let changed = ui
            .add(
                egui::Slider::new(&mut state.tools.hdr_strength, 0.0..=1.0)
                    .text("Strength")
                    .step_by(0.01),
            )
            .changed();
        if changed && has_image {
            state.update_hdr_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply"))
                .clicked()
            {
                state.push_hdr();
            }
            if state.tools.hdr_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_hdr_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_hdr();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("faux_hdr".to_string(), !default_open);
    }
}
