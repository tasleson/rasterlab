use egui::{DragValue, Ui};

use super::shared::header;
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, _has_image: bool) {
    // ── Export settings ──────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("export_settings");
    let resp = header(state.tools_force_open, "⚙  Export Settings")
        .id_salt("export_settings")
        .default_open(default_open)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }
            let mut export_changed = false;
            egui::Grid::new("export_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("JPEG quality:");
                    if ui
                        .add(
                            DragValue::new(&mut state.tools.encode_opts.jpeg_quality)
                                .range(1..=100u8),
                        )
                        .changed()
                    {
                        export_changed = true;
                    }
                    ui.end_row();
                    ui.label("PNG compression:");
                    if ui
                        .add(
                            DragValue::new(&mut state.tools.encode_opts.png_compression)
                                .range(0..=9u8),
                        )
                        .changed()
                    {
                        export_changed = true;
                    }
                    ui.end_row();
                });

            ui.separator();
            if ui
                .checkbox(
                    &mut state.tools.encode_opts.preserve_metadata,
                    "Preserve metadata on export",
                )
                .changed()
            {
                export_changed = true;
            }
            if export_changed {
                state.prefs.jpeg_quality = state.tools.encode_opts.jpeg_quality;
                state.prefs.png_compression = state.tools.encode_opts.png_compression;
                state.prefs.preserve_metadata = state.tools.encode_opts.preserve_metadata;
                state.prefs.save();
            }

            ui.separator();
            ui.label("Resize on export:");
            ui.checkbox(&mut state.tools.export_resize_enabled, "Enable");
            if state.tools.export_resize_enabled {
                egui::Grid::new("export_resize_grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Width:");
                        ui.add(
                            DragValue::new(&mut state.tools.export_resize_w)
                                .range(1..=65535u32)
                                .suffix(" px"),
                        );
                        ui.end_row();
                        ui.label("Height:");
                        ui.add(
                            DragValue::new(&mut state.tools.export_resize_h)
                                .range(1..=65535u32)
                                .suffix(" px"),
                        );
                        ui.end_row();
                        ui.label("Mode:");
                        egui::ComboBox::from_id_salt("export_resize_mode")
                            .selected_text(format!("{:?}", state.tools.export_resize_mode))
                            .show_ui(ui, |ui| {
                                use rasterlab_core::ops::ResampleMode;
                                ui.selectable_value(
                                    &mut state.tools.export_resize_mode,
                                    ResampleMode::NearestNeighbour,
                                    "Nearest",
                                );
                                ui.selectable_value(
                                    &mut state.tools.export_resize_mode,
                                    ResampleMode::Bilinear,
                                    "Bilinear",
                                );
                                ui.selectable_value(
                                    &mut state.tools.export_resize_mode,
                                    ResampleMode::Bicubic,
                                    "Bicubic",
                                );
                            });
                        ui.end_row();
                    });
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("export_settings".to_string(), !default_open);
    }
}
