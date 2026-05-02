use egui::Ui;

use super::shared::header;
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, _has_image: bool) {
    // ── Masking ───────────────────────────────────────────────────────────
    // Global modifier: when active, the next "Apply" button wraps its
    // operation in a MaskedOp that restricts the effect to the masked region.
    const MASK_LABELS: &[&str] = &["None", "Linear Gradient", "Radial Gradient"];
    let mask_default_open = state.prefs.is_tool_open("masking");
    let mask_active = state.tools.mask_sel > 0;
    let mask_header = if mask_active {
        format!("◈  Masking  [{}]", MASK_LABELS[state.tools.mask_sel])
    } else {
        "◈  Masking".to_string()
    };
    let mask_resp = header(state.tools_force_open, mask_header)
        .id_salt("masking")
        .default_open(mask_default_open)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }
            egui::Grid::new("mask_type_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Type:");
                    egui::ComboBox::from_id_salt("mask_type_combo")
                        .selected_text(MASK_LABELS[state.tools.mask_sel])
                        .show_ui(ui, |ui| {
                            for (i, &label) in MASK_LABELS.iter().enumerate() {
                                ui.selectable_value(&mut state.tools.mask_sel, i, label);
                            }
                        });
                    ui.end_row();
                });

            if state.tools.mask_sel == 1 {
                // Linear gradient controls
                ui.separator();
                egui::Grid::new("mask_lin_grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Angle:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_lin_angle, 0.0..=360.0)
                                .suffix("°")
                                .step_by(1.0),
                        );
                        ui.end_row();
                        ui.label("Center X:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_lin_cx, 0.0..=1.0)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Center Y:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_lin_cy, 0.0..=1.0)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Feather:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_lin_feather, 0.01..=1.0)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Invert:");
                        ui.checkbox(&mut state.tools.mask_lin_invert, "");
                        ui.end_row();
                    });
            }

            if state.tools.mask_sel == 2 {
                // Radial gradient controls
                ui.separator();
                egui::Grid::new("mask_rad_grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Center X:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_rad_cx, 0.0..=1.0)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Center Y:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_rad_cy, 0.0..=1.0)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Radius:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_rad_radius, 0.01..=1.5)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Feather:");
                        ui.add(
                            egui::Slider::new(&mut state.tools.mask_rad_feather, 0.01..=2.0)
                                .step_by(0.01),
                        );
                        ui.end_row();
                        ui.label("Invert:");
                        ui.checkbox(&mut state.tools.mask_rad_invert, "");
                        ui.end_row();
                    });
            }

            if mask_active {
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("⚠ Next Apply will be masked")
                        .small()
                        .color(egui::Color32::from_rgb(220, 160, 40)),
                );
                if ui.small_button("Clear mask").clicked() {
                    state.tools.mask_sel = 0;
                }
            }
        });
    if mask_resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("masking".to_string(), !mask_default_open);
    }
}
