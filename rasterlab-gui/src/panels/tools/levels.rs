use egui::{Color32, CornerRadius, Rect, Ui, Vec2};

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, _has_image: bool) {
    // ── Levels ────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("levels");
    let resp = header_for_tool(
        state.tools_force_open,
        "▨  Levels",
        state.editing,
        EditingTool::Levels,
    )
    .id_salt("levels")
    .default_open(default_open)
    .show(ui, |ui| {
        if state.editing.is_some_and(|s| s.tool != EditingTool::Levels) {
            ui.disable();
        }
        levels_ui(ui, state);
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("levels".to_string(), !default_open);
    }
}

// ---------------------------------------------------------------------------
// Levels tool
// ---------------------------------------------------------------------------

fn levels_ui(ui: &mut Ui, state: &mut AppState) {
    let has_image = state.pipeline().is_some();

    // Combined histogram
    draw_combined_histogram(ui, state);

    ui.add_space(4.0);

    // Black / midtone / white sliders
    let mut changed = false;

    egui::Grid::new("levels_grid")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            ui.label("Black:");
            let r = ui.add(
                egui::Slider::new(&mut state.tools.levels_black, 0.0..=1.0)
                    .clamping(egui::SliderClamping::Always)
                    .step_by(0.001),
            );
            if r.changed() {
                // Black point must not exceed white point
                if state.tools.levels_black >= state.tools.levels_white {
                    state.tools.levels_black = (state.tools.levels_white - 0.001).max(0.0);
                }
                changed = true;
            }
            ui.end_row();

            ui.label("Mid:");
            let r = ui.add(
                egui::Slider::new(&mut state.tools.levels_mid, 0.10..=10.0)
                    .clamping(egui::SliderClamping::Always)
                    .step_by(0.01)
                    .logarithmic(true),
            );
            if r.changed() {
                changed = true;
            }
            ui.end_row();

            ui.label("White:");
            let r = ui.add(
                egui::Slider::new(&mut state.tools.levels_white, 0.0..=1.0)
                    .clamping(egui::SliderClamping::Always)
                    .step_by(0.001),
            );
            if r.changed() {
                // White point must not go below black point
                if state.tools.levels_white <= state.tools.levels_black {
                    state.tools.levels_white = (state.tools.levels_black + 0.001).min(1.0);
                }
                changed = true;
            }
            ui.end_row();
        });

    if changed && has_image {
        state.update_levels_preview();
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(has_image, egui::Button::new("Apply Levels"))
            .clicked()
        {
            state.apply_levels();
        }
        if state.tools.levels_preview_active
            && ui
                .add_enabled(has_image, egui::Button::new("Cancel"))
                .clicked()
        {
            state.cancel_levels_preview();
        }
        if ui.button("Reset").clicked() {
            state.reset_levels();
        }
    });
}

/// Draw all four histogram channels (R, G, B, L) overlaid on a single canvas,
/// with vertical markers for the current black and white point positions.
fn draw_combined_histogram(ui: &mut Ui, state: &AppState) {
    const HEIGHT: f32 = 96.0;

    let width = ui.available_width().max(256.0);
    let (resp, painter) = ui.allocate_painter(Vec2::new(width, HEIGHT), egui::Sense::hover());
    let rect = resp.rect;

    // Dark background
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_gray(20));

    let Some(hist) = &state.histogram else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No image",
            egui::FontId::monospace(11.0),
            Color32::from_gray(100),
        );
        return;
    };

    // Normalise all channels together so relative brightnesses are preserved.
    let peak = hist
        .red
        .iter()
        .chain(hist.green.iter())
        .chain(hist.blue.iter())
        .chain(hist.luma.iter())
        .copied()
        .max()
        .unwrap_or(1)
        .max(1) as f32;

    let bar_w = (width / 256.0).max(1.0);

    let channels: [(&[u64; 256], Color32); 4] = [
        (
            &hist.luma,
            Color32::from_rgba_unmultiplied(200, 200, 200, 80),
        ),
        (&hist.red, Color32::from_rgba_unmultiplied(220, 60, 60, 120)),
        (
            &hist.green,
            Color32::from_rgba_unmultiplied(60, 180, 60, 120),
        ),
        (
            &hist.blue,
            Color32::from_rgba_unmultiplied(60, 80, 220, 120),
        ),
    ];

    for (data, color) in &channels {
        for (i, &count) in data.iter().enumerate() {
            if count == 0 {
                continue;
            }
            let bar_h = (count as f32 / peak) * HEIGHT;
            let x = rect.left() + i as f32 * bar_w;
            painter.rect_filled(
                Rect::from_min_size(
                    egui::pos2(x, rect.bottom() - bar_h),
                    Vec2::new(bar_w.max(0.5), bar_h),
                ),
                CornerRadius::ZERO,
                *color,
            );
        }
    }

    // Black-point marker (left, dark handle)
    let bx = rect.left() + state.tools.levels_black * width;
    painter.line_segment(
        [egui::pos2(bx, rect.top()), egui::pos2(bx, rect.bottom())],
        egui::Stroke::new(1.5, Color32::from_gray(60)),
    );
    // Small triangle handle at bottom
    let tp = egui::pos2(bx, rect.bottom());
    painter.add(egui::Shape::convex_polygon(
        vec![
            tp,
            egui::pos2(tp.x - 5.0, tp.y + 7.0),
            egui::pos2(tp.x + 5.0, tp.y + 7.0),
        ],
        Color32::from_gray(60),
        egui::Stroke::NONE,
    ));

    // White-point marker (right, bright handle)
    let wx = rect.left() + state.tools.levels_white * width;
    painter.line_segment(
        [egui::pos2(wx, rect.top()), egui::pos2(wx, rect.bottom())],
        egui::Stroke::new(1.5, Color32::from_gray(220)),
    );
    let tp = egui::pos2(wx, rect.bottom());
    painter.add(egui::Shape::convex_polygon(
        vec![
            tp,
            egui::pos2(tp.x - 5.0, tp.y + 7.0),
            egui::pos2(tp.x + 5.0, tp.y + 7.0),
        ],
        Color32::from_gray(220),
        egui::Stroke::NONE,
    ));

    // Midtone marker — positioned at the geometric midpoint between black/white
    let mid_frac =
        state.tools.levels_black + (state.tools.levels_white - state.tools.levels_black) * 0.5;
    let mx = rect.left() + mid_frac * width;
    painter.line_segment(
        [egui::pos2(mx, rect.top()), egui::pos2(mx, rect.bottom())],
        egui::Stroke::new(1.5, Color32::from_rgba_unmultiplied(180, 140, 60, 200)),
    );
    let tp = egui::pos2(mx, rect.bottom());
    painter.add(egui::Shape::convex_polygon(
        vec![
            tp,
            egui::pos2(tp.x - 5.0, tp.y + 7.0),
            egui::pos2(tp.x + 5.0, tp.y + 7.0),
        ],
        Color32::from_rgba_unmultiplied(180, 140, 60, 200),
        egui::Stroke::NONE,
    ));

    // Hover tooltip
    if let Some(pos) = resp.hover_pos() {
        let bucket = ((pos.x - rect.left()) / bar_w).clamp(0.0, 255.0) as usize;
        let text = format!(
            "{}  R:{} G:{} B:{} L:{}",
            bucket, hist.red[bucket], hist.green[bucket], hist.blue[bucket], hist.luma[bucket],
        );
        painter.text(
            egui::pos2(pos.x.min(rect.right() - 10.0), rect.top() + 12.0),
            egui::Align2::LEFT_CENTER,
            text,
            egui::FontId::monospace(10.0),
            Color32::WHITE,
        );
    }
}
