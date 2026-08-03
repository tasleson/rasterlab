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

    // ── Adaptive Enhance ──────────────────────────────────────────────────
    let btn =
        egui::Button::new("🔬  Adaptive Enhance").min_size(Vec2::new(ui.available_width(), 0.0));
    if ui
        .add_enabled(has_image && state.rendered.is_some(), btn)
        .on_hover_text(
            "Analyses the image first, then applies only the corrections it \
             needs: colour-cast removal, tone, saturation, and sharpening — \
             each as its own editable op.  Also reads the frame region by \
             region, so a scan border is measured around and an unevenly-lit \
             frame gets local tone (about a second on a large image)",
        )
        .clicked()
    {
        state.push_adaptive_enhance();
    }

    // ── Old Photo Restore ─────────────────────────────────────────────────
    let btn =
        egui::Button::new("🖼  Old Photo Restore").min_size(Vec2::new(ui.available_width(), 0.0));
    if ui
        .add_enabled(has_image && state.rendered.is_some(), btn)
        .on_hover_text(
            "The same analysis measured over the whole frame only — colour-cast \
             removal, tone, saturation, and sharpening.  Tuned for faded prints \
             and scans, where a whole-frame reading is what the correction \
             values were calibrated against",
        )
        .clicked()
    {
        state.push_old_photo_restore();
    }
}
