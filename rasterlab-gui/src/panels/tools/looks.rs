use egui::{Ui, Vec2};
use rasterlab_core::ops::FilmStock;

use super::crop::CropTool;
use super::shared::header;
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Looks ─────────────────────────────────────────────────────────────
    header(state.tools_force_open, "🎞  Looks")
        .id_salt("looks")
        .default_open(false)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }
            let placing_crop = state.tools.sprocket_crop_active;
            let btn =
                egui::Button::new("Classic B&W").min_size(Vec2::new(ui.available_width(), 0.0));
            if ui.add_enabled(has_image && !placing_crop, btn).clicked() {
                state.push_classic_bw();
            }

            ui.add_space(4.0);
            ui.label(egui::RichText::new("35mm Sprocket Panorama").strong());
            egui::Grid::new("sprocket_look_options")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Film stock:");
                    let selected = state
                        .tools
                        .sprocket_film_stock
                        .map_or("Random each time", FilmStock::label);
                    egui::ComboBox::from_id_salt("sprocket_film_stock")
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            if ui
                                .selectable_value(
                                    &mut state.tools.sprocket_film_stock,
                                    None,
                                    "Random each time",
                                )
                                .clicked()
                            {
                                ui.close();
                                ui.ctx().request_repaint();
                            }
                            ui.separator();
                            for stock in FilmStock::ALL {
                                if ui
                                    .selectable_value(
                                        &mut state.tools.sprocket_film_stock,
                                        Some(stock),
                                        stock.label(),
                                    )
                                    .clicked()
                                {
                                    ui.close();
                                    ui.ctx().request_repaint();
                                }
                            }
                        });
                    ui.end_row();
                });

            if placing_crop {
                if let Some(crop) = state.tools.find::<CropTool>() {
                    ui.label(
                        egui::RichText::new(format!(
                            "Crop: {}×{} at ({}, {})",
                            crop.w, crop.h, crop.x, crop.y
                        ))
                        .small()
                        .weak(),
                    );
                }
                ui.label(
                    egui::RichText::new(
                        "Drag inside the crop to move it, or drag its handles to resize.",
                    )
                    .small(),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(has_image, egui::Button::new("Apply Look"))
                        .clicked()
                    {
                        state.push_sprocket_panorama();
                    }
                    if ui.button("Cancel").clicked() {
                        state.cancel_sprocket_crop();
                    }
                });
            } else {
                let btn = egui::Button::new("Position 2:1 Crop…")
                    .min_size(Vec2::new(ui.available_width(), 0.0));
                if ui
                    .add_enabled(has_image && !state.tools.any_preview_active(), btn)
                    .on_hover_text(
                        "Place a fixed 2:1 panorama crop before applying the film border",
                    )
                    .clicked()
                {
                    state.begin_sprocket_crop();
                }
            }
        });
}
