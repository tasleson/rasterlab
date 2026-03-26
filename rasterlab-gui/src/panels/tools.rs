//! Tools panel — inputs for adding operations to the pipeline.

use egui::{Color32, ComboBox, DragValue, Pos2, Rect, Rounding, Stroke, Ui, Vec2};
use rasterlab_core::ops::CurvesOp;

use crate::state::AppState;

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
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Flip H"))
                    .clicked()
                {
                    state.push_flip_horizontal();
                }
                if ui
                    .add_enabled(has_image, egui::Button::new("Flip V"))
                    .clicked()
                {
                    state.push_flip_vertical();
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

    // ── Vignette ──────────────────────────────────────────────────────────
    egui::CollapsingHeader::new("◎  Vignette")
        .default_open(true)
        .show(ui, |ui| {
            let mut changed = false;
            egui::Grid::new("vignette_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Strength");
                    changed |= ui
                        .add(
                            DragValue::new(&mut state.vignette_strength)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        )
                        .changed();
                    ui.end_row();
                    ui.label("Radius");
                    changed |= ui
                        .add(
                            DragValue::new(&mut state.vignette_radius)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        )
                        .changed();
                    ui.end_row();
                    ui.label("Feather");
                    changed |= ui
                        .add(
                            DragValue::new(&mut state.vignette_feather)
                                .speed(0.01)
                                .range(0.0..=1.0),
                        )
                        .changed();
                    ui.end_row();
                });
            if changed && has_image {
                state.update_vignette_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply Vignette"))
                    .clicked()
                {
                    state.push_vignette();
                }
                if state.vignette_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_vignette_preview();
                }
            });
        });

    ui.separator();

    // ── Black & White ─────────────────────────────────────────────────────
    egui::CollapsingHeader::new("◑  Black & White")
        .default_open(true)
        .show(ui, |ui| {
            let old_idx = state.bw_mode_idx;
            let combo_resp = ComboBox::from_label("Mode")
                .selected_text(BW_MODES[state.bw_mode_idx])
                .show_ui(ui, |ui| {
                    for (i, &label) in BW_MODES.iter().enumerate() {
                        ui.selectable_value(&mut state.bw_mode_idx, i, label);
                    }
                });
            if (combo_resp.response.changed() || state.bw_mode_idx != old_idx) && has_image {
                state.update_bw_preview();
            }

            // Channel mixer sliders — only shown when that mode is selected.
            if state.bw_mode_idx == 3 {
                let mut changed = false;

                // Preset buttons — clicking one loads the weights and previews.
                ui.label("Presets:");
                ui.horizontal_wrapped(|ui| {
                    for &(label, r, g, b) in BW_PRESETS {
                        if ui.small_button(label).clicked() && has_image {
                            state.bw_mixer_r = r;
                            state.bw_mixer_g = g;
                            state.bw_mixer_b = b;
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
                                DragValue::new(&mut state.bw_mixer_r)
                                    .speed(0.01)
                                    .range(-2.0..=2.0),
                            )
                            .changed();
                        ui.end_row();
                        ui.label("G");
                        changed |= ui
                            .add(
                                DragValue::new(&mut state.bw_mixer_g)
                                    .speed(0.01)
                                    .range(-2.0..=2.0),
                            )
                            .changed();
                        ui.end_row();
                        ui.label("B");
                        changed |= ui
                            .add(
                                DragValue::new(&mut state.bw_mixer_b)
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
                if state.bw_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_bw_preview();
                }
            });
        });

    ui.separator();

    // ── Brightness / Contrast ─────────────────────────────────────────────
    egui::CollapsingHeader::new("☀  Brightness / Contrast")
        .default_open(true)
        .show(ui, |ui| {
            let mut changed = false;
            egui::Grid::new("bc_grid")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Brightness");
                    changed |= ui
                        .add(egui::Slider::new(&mut state.bc_brightness, -1.0..=1.0).step_by(0.01))
                        .changed();
                    ui.end_row();
                    ui.label("Contrast");
                    changed |= ui
                        .add(egui::Slider::new(&mut state.bc_contrast, -1.0..=1.0).step_by(0.01))
                        .changed();
                    ui.end_row();
                });
            if changed && has_image {
                state.update_bc_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_bc();
                }
                if state.bc_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_bc_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_bc();
                }
            });
        });

    ui.separator();

    // ── Saturation ────────────────────────────────────────────────────────
    egui::CollapsingHeader::new("🎨  Saturation")
        .default_open(true)
        .show(ui, |ui| {
            let changed = ui
                .add(egui::Slider::new(&mut state.saturation, 0.0..=4.0).step_by(0.01))
                .changed();
            if changed && has_image {
                state.update_sat_preview();
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(has_image, egui::Button::new("Apply"))
                    .clicked()
                {
                    state.push_saturation();
                }
                if state.sat_preview_active
                    && ui
                        .add_enabled(has_image, egui::Button::new("Cancel"))
                        .clicked()
                {
                    state.cancel_sat_preview();
                }
                if ui.button("Reset").clicked() {
                    state.reset_saturation();
                }
            });
        });

    ui.separator();

    // ── Curves ────────────────────────────────────────────────────────────
    egui::CollapsingHeader::new("〜  Curves")
        .default_open(true)
        .show(ui, |ui| {
            curves_ui(ui, state);
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
// Curves tool
// ---------------------------------------------------------------------------

fn curves_ui(ui: &mut Ui, state: &mut AppState) {
    let has_image = state.pipeline.is_some();

    // Square canvas — fill available width up to 200 px.
    let size = ui.available_width().min(200.0);
    let (resp, painter) = ui.allocate_painter(Vec2::splat(size), egui::Sense::click_and_drag());
    let rect = resp.rect;
    let w = rect.width();
    let h = rect.height();

    // Background and grid.
    painter.rect_filled(rect, Rounding::ZERO, Color32::from_gray(25));
    for i in 1..4 {
        let t = i as f32 / 4.0;
        let gx = rect.min.x + t * w;
        let gy = rect.min.y + t * h;
        let grid_col = Color32::from_gray(50);
        painter.line_segment(
            [Pos2::new(gx, rect.min.y), Pos2::new(gx, rect.max.y)],
            Stroke::new(1.0, grid_col),
        );
        painter.line_segment(
            [Pos2::new(rect.min.x, gy), Pos2::new(rect.max.x, gy)],
            Stroke::new(1.0, grid_col),
        );
    }
    // Identity diagonal (subtle reference).
    painter.line_segment(
        [
            Pos2::new(rect.min.x, rect.max.y),
            Pos2::new(rect.max.x, rect.min.y),
        ],
        Stroke::new(1.0, Color32::from_gray(60)),
    );

    // Build and draw the curve.
    let lut = CurvesOp::build_lut(&state.curve_points);
    {
        let mut prev: Option<Pos2> = None;
        for (i, &y_val) in lut.iter().enumerate() {
            let cx = rect.min.x + (i as f32 / 255.0) * w;
            let cy = rect.max.y - (y_val as f32 / 255.0) * h;
            let pos = Pos2::new(cx, cy);
            if let Some(p) = prev {
                painter.line_segment([p, pos], Stroke::new(1.5, Color32::WHITE));
            }
            prev = Some(pos);
        }
    }

    // Draw control point handles.
    const PT_R: f32 = 5.0;
    for (i, &[px, py]) in state.curve_points.iter().enumerate() {
        let sx = rect.min.x + px * w;
        let sy = rect.max.y - py * h;
        let col = if state.curve_dragging_idx == Some(i) {
            Color32::from_rgb(255, 200, 0)
        } else {
            Color32::WHITE
        };
        painter.circle_filled(Pos2::new(sx, sy), PT_R, col);
        painter.circle_stroke(Pos2::new(sx, sy), PT_R, Stroke::new(1.0, Color32::BLACK));
    }

    // ── Interaction ───────────────────────────────────────────────────────
    let (mouse_pos, primary_down, primary_pressed, secondary_pressed) = ui.input(|i| {
        (
            i.pointer.interact_pos(),
            i.pointer.button_down(egui::PointerButton::Primary),
            i.pointer.button_pressed(egui::PointerButton::Primary),
            i.pointer.button_pressed(egui::PointerButton::Secondary),
        )
    });

    // Release drag.
    if !primary_down {
        state.curve_dragging_idx = None;
    }

    if let Some(pos) = mouse_pos {
        // Convert screen position to curve coordinates.
        let cx = ((pos.x - rect.min.x) / w).clamp(0.0, 1.0);
        let cy = (1.0 - (pos.y - rect.min.y) / h).clamp(0.0, 1.0);

        // Continue existing drag.
        if primary_down && let Some(drag_idx) = state.curve_dragging_idx {
            let npts = state.curve_points.len();
            let new_x = if drag_idx == 0 {
                0.0
            } else if drag_idx == npts - 1 {
                1.0
            } else {
                // Constrain x between neighbours so sort order is preserved.
                let lo = state.curve_points[drag_idx - 1][0] + 0.005;
                let hi = state.curve_points[drag_idx + 1][0] - 0.005;
                cx.clamp(lo, hi)
            };
            let old = state.curve_points[drag_idx];
            state.curve_points[drag_idx] = [new_x, cy];
            if state.curve_points[drag_idx] != old && has_image {
                state.update_curve_preview();
            }
        }

        if primary_pressed && rect.contains(pos) {
            // Find a control point close enough to start a drag.
            let hit = state.curve_points.iter().position(|&[px, py]| {
                let sx = rect.min.x + px * w;
                let sy = rect.max.y - py * h;
                ((pos.x - sx).powi(2) + (pos.y - sy).powi(2)).sqrt() < PT_R + 3.0
            });
            if let Some(idx) = hit {
                state.curve_dragging_idx = Some(idx);
            } else {
                // Click on empty area → add a new point.
                state.curve_points.push([cx, cy]);
                state
                    .curve_points
                    .sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
                if has_image {
                    state.update_curve_preview();
                }
            }
        }

        if secondary_pressed && rect.contains(pos) {
            // Right-click → remove the nearest non-endpoint control point.
            let hit = state.curve_points[1..state.curve_points.len() - 1]
                .iter()
                .enumerate()
                .find(|(_, pt)| {
                    let sx = rect.min.x + pt[0] * w;
                    let sy = rect.max.y - pt[1] * h;
                    ((pos.x - sx).powi(2) + (pos.y - sy).powi(2)).sqrt() < PT_R + 4.0
                })
                .map(|(i, _)| i + 1); // offset by 1 for the slice starting at index 1
            if let Some(idx) = hit {
                state.curve_points.remove(idx);
                if has_image {
                    state.update_curve_preview();
                }
            }
        }
    }

    ui.add_space(2.0);
    ui.horizontal(|ui| {
        if ui
            .add_enabled(has_image, egui::Button::new("Apply Curve"))
            .clicked()
        {
            state.push_curves();
        }
        if state.curve_preview_active
            && ui
                .add_enabled(has_image, egui::Button::new("Cancel"))
                .clicked()
        {
            state.cancel_curve_preview();
        }
        if ui.button("Reset").clicked() {
            state.reset_curves();
        }
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
