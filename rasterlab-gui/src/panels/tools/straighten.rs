use egui::{Ui, Vec2};

use super::shared::header;
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Straighten ───────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("straighten");
    let straight_label = if state.tools.straighten_active {
        format!("⟳  Straighten  [{:.2}°]", state.tools.straighten_angle)
    } else {
        "⟳  Straighten".to_string()
    };
    let resp = header(state.tools_force_open, straight_label)
        .id_salt("straighten")
        .default_open(default_open)
        .show(ui, |ui| {
            if state.editing.is_some() {
                ui.disable();
            }
            let changed = ui
                .add(
                    egui::Slider::new(&mut state.tools.straighten_angle, -45.0..=45.0)
                        .step_by(0.1)
                        .text("Angle")
                        .suffix("°"),
                )
                .changed();
            if changed && has_image {
                state.update_straighten_preview();
            }

            ui.checkbox(
                &mut state.tools.straighten_crop,
                "Crop to rectangle after apply",
            );

            let toggle_text = if state.tools.straighten_active {
                "Hide Horizon Line"
            } else {
                "Show Horizon Line"
            };
            if ui
                .add_enabled(
                    has_image,
                    egui::Button::new(toggle_text).min_size(Vec2::new(ui.available_width(), 0.0)),
                )
                .clicked()
            {
                state.tools.straighten_active = !state.tools.straighten_active;
            }

            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply Straighten"))
                    .clicked()
                {
                    state.push_straighten();
                }
                if state.tools.straighten_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_straighten_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_straighten();
                }
            });

            if state.tools.straighten_active {
                ui.label(
                    egui::RichText::new(
                        "Drag the horizon line to match a level reference in the image.",
                    )
                    .small()
                    .color(egui::Color32::from_gray(140)),
                );
            }
        });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("straighten".to_string(), !default_open);
    }
}
