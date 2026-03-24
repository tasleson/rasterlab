//! Tools panel — inputs for adding operations to the pipeline.

use egui::{ComboBox, DragValue, Ui};

use crate::state::AppState;

const BW_MODES: &[&str] = &["Luminance (BT.709)", "Average", "Perceptual (BT.601)"];

pub fn ui(ui: &mut Ui, state: &mut AppState) {
    ui.heading("Tools");
    ui.separator();

    let has_image = state.pipeline.is_some();

    // ── Crop ─────────────────────────────────────────────────────────────
    egui::CollapsingHeader::new("✂  Crop")
        .default_open(true)
        .show(ui, |ui| {
            egui::Grid::new("crop_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("X");
                    ui.add(DragValue::new(&mut state.crop_x).speed(1));
                    ui.end_row();
                    ui.label("Y");
                    ui.add(DragValue::new(&mut state.crop_y).speed(1));
                    ui.end_row();
                    ui.label("W");
                    ui.add(
                        DragValue::new(&mut state.crop_w)
                            .speed(1)
                            .range(1..=u32::MAX),
                    );
                    ui.end_row();
                    ui.label("H");
                    ui.add(
                        DragValue::new(&mut state.crop_h)
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

    ui.separator();

    // ── Rotate ───────────────────────────────────────────────────────────
    egui::CollapsingHeader::new("↻  Rotate")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("90°"))
                    .clicked()
                {
                    state.push_rotate_90();
                }
                if ui
                    .add_enabled(has_image, egui::Button::new("180°"))
                    .clicked()
                {
                    state.push_rotate_180();
                }
                if ui
                    .add_enabled(has_image, egui::Button::new("270°"))
                    .clicked()
                {
                    state.push_rotate_270();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Angle:");
                ui.add(
                    DragValue::new(&mut state.rotate_deg)
                        .speed(0.5)
                        .suffix("°")
                        .range(-360.0..=360.0),
                );
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_rotate_arbitrary();
                }
            });
        });

    ui.separator();

    // ── Sharpen ──────────────────────────────────────────────────────────
    egui::CollapsingHeader::new("◈  Sharpen")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Strength:");
                ui.add(
                    DragValue::new(&mut state.sharpen_strength)
                        .speed(0.05)
                        .range(0.0..=10.0),
                );
            });
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Sharpen"))
                .clicked()
            {
                state.push_sharpen();
            }
        });

    ui.separator();

    // ── Black & White ─────────────────────────────────────────────────────
    egui::CollapsingHeader::new("◑  Black & White")
        .default_open(true)
        .show(ui, |ui| {
            ComboBox::from_label("Mode")
                .selected_text(BW_MODES[state.bw_mode_idx])
                .show_ui(ui, |ui| {
                    for (i, &label) in BW_MODES.iter().enumerate() {
                        ui.selectable_value(&mut state.bw_mode_idx, i, label);
                    }
                });
            if ui
                .add_enabled(has_image, egui::Button::new("Apply B&W"))
                .clicked()
            {
                state.push_bw();
            }
        });

    ui.separator();

    // ── Export settings ──────────────────────────────────────────────────
    egui::CollapsingHeader::new("⚙  Export Settings")
        .default_open(false)
        .show(ui, |ui| {
            egui::Grid::new("export_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("JPEG quality:");
                    ui.add(DragValue::new(&mut state.encode_opts.jpeg_quality).range(1..=100u8));
                    ui.end_row();
                    ui.label("PNG compression:");
                    ui.add(DragValue::new(&mut state.encode_opts.png_compression).range(0..=9u8));
                    ui.end_row();
                });
        });
}
