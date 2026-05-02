use egui::Ui;

use super::shared::{header, header_for_tool};
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── HSL Panel ─────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("hsl_panel");
    let resp = header_for_tool(
        state.tools_force_open,
        "🌈  HSL Panel",
        state.editing,
        EditingTool::HslPanel,
    )
    .id_salt("hsl_panel")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::HslPanel)
        {
            ui.disable();
        }
        hsl_panel_ui(ui, state, has_image);
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("hsl_panel".to_string(), !default_open);
    }
}

// ---------------------------------------------------------------------------
// HSL Panel tool
// ---------------------------------------------------------------------------

const HSL_BAND_NAMES: [&str; 8] = [
    "Reds", "Oranges", "Yellows", "Greens", "Aquas", "Blues", "Purples", "Magentas",
];

fn hsl_panel_ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    let mut changed = false;

    let default_open = state.prefs.is_tool_open("hsl_hue");
    let resp = header(state.tools_force_open, "Hue")
        .id_salt("hsl_hue")
        .default_open(default_open)
        .show(ui, |ui| {
            egui::Grid::new("hsl_hue_grid")
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (i, name) in HSL_BAND_NAMES.iter().enumerate() {
                        ui.label(*name);
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut state.tools.hsl_hue[i], -180.0..=180.0)
                                    .text("°")
                                    .step_by(1.0),
                            )
                            .changed();
                        ui.end_row();
                    }
                });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("hsl_hue".to_string(), !default_open);
    }

    let default_open = state.prefs.is_tool_open("hsl_sat");
    let resp = header(state.tools_force_open, "Saturation")
        .id_salt("hsl_sat")
        .default_open(default_open)
        .show(ui, |ui| {
            egui::Grid::new("hsl_sat_grid")
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (i, name) in HSL_BAND_NAMES.iter().enumerate() {
                        ui.label(*name);
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut state.tools.hsl_sat[i], -1.0..=1.0)
                                    .step_by(0.01),
                            )
                            .changed();
                        ui.end_row();
                    }
                });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("hsl_sat".to_string(), !default_open);
    }

    let default_open = state.prefs.is_tool_open("hsl_lum");
    let resp = header(state.tools_force_open, "Luminance")
        .id_salt("hsl_lum")
        .default_open(default_open)
        .show(ui, |ui| {
            egui::Grid::new("hsl_lum_grid")
                .num_columns(2)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    for (i, name) in HSL_BAND_NAMES.iter().enumerate() {
                        ui.label(*name);
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut state.tools.hsl_lum[i], -0.5..=0.5)
                                    .step_by(0.01),
                            )
                            .changed();
                        ui.end_row();
                    }
                });
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("hsl_lum".to_string(), !default_open);
    }

    if changed && has_image {
        state.update_hsl_preview();
    }

    ui.horizontal(|ui| {
        if ui
            .add_enabled(has_image, egui::Button::new("Apply"))
            .clicked()
        {
            state.push_hsl();
        }
        if state.tools.hsl_preview_active
            && ui
                .add_enabled(has_image, egui::Button::new("Cancel"))
                .clicked()
        {
            state.cancel_hsl_preview();
        }
        if ui.button("Reset").clicked() {
            state.reset_hsl();
        }
    });
}
