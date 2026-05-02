use egui::Ui;
use rasterlab_core::ops::NrMethod;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Noise Reduction (Advanced) ───────────────────────────────────────
    let default_open = state.prefs.is_tool_open("noise_reduction");
    let resp = header_for_tool(
        state.tools_force_open,
        "◉  Noise Reduction",
        state.editing,
        EditingTool::NoiseReduction,
    )
    .id_salt("noise_reduction")
    .default_open(default_open)
    .show(ui, |ui| {
        if state.editing.is_some() {
            ui.disable();
        }
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::NoiseReduction)
        {
            ui.disable();
        }
        let mut changed = false;
        let old_method = state.tools.nr_method.clone();
        egui::Grid::new("nr_grid")
            .num_columns(2)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                ui.label("Method:");
                egui::ComboBox::from_id_salt("nr_method")
                    .selected_text(match state.tools.nr_method {
                        NrMethod::Wavelet => "Wavelet (fast)",
                        NrMethod::NonLocalMeans => "Non-Local Means",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut state.tools.nr_method,
                            NrMethod::Wavelet,
                            "Wavelet (fast)",
                        );
                        ui.selectable_value(
                            &mut state.tools.nr_method,
                            NrMethod::NonLocalMeans,
                            "Non-Local Means",
                        );
                    });
                ui.end_row();
                if state.tools.nr_method != old_method {
                    changed = true;
                }

                ui.label("Luminance:");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut state.tools.nr_luma, 0.0..=1.0_f32).show_value(true),
                    )
                    .changed();
                ui.end_row();

                ui.label("Color:");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut state.tools.nr_color, 0.0..=1.0_f32)
                            .show_value(true),
                    )
                    .changed();
                ui.end_row();

                ui.label("Detail:");
                changed |= ui
                    .add(
                        egui::Slider::new(&mut state.tools.nr_detail, 0.0..=1.0_f32)
                            .show_value(true),
                    )
                    .changed();
                ui.end_row();
            });

        if state.tools.nr_method == NrMethod::NonLocalMeans {
            ui.label(
                egui::RichText::new("⚠ NLM is slow on large images (30s+)")
                    .small()
                    .color(egui::Color32::from_rgb(200, 150, 50)),
            );
        }

        if changed && has_image {
            state.update_nr_preview();
        }
        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Noise Reduction"))
                .clicked()
            {
                state.push_noise_reduction();
            }
            // Cancel is visible both while a preview is active and while
            // a (potentially slow) noise-reduction render is in flight so
            // the user can abort an NLM pass that has already started.
            if (state.tools.nr_preview_active || state.nr_in_flight())
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_nr_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_noise_reduction();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("noise_reduction".to_string(), !default_open);
    }
}
