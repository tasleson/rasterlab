use egui::{Ui, Vec2};

use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Auto Enhance ──────────────────────────────────────────────────────
    let btn = egui::Button::new("✨  Auto Enhance").min_size(Vec2::new(ui.available_width(), 0.0));
    if ui.add_enabled(has_image, btn).clicked() {
        state.push_auto_enhance();
    }
}
