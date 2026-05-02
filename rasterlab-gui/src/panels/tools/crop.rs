use egui::{DragValue, Ui};

use super::shared::header;
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Crop ─────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("crop");
    let resp = header(state.tools_force_open, "✂  Crop")
        .id_salt("crop")
        .default_open(default_open)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }
            egui::Grid::new("crop_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Aspect:");
                    const ASPECT_LABELS: &[&str] =
                        &["Free", "3:2", "4:3", "1:1", "16:9", "9:16", "Custom"];
                    egui::ComboBox::from_id_salt("crop_aspect")
                        .selected_text(ASPECT_LABELS[state.tools.crop_aspect_idx])
                        .show_ui(ui, |ui| {
                            for (i, &label) in ASPECT_LABELS.iter().enumerate() {
                                ui.selectable_value(&mut state.tools.crop_aspect_idx, i, label);
                            }
                        });
                    ui.end_row();

                    // Portrait / landscape toggle — only meaningful for landscape presets.
                    ui.label("Orientation:");
                    let orientation_matters = matches!(state.tools.crop_aspect_idx, 1 | 2 | 4);
                    let btn = egui::Button::new(if state.tools.crop_portrait {
                        "◫ Portrait"
                    } else {
                        "◫ Landscape"
                    })
                    .small();
                    if ui.add_enabled(orientation_matters, btn).clicked() {
                        state.tools.crop_portrait = !state.tools.crop_portrait;
                    }
                    ui.end_row();

                    if state.tools.crop_aspect_idx == 6 {
                        ui.label("Ratio W:H");
                        ui.add(
                            DragValue::new(&mut state.tools.crop_custom_ratio)
                                .speed(0.01)
                                .range(0.1..=20.0_f32),
                        );
                        ui.end_row();
                    }

                    ui.label("X");
                    ui.add(DragValue::new(&mut state.tools.crop_x).speed(1));
                    ui.end_row();
                    ui.label("Y");
                    ui.add(DragValue::new(&mut state.tools.crop_y).speed(1));
                    ui.end_row();
                    ui.label("W");
                    ui.add(
                        DragValue::new(&mut state.tools.crop_w)
                            .speed(1)
                            .range(1..=u32::MAX),
                    );
                    ui.end_row();
                    ui.label("H");
                    ui.add(
                        DragValue::new(&mut state.tools.crop_h)
                            .speed(1)
                            .range(1..=u32::MAX),
                    );
                    ui.end_row();
                });
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Crop"))
                .clicked()
            {
                state.push_crop();
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("crop".to_string(), !default_open);
    }
}
