use egui::{Color32, CornerRadius, Pos2, Stroke, Ui, Vec2};
use rasterlab_core::ops::CurvesOp;

use super::shared::header_for_tool;
use crate::state::{AppState, EditingTool};

pub(super) fn ui(ui: &mut Ui, state: &mut AppState, _has_image: bool) {
    // ── Curves ────────────────────────────────────────────────────────────
    let default_open = state.prefs.is_tool_open("curves");
    let resp = header_for_tool(
        state.tools_force_open,
        "〜  Curves",
        state.editing,
        EditingTool::Curves,
    )
    .id_salt("curves")
    .default_open(default_open)
    .show(ui, |ui| {
        if state.editing.is_some_and(|s| s.tool != EditingTool::Curves) {
            ui.disable();
        }
        curves_ui(ui, state);
    });
    if resp.header_response.clicked() {
        state
            .prefs
            .tools_open
            .insert("curves".to_string(), !default_open);
    }
}

// ---------------------------------------------------------------------------
// Curves tool
// ---------------------------------------------------------------------------

fn curves_ui(ui: &mut Ui, state: &mut AppState) {
    let has_image = state.pipeline().is_some();

    // Square canvas — fill available width up to 200 px.
    let size = ui.available_width().min(200.0);
    let (resp, painter) = ui.allocate_painter(Vec2::splat(size), egui::Sense::click_and_drag());
    let rect = resp.rect;
    let w = rect.width();
    let h = rect.height();

    // Background and grid.
    painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_gray(25));
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
    let lut = CurvesOp::build_lut(&state.tools.curve_points);
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
    for (i, &[px, py]) in state.tools.curve_points.iter().enumerate() {
        let sx = rect.min.x + px * w;
        let sy = rect.max.y - py * h;
        let col = if state.tools.curve_dragging_idx == Some(i) {
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
        state.tools.curve_dragging_idx = None;
    }

    if let Some(pos) = mouse_pos {
        // Convert screen position to curve coordinates.
        let cx = ((pos.x - rect.min.x) / w).clamp(0.0, 1.0);
        let cy = (1.0 - (pos.y - rect.min.y) / h).clamp(0.0, 1.0);

        // Continue existing drag.
        if primary_down && let Some(drag_idx) = state.tools.curve_dragging_idx {
            let npts = state.tools.curve_points.len();
            let new_x = if drag_idx == 0 {
                0.0
            } else if drag_idx == npts - 1 {
                1.0
            } else {
                // Constrain x between neighbours so sort order is preserved.
                let lo = state.tools.curve_points[drag_idx - 1][0] + 0.005;
                let hi = state.tools.curve_points[drag_idx + 1][0] - 0.005;
                cx.clamp(lo, hi)
            };
            let old = state.tools.curve_points[drag_idx];
            state.tools.curve_points[drag_idx] = [new_x, cy];
            if state.tools.curve_points[drag_idx] != old && has_image {
                state.update_curve_preview();
            }
        }

        if primary_pressed && rect.contains(pos) {
            // Find a control point close enough to start a drag.
            let hit = state.tools.curve_points.iter().position(|&[px, py]| {
                let sx = rect.min.x + px * w;
                let sy = rect.max.y - py * h;
                ((pos.x - sx).powi(2) + (pos.y - sy).powi(2)).sqrt() < PT_R + 3.0
            });
            if let Some(idx) = hit {
                state.tools.curve_dragging_idx = Some(idx);
            } else {
                // Click on empty area → add a new point.
                state.tools.curve_points.push([cx, cy]);
                state
                    .tools
                    .curve_points
                    .sort_by(|a, b| a[0].partial_cmp(&b[0]).unwrap());
                if has_image {
                    state.update_curve_preview();
                }
            }
        }

        if secondary_pressed && rect.contains(pos) {
            // Right-click → remove the nearest non-endpoint control point.
            let hit = state.tools.curve_points[1..state.tools.curve_points.len() - 1]
                .iter()
                .enumerate()
                .find(|(_, pt)| {
                    let sx = rect.min.x + pt[0] * w;
                    let sy = rect.max.y - pt[1] * h;
                    ((pos.x - sx).powi(2) + (pos.y - sy).powi(2)).sqrt() < PT_R + 4.0
                })
                .map(|(i, _)| i + 1); // offset by 1 for the slice starting at index 1
            if let Some(idx) = hit {
                state.tools.curve_points.remove(idx);
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
        if state.tools.curve_preview_active
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
