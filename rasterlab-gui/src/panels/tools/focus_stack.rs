use egui::{Color32, Ui};

use super::shared::{header, path_list_ui};
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Focus Stack ───────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("focus_stack");
    let resp = header(state.tools_force_open, "🎯  Focus Stack")
        .id_salt("focus_stack")
        .default_open(default_open)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }
            ui.label(
                egui::RichText::new("Fuse multiple frames at different focus distances")
                    .small()
                    .color(Color32::from_gray(140)),
            );
            ui.add_space(2.0);

            if state.tools.focus_stack_paths.is_empty() {
                ui.label(
                    egui::RichText::new("No frames added yet.")
                        .small()
                        .italics(),
                );
            } else {
                if let Some(idx) =
                    path_list_ui(ui, &state.tools.focus_stack_paths, "focus_stack_list")
                {
                    state.tools.focus_stack_paths.remove(idx);
                    if state.tools.focus_stack_paths.len() < 2 {
                        state.cancel_focus_stack_preview();
                    }
                }
            }

            ui.add_space(4.0);
            if ui
                .add_enabled(has_image, egui::Button::new("+ Add Frame…"))
                .clicked()
            {
                // Seed the list with the current image's path if it's the first entry.
                if state.tools.focus_stack_paths.is_empty()
                    && let Some(p) = state.last_path.as_ref()
                {
                    state
                        .tools
                        .focus_stack_paths
                        .push(p.to_string_lossy().into_owned());
                }
                state.tools.focus_stack_dialog_requested = true;
            }

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let ready = state.tools.focus_stack_paths.len() >= 2;
                if ui
                    .add_enabled(has_image && ready, egui::Button::new("Stack"))
                    .clicked()
                {
                    state.push_focus_stack();
                }
                if state.tools.focus_stack_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_focus_stack_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_focus_stack();
                }
            });

            if state.tools.focus_stack_paths.len() == 1 {
                ui.label(
                    egui::RichText::new("Add at least one more frame to fuse.")
                        .small()
                        .color(egui::Color32::from_rgb(200, 150, 50)),
                );
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("focus_stack".to_string(), !default_open);
    }
}
