use egui::{DragValue, Ui};

use super::shared::header;
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Rotate ───────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("rotate");
    let resp = header(state.tools_force_open, "↻  Rotate")
        .id_salt("rotate")
        .default_open(default_open)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }
            ui.horizontal(|ui| {
                for deg in [90.0_f32, 180.0, 270.0] {
                    if ui
                        .add_enabled(has_image, egui::Button::new(format!("{deg}°")))
                        .clicked()
                    {
                        // Accumulate and normalise to (-360, 360].
                        state.tools.rotate_deg = (state.tools.rotate_deg + deg) % 360.0;
                        state.update_rotate_preview();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Angle:");
                let changed = ui
                    .add(
                        DragValue::new(&mut state.tools.rotate_deg)
                            .speed(0.5)
                            .suffix("°")
                            .range(-360.0..=360.0),
                    )
                    .changed();
                if changed && has_image {
                    state.update_rotate_preview();
                }
            });
            ui.horizontal(|ui| {
                // Only offer Apply when there is a net non-zero rotation.
                let has_rotation = state.tools.rotate_preview_active
                    && (state.tools.rotate_deg % 360.0).abs() > 0.001;
                if has_rotation
                    && ui
                        .add_enabled(has_image, egui::Button::new("Apply"))
                        .clicked()
                {
                    state.push_rotate_arbitrary();
                }
                if state.tools.rotate_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_rotate_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_rotate();
                }
            });
            ui.checkbox(
                &mut state.tools.rotate_crop,
                "Crop to rectangle after apply",
            );
            ui.horizontal(|ui| {
                let h_label = if state.tools.flip_h_pending {
                    "Flip H ✓"
                } else {
                    "Flip H"
                };
                if ui
                    .add_enabled(has_image, egui::Button::new(h_label))
                    .clicked()
                {
                    state.tools.flip_h_pending = !state.tools.flip_h_pending;
                    state.update_flip_preview();
                }
                let v_label = if state.tools.flip_v_pending {
                    "Flip V ✓"
                } else {
                    "Flip V"
                };
                if ui
                    .add_enabled(has_image, egui::Button::new(v_label))
                    .clicked()
                {
                    state.tools.flip_v_pending = !state.tools.flip_v_pending;
                    state.update_flip_preview();
                }
            });
            if state.tools.flip_preview_active {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(has_image, egui::Button::new("Apply"))
                        .clicked()
                    {
                        state.push_flip_pending();
                    }
                    if ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                    {
                        state.cancel_flip_preview();
                    }
                });
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("rotate".to_string(), !default_open);
    }
}
