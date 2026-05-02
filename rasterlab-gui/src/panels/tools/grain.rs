use egui::{DragValue, Ui};

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

/// Film-grain presets: (label, strength, size).
/// Inspired by popular 35 mm film stocks.
const GRAIN_PRESETS: &[(&str, f32, f32)] = &[
    ("T-Max 100", 0.03, 1.0),   // finest grain, technical pan
    ("Gold 200", 0.05, 1.3),    // Kodak Gold — fine, warm
    ("Portra 400", 0.06, 1.5),  // Kodak Portra — portrait favourite
    ("Pro 400H", 0.07, 1.2),    // Fuji Pro 400H — very fine for 400
    ("HP5 400", 0.09, 1.6),     // Ilford HP5 — classic reportage
    ("Tri-X 400", 0.10, 1.8),   // Kodak Tri-X — definitive B&W stock
    ("Superia 400", 0.08, 1.5), // Fuji Superia — consumer colour
    ("Portra 800", 0.12, 2.0),  // Kodak Portra 800 — low-light portrait
    ("Neopan 1600", 0.18, 2.5), // Fuji Neopan 1600 — gritty documentary
    ("T-Max 3200", 0.25, 3.0),  // Kodak T-Max P3200 — extreme push
    ("Heavy Push", 0.35, 3.5),  // fictional heavy-push look
];

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, _has_image: bool) {
    // ── Grain ─────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("grain");
    let resp = header_for_tool(
        state.tools_force_open,
        "⣿  Grain",
        state.editing,
        EditingTool::Grain,
    )
    .id_salt("grain")
    .default_open(default_open)
    .show(ui, |ui| {
        if state.editing.is_some_and(|s| s.tool != EditingTool::Grain) {
            ui.disable();
        }
        grain_ui(ui, state);
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("grain".to_string(), !default_open);
    }
}

// ---------------------------------------------------------------------------
// Grain tool
// ---------------------------------------------------------------------------

fn grain_ui(ui: &mut Ui, state: &mut AppState) {
    let has_image = state.pipeline().is_some();

    // Film preset buttons.
    ui.label("Film presets:");
    ui.horizontal_wrapped(|ui| {
        for &(label, strength, size) in GRAIN_PRESETS {
            if ui.small_button(label).clicked() && has_image {
                state.tools.grain_strength = strength;
                state.tools.grain_size = size;
                state.update_grain_preview();
            }
        }
    });
    ui.add_space(2.0);

    // Strength and size sliders.
    let mut changed = false;
    egui::Grid::new("grain_grid")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label("Strength");
            changed |= ui
                .add(egui::Slider::new(&mut state.tools.grain_strength, 0.0..=1.0).step_by(0.01))
                .changed();
            ui.end_row();
            ui.label("Size");
            changed |= ui
                .add(egui::Slider::new(&mut state.tools.grain_size, 1.0..=32.0).step_by(0.1))
                .changed();
            ui.end_row();
            ui.label("Seed");
            changed |= ui
                .add(DragValue::new(&mut state.tools.grain_seed))
                .changed();
            ui.end_row();
        });
    if changed && has_image {
        state.update_grain_preview();
    }

    ui.horizontal(|ui| {
        if ui
            .add_enabled(has_image, egui::Button::new("Apply Grain"))
            .clicked()
        {
            state.push_grain();
        }
        if state.tools.grain_preview_active
            && ui
                .add_enabled(has_image, egui::Button::new("Cancel"))
                .clicked()
        {
            state.cancel_grain_preview();
        }
        if ui.button("Reset").clicked() {
            state.reset_grain();
        }
    });
}
