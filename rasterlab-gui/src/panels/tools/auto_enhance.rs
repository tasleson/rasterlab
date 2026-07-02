use egui::{Ui, Vec2};

use crate::state::AppState;

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, has_image: bool) {
    // ── Auto Enhance ──────────────────────────────────────────────────────
    let btn = egui::Button::new("✨  Auto Enhance").min_size(Vec2::new(ui.available_width(), 0.0));
    if ui
        .add_enabled(has_image, btn)
        .on_hover_text("Fixed preset: levels stretch, saturation boost, mild sharpen")
        .clicked()
    {
        state.push_auto_enhance();
    }

    // ── Smart Enhance ─────────────────────────────────────────────────────
    let btn = egui::Button::new("🔬  Smart Enhance").min_size(Vec2::new(ui.available_width(), 0.0));
    if ui
        .add_enabled(has_image && state.rendered.is_some(), btn)
        .on_hover_text(
            "Analyses the image first, then applies only the corrections it \
             needs: colour-cast removal, tone, saturation, and sharpening — \
             each as its own editable op",
        )
        .clicked()
    {
        state.push_smart_enhance();
    }
}
