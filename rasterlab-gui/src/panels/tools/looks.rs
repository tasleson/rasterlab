use egui::{Ui, Vec2};

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
            let btn =
                egui::Button::new("Classic B&W").min_size(Vec2::new(ui.available_width(), 0.0));
            if ui.add_enabled(has_image, btn).clicked() {
                state.push_classic_bw();
            }
        });
}
