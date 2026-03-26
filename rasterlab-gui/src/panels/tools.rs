//! Tools panel — inputs for adding operations to the pipeline.

use egui::{Color32, ComboBox, DragValue, Rect, Rounding, Ui, Vec2};

use crate::state::AppState;

const BW_MODES: &[&str] = &["Luminance (BT.709)", "Average", "Perceptual (BT.601)"];

pub fn ui(ui: &mut Ui, state: &mut AppState) {
    ui.heading("Tools");
    ui.separator();

    let has_image = state.pipeline.is_some();

    // ── Crop ─────────────────────────────────────────────────────────────
    egui::CollapsingHeader::new("✂  Crop")
        .default_open(true)
        .show(ui, |ui| {
            egui::Grid::new("crop_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("X");
                    ui.add(DragValue::new(&mut state.crop_x).speed(1));
                    ui.end_row();
                    ui.label("Y");
                    ui.add(DragValue::new(&mut state.crop_y).speed(1));
                    ui.end_row();
                    ui.label("W");
                    ui.add(
                        DragValue::new(&mut state.crop_w)
                            .speed(1)
                            .range(1..=u32::MAX),
                    );
                    ui.end_row();
                    ui.label("H");
                    ui.add(
                        DragValue::new(&mut state.crop_h)
                            .speed(1)
                            .range(1..=u32::MAX),
                    );
                    ui.end_row();
                });
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Crop"))
                .clicked()
            {
                state.push_crop();
            }
        });

    ui.separator();

    // ── Rotate ───────────────────────────────────────────────────────────
    egui::CollapsingHeader::new("↻  Rotate")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("90°"))
                    .clicked()
                {
                    state.push_rotate_90();
                }
                if ui
                    .add_enabled(has_image, egui::Button::new("180°"))
                    .clicked()
                {
                    state.push_rotate_180();
                }
                if ui
                    .add_enabled(has_image, egui::Button::new("270°"))
                    .clicked()
                {
                    state.push_rotate_270();
                }
            });
            ui.horizontal(|ui| {
                ui.label("Angle:");
                ui.add(
                    DragValue::new(&mut state.rotate_deg)
                        .speed(0.5)
                        .suffix("°")
                        .range(-360.0..=360.0),
                );
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_rotate_arbitrary();
                }
            });
        });

    ui.separator();

    // ── Sharpen ──────────────────────────────────────────────────────────
    egui::CollapsingHeader::new("◈  Sharpen")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Strength:");
                ui.add(
                    DragValue::new(&mut state.sharpen_strength)
                        .speed(0.05)
                        .range(0.0..=10.0),
                );
            });
            if ui
                .add_enabled(has_image, egui::Button::new("Apply Sharpen"))
                .clicked()
            {
                state.push_sharpen();
            }
        });

    ui.separator();

    // ── Black & White ─────────────────────────────────────────────────────
    egui::CollapsingHeader::new("◑  Black & White")
        .default_open(true)
        .show(ui, |ui| {
            ComboBox::from_label("Mode")
                .selected_text(BW_MODES[state.bw_mode_idx])
                .show_ui(ui, |ui| {
                    for (i, &label) in BW_MODES.iter().enumerate() {
                        ui.selectable_value(&mut state.bw_mode_idx, i, label);
                    }
                });
            if ui
                .add_enabled(has_image, egui::Button::new("Apply B&W"))
                .clicked()
            {
                state.push_bw();
            }
        });

    ui.separator();

    // ── Levels ────────────────────────────────────────────────────────────
    egui::CollapsingHeader::new("▨  Levels")
        .default_open(true)
        .show(ui, |ui| {
            levels_ui(ui, state);
        });

    ui.separator();

    // ── Export settings ──────────────────────────────────────────────────
    egui::CollapsingHeader::new("⚙  Export Settings")
        .default_open(false)
        .show(ui, |ui| {
            egui::Grid::new("export_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("JPEG quality:");
                    ui.add(DragValue::new(&mut state.encode_opts.jpeg_quality).range(1..=100u8));
                    ui.end_row();
                    ui.label("PNG compression:");
                    ui.add(DragValue::new(&mut state.encode_opts.png_compression).range(0..=9u8));
                    ui.end_row();
                });
        });
}

// ---------------------------------------------------------------------------
// Levels tool
// ---------------------------------------------------------------------------

fn levels_ui(ui: &mut Ui, state: &mut AppState) {
    let has_image = state.pipeline.is_some();

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
                egui::Slider::new(&mut state.levels_black, 0.0..=1.0)
                    .clamping(egui::SliderClamping::Always)
                    .step_by(0.001),
            );
            if r.changed() {
                // Black point must not exceed white point
                if state.levels_black >= state.levels_white {
                    state.levels_black = (state.levels_white - 0.001).max(0.0);
                }
                changed = true;
            }
            ui.end_row();

            ui.label("Mid:");
            let r = ui.add(
                egui::Slider::new(&mut state.levels_mid, 0.10..=10.0)
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
                egui::Slider::new(&mut state.levels_white, 0.0..=1.0)
                    .clamping(egui::SliderClamping::Always)
                    .step_by(0.001),
            );
            if r.changed() {
                // White point must not go below black point
                if state.levels_white <= state.levels_black {
                    state.levels_white = (state.levels_black + 0.001).min(1.0);
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
    painter.rect_filled(rect, Rounding::ZERO, Color32::from_gray(20));

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
                Rounding::ZERO,
                *color,
            );
        }
    }

    // Black-point marker (left, dark handle)
    let bx = rect.left() + state.levels_black * width;
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
    let wx = rect.left() + state.levels_white * width;
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
    let mid_frac = state.levels_black + (state.levels_white - state.levels_black) * 0.5;
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
