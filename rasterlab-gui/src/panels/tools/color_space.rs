use egui::Ui;

use super::shared::header;
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Color Space Conversion ────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("color_space");
    let resp = header(state.tools_force_open, "⬛  Color Space")
        .id_salt("color_space")
        .default_open(default_open)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }
            use rasterlab_core::ops::ColorSpaceConversion;
            egui::ComboBox::from_id_salt("color_space_combo")
                .selected_text(match state.tools.color_space_conversion {
                    ColorSpaceConversion::SrgbToDisplayP3 => "sRGB → Display P3",
                    ColorSpaceConversion::DisplayP3ToSrgb => "Display P3 → sRGB",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut state.tools.color_space_conversion,
                        ColorSpaceConversion::SrgbToDisplayP3,
                        "sRGB → Display P3",
                    );
                    ui.selectable_value(
                        &mut state.tools.color_space_conversion,
                        ColorSpaceConversion::DisplayP3ToSrgb,
                        "Display P3 → sRGB",
                    );
                });
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Conversion"))
                .clicked()
            {
                state.push_color_space();
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("color_space".to_string(), !default_open);
    }
}
