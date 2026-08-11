//! Interactive gradient-mask placement, handle drawing, and the translucent
//! mask preview overlay.

use egui::{Color32, ColorImage, Pos2, Rect, Stroke, TextureOptions, Ui, Vec2};

use crate::state::AppState;

use super::coords::{norm_to_screen, screen_to_norm};
use super::{CanvasState, CanvasView};

/// Mask selector values used by the tools panel.
const MASK_LINEAR: usize = 1;
const MASK_RADIAL: usize = 2;

impl CanvasState {
    /// Drag-to-place the active gradient mask and draw its handles.
    pub(super) fn handle_mask(
        &mut self,
        ui: &mut Ui,
        painter: &egui::Painter,
        state: &mut AppState,
        view: &CanvasView,
    ) {
        let CanvasView {
            canvas_rect,
            image_tl,
            display_size,
            over_canvas,
            ..
        } = *view;

        // Clear any stale crop selection while mask mode is active.
        self.crop_start = None;
        self.crop_end = None;

        let (ptr_pos, primary_pressed, primary_down) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_down(egui::PointerButton::Primary),
            )
        });

        if primary_pressed
            && over_canvas
            && let Some(p) = ptr_pos
        {
            self.mask_drag_start = Some(screen_to_norm(p, image_tl, display_size));
        }
        if primary_down {
            if let (Some(start), Some(p)) = (self.mask_drag_start, ptr_pos) {
                let end = screen_to_norm(p, image_tl, display_size);
                match state.tools.mask_sel {
                    MASK_LINEAR => update_linear_mask(state, start, end),
                    MASK_RADIAL => update_radial_mask(state, start, end),
                    _ => {}
                }
            }
        } else {
            self.mask_drag_start = None;
        }

        match state.tools.mask_sel {
            MASK_LINEAR => {
                draw_linear_mask_handles(painter, state, image_tl, display_size, canvas_rect)
            }
            MASK_RADIAL => {
                draw_radial_mask_handles(painter, state, image_tl, display_size, canvas_rect)
            }
            _ => {}
        }
    }

    /// Draw the translucent mask preview over the image area.
    ///
    /// Rendered at 256×256 and scaled to the image area so the user can see
    /// where the next masked Apply will take effect. Releases the texture when
    /// no mask is selected.
    pub(super) fn draw_mask_overlay(&mut self, ui: &Ui, state: &AppState, view: &CanvasView) {
        if state.tools.mask_sel == 0 {
            self.mask_overlay_texture = None;
            self.mask_overlay_hash = 0;
            return;
        }

        let hash = mask_params_hash(state);
        if self.mask_overlay_texture.is_none() || hash != self.mask_overlay_hash {
            self.mask_overlay_texture = Some(ui.ctx().load_texture(
                "mask_overlay",
                build_mask_preview(state, 256, 256),
                TextureOptions::LINEAR,
            ));
            self.mask_overlay_hash = hash;
        }
        let Some(texture) = &self.mask_overlay_texture else {
            return;
        };

        // The overlay covers the image area in screen space.
        let scale = state.rendered_scale;
        let full_w = view.img_w as f32 / scale;
        let full_h = view.img_h as f32 / scale;
        let overlay_rect = Rect::from_min_size(
            view.image_tl,
            Vec2::new(full_w * self.zoom, full_h * self.zoom),
        );
        // Use a clipped painter so it stays inside the canvas area.
        ui.painter().with_clip_rect(view.canvas_rect).image(
            texture.id(),
            overlay_rect,
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }
}

/// Update linear mask from a drag: start is the "0% effect" end,
/// end is the "100% effect" end.  Center, angle, and feather are derived.
fn update_linear_mask(state: &mut AppState, start: Pos2, end: Pos2) {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-4 {
        return; // Too short — skip to avoid a degenerate angle.
    }
    state.tools.mask_lin_cx = (start.x + end.x) * 0.5;
    state.tools.mask_lin_cy = (start.y + end.y) * 0.5;
    state.tools.mask_lin_angle = dy.atan2(dx).to_degrees();
    state.tools.mask_lin_feather = len;
}

/// Update radial mask from a drag: start is the centre, end defines the radius.
fn update_radial_mask(state: &mut AppState, start: Pos2, end: Pos2) {
    let dx = end.x - start.x;
    let dy = end.y - start.y;
    state.tools.mask_rad_cx = start.x;
    state.tools.mask_rad_cy = start.y;
    state.tools.mask_rad_radius = (dx * dx + dy * dy).sqrt();
}

/// Draw handles showing the current linear gradient mask extent.
fn draw_linear_mask_handles(
    painter: &egui::Painter,
    state: &AppState,
    image_tl: Pos2,
    display_size: Vec2,
    canvas_rect: Rect,
) {
    let painter = painter.with_clip_rect(canvas_rect);
    let rad = state.tools.mask_lin_angle.to_radians();
    let (cos_a, sin_a) = (rad.cos(), rad.sin());
    let half = state.tools.mask_lin_feather * 0.5;

    let center = Pos2::new(state.tools.mask_lin_cx, state.tools.mask_lin_cy);
    let a_norm = Pos2::new(center.x - cos_a * half, center.y - sin_a * half);
    let b_norm = Pos2::new(center.x + cos_a * half, center.y + sin_a * half);

    let center_s = norm_to_screen(center, image_tl, display_size);
    let a_s = norm_to_screen(a_norm, image_tl, display_size);
    let b_s = norm_to_screen(b_norm, image_tl, display_size);

    let shadow = Stroke::new(3.0_f32, Color32::from_black_alpha(160));
    let white = Stroke::new(1.5_f32, Color32::WHITE);

    painter.line_segment([a_s, b_s], shadow);
    painter.line_segment([a_s, b_s], white);

    for &pt in &[a_s, center_s, b_s] {
        painter.circle_filled(pt, 6.0, Color32::from_black_alpha(160));
        painter.circle_stroke(pt, 6.0, Stroke::new(1.5_f32, Color32::WHITE));
    }
}

/// Draw handles showing the current radial gradient mask extent.
fn draw_radial_mask_handles(
    painter: &egui::Painter,
    state: &AppState,
    image_tl: Pos2,
    display_size: Vec2,
    canvas_rect: Rect,
) {
    let painter = painter.with_clip_rect(canvas_rect);
    let center_norm = Pos2::new(state.tools.mask_rad_cx, state.tools.mask_rad_cy);
    let center_s = norm_to_screen(center_norm, image_tl, display_size);

    // Convert radius from normalised space to screen pixels per axis.
    let rx = state.tools.mask_rad_radius * display_size.x;
    let ry = state.tools.mask_rad_radius * display_size.y;

    draw_ellipse_stroke(
        &painter,
        center_s,
        rx,
        ry,
        Stroke::new(3.0_f32, Color32::from_black_alpha(160)),
    );
    draw_ellipse_stroke(
        &painter,
        center_s,
        rx,
        ry,
        Stroke::new(1.5_f32, Color32::WHITE),
    );

    // Crosshair at centre.
    let arm = 8.0_f32;
    painter.line_segment(
        [
            center_s - Vec2::new(arm, 0.0),
            center_s + Vec2::new(arm, 0.0),
        ],
        Stroke::new(1.5_f32, Color32::WHITE),
    );
    painter.line_segment(
        [
            center_s - Vec2::new(0.0, arm),
            center_s + Vec2::new(0.0, arm),
        ],
        Stroke::new(1.5_f32, Color32::WHITE),
    );
    painter.circle_filled(center_s, 4.0, Color32::from_black_alpha(160));
    painter.circle_stroke(center_s, 4.0, Stroke::new(1.5_f32, Color32::WHITE));
}

/// Approximate an ellipse with line segments.
fn draw_ellipse_stroke(painter: &egui::Painter, center: Pos2, rx: f32, ry: f32, stroke: Stroke) {
    const N: usize = 48;
    let pts: Vec<Pos2> = (0..=N)
        .map(|i| {
            let a = i as f32 * 2.0 * std::f32::consts::PI / N as f32;
            Pos2::new(center.x + rx * a.cos(), center.y + ry * a.sin())
        })
        .collect();
    for w in pts.windows(2) {
        painter.line_segment([w[0], w[1]], stroke);
    }
}

/// Hash the current mask parameters so the overlay texture is only rebuilt
/// when something actually changes.
fn mask_params_hash(state: &AppState) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    state.tools.mask_sel.hash(&mut h);
    // Hash float bits — NaN-safe for UI values.
    state.tools.mask_lin_cx.to_bits().hash(&mut h);
    state.tools.mask_lin_cy.to_bits().hash(&mut h);
    state.tools.mask_lin_angle.to_bits().hash(&mut h);
    state.tools.mask_lin_feather.to_bits().hash(&mut h);
    state.tools.mask_lin_invert.hash(&mut h);
    state.tools.mask_rad_cx.to_bits().hash(&mut h);
    state.tools.mask_rad_cy.to_bits().hash(&mut h);
    state.tools.mask_rad_radius.to_bits().hash(&mut h);
    state.tools.mask_rad_feather.to_bits().hash(&mut h);
    state.tools.mask_rad_invert.hash(&mut h);
    h.finish()
}

/// Build a small `ColorImage` that visualises the current mask as a
/// semi-transparent blue overlay.  Opacity of each pixel = mask opacity.
fn build_mask_preview(state: &AppState, w: usize, h: usize) -> ColorImage {
    let shape = match state.tools.current_mask_shape() {
        Some(s) => s,
        None => return ColorImage::new([w, h], vec![Color32::TRANSPARENT; w * h]),
    };
    let mut pixels = Vec::with_capacity(w * h);
    for y in 0..h {
        let ny = (y as f32 + 0.5) / h as f32;
        for x in 0..w {
            let nx = (x as f32 + 0.5) / w as f32;
            let opacity = shape.eval(nx, ny);
            let alpha = (opacity * 140.0) as u8;
            pixels.push(Color32::from_rgba_unmultiplied(30, 90, 255, alpha));
        }
    }
    ColorImage {
        size: [w, h],
        pixels,
        source_size: egui::Vec2::new(w as f32, h as f32),
    }
}
