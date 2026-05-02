use egui::{DragValue, Ui};

use super::shared::header;
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Resize ────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("resize");
    let resp = header(state.tools_force_open, "⤢  Resize")
        .id_salt("resize")
        .default_open(default_open)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }
            use rasterlab_core::ops::ResampleMode;

            let orig_w = state.tools.resize_w;
            let orig_h = state.tools.resize_h;

            // Source image dims for preset filtering and aspect-ratio math.
            let src_dims = state
                .pipeline()
                .map(|p: &rasterlab_core::pipeline::EditPipeline| {
                    (p.source().width, p.source().height)
                });

            // ── MP preset dropdown ────────────────────────────────────
            // (label, total target pixels)
            const MP_PRESETS: &[(&str, u32)] = &[
                ("24 MP", 24_000_000),
                ("20 MP", 20_000_000),
                ("16 MP", 16_000_000),
                ("12 MP", 12_000_000),
                ("10 MP", 10_000_000),
                ("8 MP", 8_000_000),
                ("6 MP", 6_000_000),
                ("5 MP", 5_000_000),
                ("4 MP", 4_000_000),
                ("3 MP", 3_000_000),
                ("2 MP", 2_000_000),
                ("1 MP", 1_000_000),
            ];

            if let Some((src_w, src_h)) = src_dims {
                let src_px = src_w as u64 * src_h as u64;
                // h/w aspect ratio of the source image.
                let aspect = src_h as f64 / src_w as f64;

                let available: Vec<(&str, u32)> = MP_PRESETS
                    .iter()
                    .filter(|(_, px)| (*px as u64) < src_px)
                    .map(|(lbl, px)| (*lbl, *px))
                    .collect();

                if !available.is_empty() {
                    // Check whether the current w×h matches a preset (±2 px rounding).
                    let cur_px = orig_w as u64 * orig_h as u64;
                    let selected_label = available
                        .iter()
                        .find(|(_, target_px)| {
                            let w = ((*target_px as f64 / aspect).sqrt().round() as u32).max(1);
                            let h = (w as f64 * aspect).round() as u32;
                            (w as u64 * h as u64).abs_diff(cur_px) <= 2
                        })
                        .map(|(lbl, _)| *lbl)
                        .unwrap_or("— Preset —");

                    ui.horizontal(|ui| {
                        ui.label("Preset");
                        egui::ComboBox::from_id_salt("resize_mp_preset")
                            .selected_text(selected_label)
                            .show_ui(ui, |ui| {
                                for (lbl, target_px) in &available {
                                    let w =
                                        ((*target_px as f64 / aspect).sqrt().round() as u32).max(1);
                                    let h = (w as f64 * aspect).round() as u32;
                                    let hint = format!("{lbl}  ({w}×{h})");
                                    if ui.selectable_label(selected_label == *lbl, hint).clicked() {
                                        state.tools.resize_w = w;
                                        state.tools.resize_h = h;
                                    }
                                }
                            });
                    });
                    ui.add_space(4.0);
                }
            }

            egui::Grid::new("resize_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Width");
                    let w_resp = ui.add(
                        DragValue::new(&mut state.tools.resize_w)
                            .speed(1)
                            .range(1..=32000_u32)
                            .suffix(" px"),
                    );
                    ui.end_row();
                    ui.label("Height");
                    let h_resp = ui.add(
                        DragValue::new(&mut state.tools.resize_h)
                            .speed(1)
                            .range(1..=32000_u32)
                            .suffix(" px"),
                    );
                    ui.end_row();
                    ui.label("Method");
                    egui::ComboBox::from_id_salt("resize_mode")
                        .selected_text(match state.tools.resize_mode {
                            ResampleMode::NearestNeighbour => "Nearest",
                            ResampleMode::Bilinear => "Bilinear",
                            ResampleMode::Bicubic => "Bicubic",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.tools.resize_mode,
                                ResampleMode::NearestNeighbour,
                                "Nearest",
                            );
                            ui.selectable_value(
                                &mut state.tools.resize_mode,
                                ResampleMode::Bilinear,
                                "Bilinear",
                            );
                            ui.selectable_value(
                                &mut state.tools.resize_mode,
                                ResampleMode::Bicubic,
                                "Bicubic",
                            );
                        });
                    ui.end_row();
                    ui.label("Lock aspect");
                    ui.checkbox(&mut state.tools.resize_lock_aspect, "");
                    ui.end_row();

                    // Propagate aspect-ratio constraint after editing.
                    if state.tools.resize_lock_aspect && orig_w > 0 && orig_h > 0 {
                        if w_resp.changed() && orig_w != state.tools.resize_w {
                            let ratio = orig_h as f64 / orig_w as f64;
                            state.tools.resize_h =
                                ((state.tools.resize_w as f64 * ratio).round() as u32).max(1);
                        } else if h_resp.changed() && orig_h != state.tools.resize_h {
                            let ratio = orig_w as f64 / orig_h as f64;
                            state.tools.resize_w =
                                ((state.tools.resize_h as f64 * ratio).round() as u32).max(1);
                        }
                    }
                });

            if ui
                .add_enabled(has_image, egui::Button::new("Apply Resize"))
                .clicked()
            {
                state.push_resize();
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("resize".to_string(), !default_open);
    }
}
