use egui::{DragValue, Ui, Vec2};

use super::shared::header;
use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Spot Heal ─────────────────────────────────────────────────────────
    {
        let default_open = state.prefs.is_tool_open("heal");
        let heal_label = if state.tools.heal_active {
            format!(
                "✦  Spot Heal  [ACTIVE \u{2014} {} spot{}]",
                state.tools.heal_spots.len(),
                if state.tools.heal_spots.len() == 1 {
                    ""
                } else {
                    "s"
                }
            )
        } else {
            "✦  Spot Heal".to_string()
        };
        let resp = header(state.tools_force_open, heal_label)
            .id_salt("heal")
            .default_open(default_open)
            .show(ui, |ui| {
                if state.editing.is_some() {
                    ui.disable();
                }
                egui::Grid::new("heal_grid")
                    .num_columns(2)
                    .spacing([8.0, 4.0])
                    .show(ui, |ui| {
                        ui.label("Radius:");
                        ui.add(
                            DragValue::new(&mut state.tools.heal_radius)
                                .speed(1)
                                .range(5_u32..=300_u32),
                        );
                        ui.end_row();
                    });

                let mode_btn_text = if state.tools.heal_active {
                    "Stop Painting"
                } else {
                    "Start Painting"
                };
                if ui
                    .add_enabled(
                        has_image,
                        egui::Button::new(mode_btn_text)
                            .min_size(Vec2::new(ui.available_width(), 0.0)),
                    )
                    .clicked()
                {
                    state.tools.heal_active = !state.tools.heal_active;
                }

                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(
                            has_image && !state.tools.heal_spots.is_empty(),
                            egui::Button::new("Apply Heal"),
                        )
                        .clicked()
                    {
                        state.push_heal();
                    }
                    if ui
                        .add_enabled(
                            !state.tools.heal_spots.is_empty(),
                            egui::Button::new("Clear"),
                        )
                        .clicked()
                    {
                        state.tools.heal_spots.clear();
                    }
                });

                if state.tools.heal_active {
                    ui.label(
                        egui::RichText::new(
                            "Click on blemishes to heal them.\nRight-click a spot to remove it.",
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
                .insert("heal".to_string(), !default_open);
        }
    }
}
