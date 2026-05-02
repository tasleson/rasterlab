use egui::{ComboBox, DragValue, Ui};

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

const BW_MODES: &[&str] = &[
    "Luminance (BT.709)",
    "Average",
    "Perceptual (BT.601)",
    "Channel Mixer",
];

/// Named channel-mixer presets: (label, R, G, B).
/// Weights need not sum to 1 — clamped to [0,255] per-pixel in the op.
const BW_PRESETS: &[(&str, f32, f32, f32)] = &[
    ("Neutral", 0.2126, 0.7152, 0.0722),     // BT.709
    ("Dramatic Contrast", 0.60, 0.40, 0.00), // red/yellow boost, dark skies
    ("Red Filter", 1.00, 0.00, 0.00),        // mimics red lens filter
    ("Green Filter", 0.00, 1.00, 0.00),      // maximum fine detail
    ("Blue Filter", 0.00, 0.00, 1.00),       // hazy, atmospheric
    ("Soften / Skin", 0.25, 0.55, 0.20),     // flatters skin tones
    ("Urban / Cool", 0.00, 0.30, 0.70),      // gritty blue-channel look
    ("High Key", 0.40, 0.50, 0.30),          // weights > sum→lifted midtones
    ("Low Key", 0.10, 0.20, 0.05),           // weights < sum→crushed midtones
    ("Infrared", 0.90, 0.10, -0.10),         // blown-out foliage simulation
];

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Black & White ─────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("bw");
    let resp = header_for_tool(
        state.tools_force_open,
        "◑  Black & White",
        state.editing,
        EditingTool::BlackAndWhite,
    )
    .id_salt("bw")
    .default_open(default_open)
    .show(ui, |ui| {
        if state
            .editing
            .is_some_and(|s| s.tool != EditingTool::BlackAndWhite)
        {
            ui.disable();
        }
        let old_idx = state.tools.bw_mode_idx;
        let combo_resp = ComboBox::from_label("Mode")
            .selected_text(BW_MODES[state.tools.bw_mode_idx])
            .show_ui(ui, |ui| {
                for (i, &label) in BW_MODES.iter().enumerate() {
                    ui.selectable_value(&mut state.tools.bw_mode_idx, i, label);
                }
            });
        if (combo_resp.response.changed() || state.tools.bw_mode_idx != old_idx) && has_image {
            state.update_bw_preview();
        }

        // Channel mixer sliders — only shown when that mode is selected.
        if state.tools.bw_mode_idx == 3 {
            let mut changed = false;

            // Preset buttons — clicking one loads the weights and previews.
            ui.label("Presets:");
            ui.horizontal_wrapped(|ui| {
                for &(label, r, g, b) in BW_PRESETS {
                    if ui.small_button(label).clicked() && has_image {
                        state.tools.bw_mixer_r = r;
                        state.tools.bw_mixer_g = g;
                        state.tools.bw_mixer_b = b;
                        state.update_bw_preview();
                    }
                }
            });
            ui.add_space(2.0);

            egui::Grid::new("bw_mixer_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("R");
                    changed |= ui
                        .add(
                            DragValue::new(&mut state.tools.bw_mixer_r)
                                .speed(0.01)
                                .range(-2.0..=2.0),
                        )
                        .changed();
                    ui.end_row();
                    ui.label("G");
                    changed |= ui
                        .add(
                            DragValue::new(&mut state.tools.bw_mixer_g)
                                .speed(0.01)
                                .range(-2.0..=2.0),
                        )
                        .changed();
                    ui.end_row();
                    ui.label("B");
                    changed |= ui
                        .add(
                            DragValue::new(&mut state.tools.bw_mixer_b)
                                .speed(0.01)
                                .range(-2.0..=2.0),
                        )
                        .changed();
                    ui.end_row();
                });
            if changed && has_image {
                state.update_bw_preview();
            }
        }

        ui.horizontal(|ui| {
            if ui
                .add_enabled(has_image, egui::Button::new("Apply B&W"))
                .clicked()
            {
                state.push_bw();
            }
            if state.tools.bw_preview_active
                && ui
                    .add_enabled(has_image, egui::Button::new("Cancel"))
                    .clicked()
            {
                state.cancel_bw_preview();
            }
            if ui.button("Reset").clicked() {
                state.reset_bw();
            }
        });
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("bw".to_string(), !default_open);
    }
}
