use egui::Ui;

use super::shared::{header, path_list_ui};
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Panorama ──────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("panorama");
    let resp = header(state.tools_force_open, "🌅  Panorama")
        .id_salt("panorama")
        .default_open(default_open)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }
            // Image list
            if state.tools.panorama_paths.is_empty() {
                ui.label(
                    egui::RichText::new("No images added yet.")
                        .small()
                        .italics(),
                );
            } else {
                if let Some(idx) = path_list_ui(ui, &state.tools.panorama_paths, "panorama_list") {
                    state.tools.panorama_paths.remove(idx);
                    if state.tools.panorama_paths.len() < 2 {
                        state.cancel_panorama_preview();
                    }
                }
            }

            ui.add_space(4.0);
            if ui
                .add_enabled(has_image, egui::Button::new("+ Add Image…"))
                .clicked()
            {
                // Seed the list with the current image's path if it's the first entry.
                if state.tools.panorama_paths.is_empty()
                    && let Some(p) = state.last_path.as_ref()
                {
                    state
                        .tools
                        .panorama_paths
                        .push(p.to_string_lossy().into_owned());
                }
                state.tools.panorama_dialog_requested = true;
            }

            ui.add_space(4.0);
            egui::Grid::new("panorama_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Feather:");
                    let changed = ui
                        .add(
                            egui::Slider::new(&mut state.tools.panorama_feather_px, 1u32..=300)
                                .suffix(" px"),
                        )
                        .changed();
                    ui.end_row();
                    if changed && state.tools.panorama_paths.len() >= 2 {
                        state.tools.panorama_preview_active = true;
                        state.request_render();
                    }
                });

            ui.horizontal(|ui| {
                let ready = state.tools.panorama_paths.len() >= 2;
                if ui
                    .add_enabled(has_image && ready, egui::Button::new("Stitch"))
                    .clicked()
                {
                    state.push_panorama();
                }
                if state.tools.panorama_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_panorama_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_panorama();
                }
            });

            if state.tools.panorama_paths.len() == 1 {
                ui.label(
                    egui::RichText::new("Add at least one more image to stitch.")
                        .small()
                        .color(egui::Color32::from_rgb(200, 150, 50)),
                );
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("panorama".to_string(), !default_open);
    }
}
