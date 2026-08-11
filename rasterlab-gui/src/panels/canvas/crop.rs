//! Crop selection: handle hit-testing, drag geometry, aspect constraints, and
//! the marching-ants overlay.

use std::time::Duration;

use egui::{Color32, Pos2, Rect, Stroke, Ui, Vec2};

use crate::panels::tools::crop::CropTool;
use crate::state::AppState;

use super::coords::{image_to_screen, screen_to_image};
use super::{CanvasState, CanvasView};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CropDragMode {
    Creating,
    Moving,
    ResizeN,
    ResizeS,
    ResizeE,
    ResizeW,
    ResizeNW,
    ResizeNE,
    ResizeSW,
    ResizeSE,
}

/// Radius (in screen pixels) used both to draw a handle and to hit-test it.
const HANDLE_R: f32 = 6.0;

impl CanvasState {
    /// Run the crop selection interaction and draw its overlay.
    ///
    /// This is the fallback on-canvas behaviour: it owns the primary drag when
    /// no mask, heal, or straighten tool has claimed it.
    pub(super) fn handle_crop(
        &mut self,
        ui: &mut Ui,
        resp: &egui::Response,
        painter: &egui::Painter,
        state: &mut AppState,
        view: &CanvasView,
    ) {
        let CanvasView {
            image_tl,
            img_w,
            img_h,
            ..
        } = *view;

        // Screen-space rect of the current selection, if it's large enough
        // to show handles on.  Used for both hit-testing and drawing.
        let current_rect_screen = self.crop_start.zip(self.crop_end).and_then(|(a, b)| {
            let r = Rect::from_two_pos(
                image_to_screen(a, image_tl, self.zoom),
                image_to_screen(b, image_tl, self.zoom),
            );
            (r.width() > 2.0 && r.height() > 2.0).then_some(r)
        });

        // Hover cursor hint when not actively dragging.
        if self.crop_drag.is_none()
            && resp.hovered()
            && let Some(rs) = current_rect_screen
            && let Some(ptr) = resp.hover_pos()
            && let Some(mode) = hit_test_handle(rs, ptr)
        {
            ui.ctx().set_cursor_icon(cursor_for_mode(mode));
        }

        if resp.drag_started_by(egui::PointerButton::Primary)
            && let Some(p_screen) = resp.interact_pointer_pos()
        {
            let ptr_img = screen_to_image(p_screen, image_tl, self.zoom);
            let mode = current_rect_screen
                .and_then(|rs| hit_test_handle(rs, p_screen))
                .filter(|_| self.crop_start.is_some() && self.crop_end.is_some());
            match mode {
                Some(m) => {
                    self.crop_drag = Some(m);
                    self.crop_drag_start_ptr = ptr_img;
                    // crop_start/crop_end are guaranteed Some by the filter above.
                    let (a, b) = (self.crop_start.unwrap(), self.crop_end.unwrap());
                    self.crop_drag_start_rect = Rect::from_two_pos(a, b);
                }
                None => {
                    // Empty area → start a fresh selection.
                    self.crop_drag = Some(CropDragMode::Creating);
                    self.crop_start = Some(ptr_img);
                    self.crop_end = Some(ptr_img);
                }
            }
        }

        if resp.dragged_by(egui::PointerButton::Primary)
            && let Some(p_screen) = resp.interact_pointer_pos()
            && let Some(mode) = self.crop_drag
        {
            let ptr_img = screen_to_image(p_screen, image_tl, self.zoom);
            match mode {
                CropDragMode::Creating => {
                    let constrained = constrain_drag_end(
                        self.crop_start.unwrap_or(ptr_img),
                        ptr_img,
                        state.tools.crop_aspect_ratio(),
                    );
                    self.crop_end = Some(constrained);
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
                }
                CropDragMode::Moving => {
                    let delta = ptr_img - self.crop_drag_start_ptr;
                    let start_rect = self.crop_drag_start_rect;
                    let w = start_rect.width();
                    let h = start_rect.height();
                    let max_x = (img_w as f32 - w).max(0.0);
                    let max_y = (img_h as f32 - h).max(0.0);
                    let nx = (start_rect.min.x + delta.x).clamp(0.0, max_x);
                    let ny = (start_rect.min.y + delta.y).clamp(0.0, max_y);
                    self.crop_start = Some(Pos2::new(nx, ny));
                    self.crop_end = Some(Pos2::new(nx + w, ny + h));
                    ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
                }
                _ => {
                    let start_rect = self.crop_drag_start_rect;
                    let ratio = state.tools.crop_aspect_ratio();
                    let ptr_c = Pos2::new(
                        ptr_img.x.clamp(0.0, img_w as f32),
                        ptr_img.y.clamp(0.0, img_h as f32),
                    );
                    let (min, max) = resize_rect(mode, start_rect, ptr_c, ratio, img_w, img_h);
                    self.crop_start = Some(min);
                    self.crop_end = Some(max);
                    ui.ctx().set_cursor_icon(cursor_for_mode(mode));
                }
            }
        }

        if resp.drag_stopped_by(egui::PointerButton::Primary)
            && let (Some(start), Some(end)) = (self.crop_start, self.crop_end)
        {
            let (x, y, w, h) = image_to_crop(start, end, img_w, img_h);
            // Aspect constraint only applies to newly drawn selections.
            // Editing an existing rect (move/resize) should respect the
            // user's explicit handle placement.
            let (cx, cy, cw, ch) = match self.crop_drag {
                Some(CropDragMode::Creating) => {
                    constrain_aspect(x, y, w, h, img_w, img_h, state.tools.crop_aspect_ratio())
                }
                _ => (x, y, w, h),
            };
            let crop = state.tools.find_mut::<CropTool>().unwrap();
            crop.x = cx;
            crop.y = cy;
            crop.w = cw;
            crop.h = ch;
            // Snap the on-canvas rect to the clamped integer coords so a
            // subsequent drag starts from the committed rectangle.
            self.crop_start = Some(Pos2::new(cx as f32, cy as f32));
            self.crop_end = Some(Pos2::new((cx + cw) as f32, (cy + ch) as f32));
            self.crop_drag = None;
        }

        // ── Clear selection: right-click or Escape ───────────────────────
        if resp.secondary_clicked() {
            self.clear_crop_selection(state);
        }
        let escape_pressed = ui.input(|i| i.key_pressed(egui::Key::Escape));
        if escape_pressed {
            self.clear_crop_selection(state);
        }

        // ── Marching-ants overlay ─────────────────────────────────────────
        if let (Some(start), Some(end)) = (self.crop_start, self.crop_end) {
            let sel = Rect::from_two_pos(
                image_to_screen(start, image_tl, self.zoom),
                image_to_screen(end, image_tl, self.zoom),
            );
            if sel.width() > 2.0 && sel.height() > 2.0 {
                let time = ui.input(|i| i.time) as f32;
                draw_marching_ants(painter, sel, time);
                draw_crop_handles(painter, sel);
                ui.ctx().request_repaint_after(Duration::from_millis(16));
            }
        }
    }

    /// Drop the on-canvas selection and leave the fixed 2:1 sprocket workflow.
    fn clear_crop_selection(&mut self, state: &mut AppState) {
        self.crop_start = None;
        self.crop_end = None;
        self.crop_drag = None;
        state.tools.sprocket_crop_active = false;
    }
}

/// Hit-test a pointer against the crop rect handles. Corners take priority over
/// edges, edges over the interior, so users can reliably grab a corner even
/// when it's flush against the rect body.
fn hit_test_handle(rect: Rect, ptr: Pos2) -> Option<CropDragMode> {
    let r = HANDLE_R + 2.0;
    let near = |p: Pos2| (p - ptr).length() <= r;
    if near(rect.left_top()) {
        return Some(CropDragMode::ResizeNW);
    }
    if near(rect.right_top()) {
        return Some(CropDragMode::ResizeNE);
    }
    if near(rect.left_bottom()) {
        return Some(CropDragMode::ResizeSW);
    }
    if near(rect.right_bottom()) {
        return Some(CropDragMode::ResizeSE);
    }
    if near(Pos2::new(rect.center().x, rect.top())) {
        return Some(CropDragMode::ResizeN);
    }
    if near(Pos2::new(rect.center().x, rect.bottom())) {
        return Some(CropDragMode::ResizeS);
    }
    if near(Pos2::new(rect.left(), rect.center().y)) {
        return Some(CropDragMode::ResizeW);
    }
    if near(Pos2::new(rect.right(), rect.center().y)) {
        return Some(CropDragMode::ResizeE);
    }
    if rect.contains(ptr) {
        return Some(CropDragMode::Moving);
    }
    None
}

fn cursor_for_mode(mode: CropDragMode) -> egui::CursorIcon {
    match mode {
        CropDragMode::Creating => egui::CursorIcon::Crosshair,
        CropDragMode::Moving => egui::CursorIcon::Move,
        CropDragMode::ResizeN | CropDragMode::ResizeS => egui::CursorIcon::ResizeVertical,
        CropDragMode::ResizeE | CropDragMode::ResizeW => egui::CursorIcon::ResizeHorizontal,
        CropDragMode::ResizeNW | CropDragMode::ResizeSE => egui::CursorIcon::ResizeNwSe,
        CropDragMode::ResizeNE | CropDragMode::ResizeSW => egui::CursorIcon::ResizeNeSw,
    }
}

/// Compute the new crop rect for a resize drag.
///
/// For corner handles the opposite corner is the anchor.  For edge handles the
/// opposite edge is the anchor and the perpendicular dimension grows/shrinks
/// symmetrically around the start rect's centre on that axis.
///
/// When a ratio is locked, the rect is shrunk uniformly if aspect-preserving
/// growth would exceed the image bounds — so the aspect is honoured even at
/// the edges.
fn resize_rect(
    mode: CropDragMode,
    start_rect: Rect,
    ptr: Pos2,
    ratio: Option<(f32, f32)>,
    img_w: u32,
    img_h: u32,
) -> (Pos2, Pos2) {
    let iw = img_w as f32;
    let ih = img_h as f32;

    // Helper for corner drags: anchor stays fixed, opposite corner follows the
    // pointer.  Width leads (matching the Creating behaviour) and height is
    // derived from the aspect ratio, then both are clamped to image bounds —
    // height may force width to shrink if it's the binding constraint.
    let corner = |anchor: Pos2| -> (Pos2, Pos2) {
        let sign_x = if ptr.x >= anchor.x { 1.0 } else { -1.0 };
        let sign_y = if ptr.y >= anchor.y { 1.0 } else { -1.0 };
        let max_w = if sign_x > 0.0 {
            iw - anchor.x
        } else {
            anchor.x
        };
        let max_h = if sign_y > 0.0 {
            ih - anchor.y
        } else {
            anchor.y
        };
        let (w, h) = match ratio {
            Some((rw, rh)) => {
                let want_w = (ptr.x - anchor.x).abs().min(max_w);
                let h_from_w = want_w * rh / rw;
                if h_from_w <= max_h {
                    (want_w, h_from_w)
                } else {
                    (max_h * rw / rh, max_h)
                }
            }
            None => (
                (ptr.x - anchor.x).abs().min(max_w),
                (ptr.y - anchor.y).abs().min(max_h),
            ),
        };
        let other = Pos2::new(anchor.x + sign_x * w, anchor.y + sign_y * h);
        let min = Pos2::new(anchor.x.min(other.x), anchor.y.min(other.y));
        let max = Pos2::new(anchor.x.max(other.x), anchor.y.max(other.y));
        (min, max)
    };

    // Helper for edge drags on a horizontal edge (N or S): the opposite edge
    // is the anchor (y_anchor), width grows/shrinks around the start rect's
    // horizontal centre.
    let horiz_edge = |y_anchor: f32, cx: f32| -> (Pos2, Pos2) {
        let sign = if ptr.y >= y_anchor { 1.0 } else { -1.0 };
        let max_h = if sign > 0.0 { ih - y_anchor } else { y_anchor };
        let want_h = (ptr.y - y_anchor).abs().min(max_h);
        let (w, h) = match ratio {
            Some((rw, rh)) => {
                let max_w = 2.0 * cx.min(iw - cx);
                let want_w = (want_h * rw / rh).min(max_w);
                let final_h = want_w * rh / rw;
                (want_w, final_h)
            }
            None => (start_rect.width(), want_h),
        };
        let y_other = y_anchor + sign * h;
        let min_y = y_anchor.min(y_other);
        let max_y = y_anchor.max(y_other);
        let min_x = (cx - w / 2.0).max(0.0);
        let max_x = (cx + w / 2.0).min(iw);
        (Pos2::new(min_x, min_y), Pos2::new(max_x, max_y))
    };

    // Helper for edge drags on a vertical edge (E or W).
    let vert_edge = |x_anchor: f32, cy: f32| -> (Pos2, Pos2) {
        let sign = if ptr.x >= x_anchor { 1.0 } else { -1.0 };
        let max_w = if sign > 0.0 { iw - x_anchor } else { x_anchor };
        let want_w = (ptr.x - x_anchor).abs().min(max_w);
        let (w, h) = match ratio {
            Some((rw, rh)) => {
                let max_h = 2.0 * cy.min(ih - cy);
                let want_h = (want_w * rh / rw).min(max_h);
                let final_w = want_h * rw / rh;
                (final_w, want_h)
            }
            None => (want_w, start_rect.height()),
        };
        let x_other = x_anchor + sign * w;
        let min_x = x_anchor.min(x_other);
        let max_x = x_anchor.max(x_other);
        let min_y = (cy - h / 2.0).max(0.0);
        let max_y = (cy + h / 2.0).min(ih);
        (Pos2::new(min_x, min_y), Pos2::new(max_x, max_y))
    };

    match mode {
        CropDragMode::ResizeNW => corner(start_rect.max),
        CropDragMode::ResizeNE => corner(Pos2::new(start_rect.min.x, start_rect.max.y)),
        CropDragMode::ResizeSW => corner(Pos2::new(start_rect.max.x, start_rect.min.y)),
        CropDragMode::ResizeSE => corner(start_rect.min),
        CropDragMode::ResizeN => horiz_edge(start_rect.max.y, start_rect.center().x),
        CropDragMode::ResizeS => horiz_edge(start_rect.min.y, start_rect.center().x),
        CropDragMode::ResizeE => vert_edge(start_rect.min.x, start_rect.center().y),
        CropDragMode::ResizeW => vert_edge(start_rect.max.x, start_rect.center().y),
        _ => (start_rect.min, start_rect.max),
    }
}

fn draw_crop_handles(painter: &egui::Painter, rect: Rect) {
    let points = [
        rect.left_top(),
        rect.right_top(),
        rect.left_bottom(),
        rect.right_bottom(),
        Pos2::new(rect.center().x, rect.top()),
        Pos2::new(rect.center().x, rect.bottom()),
        Pos2::new(rect.left(), rect.center().y),
        Pos2::new(rect.right(), rect.center().y),
    ];
    for p in points {
        let handle = Rect::from_center_size(p, Vec2::splat(HANDLE_R * 2.0));
        painter.rect_filled(handle, 1.0, Color32::WHITE);
        painter.rect_stroke(
            handle,
            1.0,
            Stroke::new(1.0_f32, Color32::from_black_alpha(200)),
            egui::StrokeKind::Middle,
        );
    }
}

/// Convert two image-space corner points into a clamped crop rectangle (x, y, w, h).
fn image_to_crop(start: Pos2, end: Pos2, img_w: u32, img_h: u32) -> (u32, u32, u32, u32) {
    let min = start.min(end);
    let max = start.max(end);
    let x1 = (min.x.max(0.0) as u32).min(img_w);
    let y1 = (min.y.max(0.0) as u32).min(img_h);
    let x2 = (max.x.max(0.0) as u32).min(img_w);
    let y2 = (max.y.max(0.0) as u32).min(img_h);
    (
        x1,
        y1,
        x2.saturating_sub(x1).max(1),
        y2.saturating_sub(y1).max(1),
    )
}

/// Clamp a crop rect to an optional aspect ratio (w/h = ratio.0/ratio.1).
/// Adjusts height to match the width (keeping x,y fixed), then clamps to image bounds.
fn constrain_aspect(
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    img_w: u32,
    img_h: u32,
    ratio: Option<(f32, f32)>,
) -> (u32, u32, u32, u32) {
    let Some((rw, rh)) = ratio else {
        return (x, y, w, h);
    };
    let target_h = ((w as f32 * rh / rw).round() as u32).max(1);
    let ch = target_h.min(img_h.saturating_sub(y)).max(1);
    let cw = ((ch as f32 * rw / rh).round() as u32)
        .min(img_w.saturating_sub(x))
        .max(1);
    (x, y, cw, ch)
}

/// When an aspect ratio is locked, adjust the drag end point so the selection
/// matches the ratio.  Always fixes width and adjusts height.
fn constrain_drag_end(start: Pos2, end: Pos2, ratio: Option<(f32, f32)>) -> Pos2 {
    let Some((rw, rh)) = ratio else {
        return end;
    };
    let w = (end.x - start.x).abs();
    let h_target = w * rh / rw;
    let sign_y = if end.y >= start.y { 1.0 } else { -1.0 };
    Pos2::new(end.x, start.y + sign_y * h_target)
}

// ---------------------------------------------------------------------------
// Marching-ants drawing
// ---------------------------------------------------------------------------

fn draw_marching_ants(painter: &egui::Painter, rect: Rect, time: f32) {
    const DASH: f32 = 8.0;
    const GAP: f32 = 4.0;
    const SPEED: f32 = 15.0;

    let offset = (time * SPEED).rem_euclid(DASH + GAP);
    painter.rect_stroke(
        rect,
        0.0,
        Stroke::new(2.0_f32, Color32::WHITE),
        egui::StrokeKind::Middle,
    );

    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    for i in 0..4 {
        dashed_segment(
            painter,
            corners[i],
            corners[(i + 1) % 4],
            Stroke::new(1.0_f32, Color32::BLACK),
            DASH,
            GAP,
            offset,
        );
    }
}

fn dashed_segment(
    painter: &egui::Painter,
    a: Pos2,
    b: Pos2,
    stroke: Stroke,
    dash: f32,
    gap: f32,
    offset: f32,
) {
    let total = (b - a).length();
    if total < 0.5 {
        return;
    }
    let dir = (b - a) / total;
    let period = dash + gap;
    let mut t = -(offset.rem_euclid(period));
    while t < total {
        let s = t.max(0.0);
        let e = (t + dash).min(total);
        if s < e {
            painter.line_segment([a + dir * s, a + dir * e], stroke);
        }
        t += period;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constrain_aspect_free_passthrough() {
        assert_eq!(
            constrain_aspect(0, 0, 100, 80, 1000, 1000, None),
            (0, 0, 100, 80)
        );
    }

    #[test]
    fn constrain_aspect_square() {
        let (x, y, w, h) = constrain_aspect(0, 0, 100, 80, 1000, 1000, Some((1.0, 1.0)));
        assert_eq!(w, h);
        assert_eq!(x, 0);
        assert_eq!(y, 0);
    }

    #[test]
    fn constrain_aspect_3_2() {
        let (_, _, w, h) = constrain_aspect(0, 0, 120, 60, 1000, 1000, Some((3.0, 2.0)));
        // w/h should be close to 1.5
        let ratio = w as f32 / h as f32;
        assert!((ratio - 1.5).abs() < 0.05, "ratio={ratio}");
    }

    #[test]
    fn constrain_drag_end_free() {
        let start = Pos2::new(10.0, 10.0);
        let end = Pos2::new(50.0, 70.0);
        assert_eq!(constrain_drag_end(start, end, None), end);
    }

    #[test]
    fn constrain_drag_end_square() {
        let start = Pos2::new(0.0, 0.0);
        let end = Pos2::new(100.0, 50.0);
        let constrained = constrain_drag_end(start, end, Some((1.0, 1.0)));
        // With 1:1 ratio, height should equal width = 100
        assert!((constrained.y - 100.0).abs() < 1.0, "y={}", constrained.y);
    }
}
