//! Straighten: a draggable horizon line over a fifths grid, driving the
//! straighten tool's angle and its live preview.

use egui::{Color32, Pos2, Rect, Stroke, Ui};

use crate::panels::tools::straighten::StraightenTool;
use crate::state::AppState;

use super::coords::{image_to_screen, screen_to_image};
use super::{CanvasState, CanvasView};

impl CanvasState {
    /// Drive the horizon line and draw the straighten overlay.
    pub(super) fn handle_straighten(
        &mut self,
        ui: &mut Ui,
        painter: &egui::Painter,
        state: &mut AppState,
        view: &CanvasView,
    ) {
        let CanvasView {
            image_tl,
            display_size,
            img_w,
            img_h,
            over_canvas,
            ..
        } = *view;

        // Clear crop selection while straighten mode is active.
        self.crop_start = None;
        self.crop_end = None;

        if over_canvas {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        }

        // Initialise the horizon line if not yet set.
        if self.straighten_line.is_none() {
            let cx = img_w as f32 / 2.0;
            let cy = img_h as f32 / 2.0;
            let half_w = img_w as f32 * 0.35;
            self.straighten_line = Some([Pos2::new(cx - half_w, cy), Pos2::new(cx + half_w, cy)]);
            state.tools.find_mut::<StraightenTool>().unwrap().angle = 0.0;
            state.update_straighten_preview();
        }

        let (ptr_pos, primary_pressed, primary_down, primary_released) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_down(egui::PointerButton::Primary),
                i.pointer.button_released(egui::PointerButton::Primary),
            )
        });

        // Hit-test the two endpoint handles (r=8 screen px).
        if primary_pressed
            && over_canvas
            && let (Some(pts), Some(ptr)) = (self.straighten_line, ptr_pos)
        {
            let handle_r = 12.0_f32; // screen-space hit radius
            for (i, &ep_img) in pts.iter().enumerate() {
                let ep_screen = image_to_screen(ep_img, image_tl, self.zoom);
                if (ep_screen - ptr).length() < handle_r {
                    self.straighten_dragging = Some(i);
                    break;
                }
            }
        }

        if primary_down
            && let (Some(drag_idx), Some(ptr)) = (self.straighten_dragging, ptr_pos)
            && let Some(pts) = &mut self.straighten_line
        {
            pts[drag_idx] = screen_to_image(ptr, image_tl, self.zoom);
            // Recompute angle and update preview only while dragging.
            let [p0, p1] = *pts;
            let line_angle = (p1.y - p0.y).atan2(p1.x - p0.x).to_degrees();
            state.tools.find_mut::<StraightenTool>().unwrap().angle = -line_angle;
            state.update_straighten_preview();
        }

        if primary_released {
            self.straighten_dragging = None;
        }

        // ── Grid overlay ─────────────────────────────────────────────────
        let img_rect = Rect::from_min_size(image_tl, display_size);
        let clipped = painter.with_clip_rect(img_rect);
        for i in 1..5_u32 {
            let t = i as f32 / 5.0;
            let x = img_rect.min.x + img_rect.width() * t;
            let y = img_rect.min.y + img_rect.height() * t;
            clipped.line_segment(
                [Pos2::new(x, img_rect.min.y), Pos2::new(x, img_rect.max.y)],
                Stroke::new(0.5_f32, Color32::from_white_alpha(60)),
            );
            clipped.line_segment(
                [Pos2::new(img_rect.min.x, y), Pos2::new(img_rect.max.x, y)],
                Stroke::new(0.5_f32, Color32::from_white_alpha(60)),
            );
        }

        // ── Horizon line ─────────────────────────────────────────────────
        if let Some([p0, p1]) = self.straighten_line {
            let s0 = image_to_screen(p0, image_tl, self.zoom);
            let s1 = image_to_screen(p1, image_tl, self.zoom);
            // Shadow
            painter.line_segment(
                [s0, s1],
                Stroke::new(2.5_f32, Color32::from_black_alpha(100)),
            );
            // Main line
            painter.line_segment([s0, s1], Stroke::new(1.5_f32, Color32::WHITE));
            // Endpoints
            for &ep in &[s0, s1] {
                painter.circle_filled(ep, 6.0, Color32::from_black_alpha(100));
                painter.circle_stroke(ep, 6.0, Stroke::new(1.5_f32, Color32::WHITE));
            }
            // Angle label near midpoint
            let mid = Pos2::new((s0.x + s1.x) / 2.0, (s0.y + s1.y) / 2.0 - 14.0);
            let angle_text = format!(
                "{:.2}°",
                state.tools.find::<StraightenTool>().unwrap().angle
            );
            painter.text(
                mid,
                egui::Align2::CENTER_BOTTOM,
                &angle_text,
                egui::FontId::proportional(12.0),
                Color32::WHITE,
            );
        }
    }
}
