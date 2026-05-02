use egui::{Color32, Ui};

use super::shared::{header, path_list_ui};
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── HDR Merge ─────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("hdr_merge");
    let resp = header(state.tools_force_open, "✺  HDR Merge")
        .id_salt("hdr_merge")
        .default_open(default_open)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }
            ui.label(
                egui::RichText::new("Fuse bracketed exposures into a single extended-range image")
                    .small()
                    .color(Color32::from_gray(140)),
            );
            ui.add_space(2.0);

            if state.tools.hdr_merge_paths.is_empty() {
                ui.label(
                    egui::RichText::new("No exposures added yet.")
                        .small()
                        .italics(),
                );
            } else {
                if let Some(idx) = path_list_ui(ui, &state.tools.hdr_merge_paths, "hdr_merge_list")
                {
                    state.tools.hdr_merge_paths.remove(idx);
                    if state.tools.hdr_merge_paths.len() < 2 {
                        state.cancel_hdr_merge_preview();
                    }
                }
            }

            ui.add_space(4.0);
            if ui
                .add_enabled(has_image, egui::Button::new("+ Add Exposure…"))
                .clicked()
            {
                // Seed the list with the current image's path as the first entry.
                if state.tools.hdr_merge_paths.is_empty()
                    && let Some(p) = state.last_path.as_ref()
                {
                    state
                        .tools
                        .hdr_merge_paths
                        .push(p.to_string_lossy().into_owned());
                }
                state.tools.hdr_merge_dialog_requested = true;
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let ready = state.tools.hdr_merge_paths.len() >= 2;
                if ui
                    .add_enabled(has_image && ready, egui::Button::new("Merge"))
                    .clicked()
                {
                    state.push_hdr_merge();
                }
                if state.tools.hdr_merge_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_hdr_merge_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_hdr_merge();
                }
            });

            if state.tools.hdr_merge_paths.len() == 1 {
                ui.label(
                    egui::RichText::new("Add at least one more bracket to merge.")
                        .small()
                        .color(egui::Color32::from_rgb(200, 150, 50)),
                );
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("hdr_merge".to_string(), !default_open);
    }
}
