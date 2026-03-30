//! Central image viewer with zoom/pan and crop-selection overlay.

use std::time::Duration;

use egui::{Color32, ColorImage, Pos2, Rect, Stroke, TextureHandle, TextureOptions, Ui, Vec2};
use rasterlab_core::Image;

use crate::state::AppState;

/// Persistent state for the canvas panel.
pub struct CanvasState {
    pub zoom: f32,
    pan_offset: Vec2,
    texture: Option<TextureHandle>,
    /// Hash of the last image pointer+length — detects when pixel data changes.
    last_hash: u64,
    /// Generation counter from AppState — resets view on a new file open.
    last_generation: u64,
    /// Dimensions of the last rendered image — resets view when they change (crop, rotate 90/270).
    last_img_dims: (u32, u32),
    /// Canvas size on the previous frame — triggers a refit when the window is resized.
    last_canvas_size: Vec2,
    crop_start: Option<Pos2>,
    crop_end: Option<Pos2>,
    /// Overlay texture for full-resolution viewport previews.
    overlay_texture: Option<TextureHandle>,
    overlay_last_hash: u64,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan_offset: Vec2::ZERO,
            texture: None,
            last_hash: 0,
            last_generation: 0,
            last_img_dims: (0, 0),
            last_canvas_size: Vec2::ZERO,
            crop_start: None,
            crop_end: None,
            overlay_texture: None,
            overlay_last_hash: 0,
        }
    }
}

impl CanvasState {
    pub fn ui(&mut self, ui: &mut Ui, state: &mut AppState) {
        let Some(image) = state.rendered.as_ref() else {
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new("Open an image to begin")
                        .size(22.0)
                        .color(Color32::from_gray(120)),
                );
            });
            return;
        };

        let img_w = image.width;
        let img_h = image.height;

        // ── Reserve bottom strip for zoom controls before computing canvas rect ──
        // This must come first so the controls have real layout space.
        let available = ui.available_rect_before_wrap();
        let bar_height = 24.0;
        let canvas_size = Vec2::new(
            available.width(),
            (available.height() - bar_height).max(1.0),
        );
        let canvas_rect = Rect::from_min_size(available.min, canvas_size);

        // ── Rebuild GPU texture only when pixel data changes ─────────────────
        let new_hash = compute_hash(image);
        let img_gen = state.image_generation;

        if self.texture.is_none() || new_hash != self.last_hash {
            self.texture = Some(ui.ctx().load_texture(
                "canvas_image",
                image_to_egui(image),
                TextureOptions::LINEAR,
            ));
            self.last_hash = new_hash;
        }

        // Reset view when a new file is opened OR when dimensions change
        // (crop, rotate 90°/270°). Sharpen, B&W, rotate 180° etc. preserve zoom/pan.
        // Ignore dimension changes caused by downsampled preview renders — we
        // don't want to reset zoom/pan every time a 1/4-scale preview arrives.
        let dims_changed = (img_w, img_h) != self.last_img_dims && !state.rendered_is_preview;
        let canvas_resized =
            canvas_size != self.last_canvas_size && self.last_canvas_size != Vec2::ZERO;
        if img_gen != self.last_generation || dims_changed || canvas_resized {
            self.zoom = fit_zoom(img_w, img_h, canvas_size);
            self.pan_offset = Vec2::ZERO;
            self.crop_start = None;
            self.crop_end = None;
            self.last_generation = img_gen;
            self.last_img_dims = (img_w, img_h);
        }
        self.last_canvas_size = canvas_size;

        // ── Publish viewport for preview optimisation ─────────────────────
        // Viewport is always computed in full-res image coordinates (zoom is
        // always expressed as screen-pixels-per-full-res-pixel).
        {
            let rs = state.rendered_scale;
            // Full-res dimensions regardless of whether we're showing a preview.
            let full_w = (img_w as f32 / rs) as u32;
            let full_h = (img_h as f32 / rs) as u32;
            let vis_x0 = (-self.pan_offset.x / self.zoom).max(0.0) as u32;
            let vis_y0 = (-self.pan_offset.y / self.zoom).max(0.0) as u32;
            let vis_x1 =
                ((canvas_size.x - self.pan_offset.x) / self.zoom).min(full_w as f32) as u32;
            let vis_y1 =
                ((canvas_size.y - self.pan_offset.y) / self.zoom).min(full_h as f32) as u32;
            let vp_w = vis_x1.saturating_sub(vis_x0).max(1);
            let vp_h = vis_y1.saturating_sub(vis_y0).max(1);
            state.preview_viewport = Some([vis_x0, vis_y0, vp_w, vp_h]);
        }

        let tex = self.texture.as_ref().unwrap();
        // When the rendered image is a downsampled preview, scale up the zoom
        // so it fills the same screen area as the full-res image would.
        // self.zoom is always in full-res image pixels; rendered_scale converts.
        let effective_zoom = self.zoom / state.rendered_scale;
        let display_size = Vec2::new(img_w as f32 * effective_zoom, img_h as f32 * effective_zoom);

        let (resp, painter) = ui.allocate_painter(canvas_size, egui::Sense::click_and_drag());

        // ── Middle-mouse pan + Ctrl+scroll-wheel zoom ────────────────────────
        ui.input(|i| {
            if i.pointer.button_down(egui::PointerButton::Middle) {
                self.pan_offset += i.pointer.delta();
            }
            // raw_scroll_delta fires once per wheel notch; smooth_scroll_delta
            // spreads one event across many frames which shifts the pivot each
            // frame and causes the image to jump.
            let scroll = i.raw_scroll_delta.y;
            if scroll != 0.0 && i.modifiers.ctrl {
                let old_zoom = self.zoom;
                let factor = (1.0 + scroll * 0.003).clamp(0.8, 1.25);
                self.zoom = (self.zoom * factor).clamp(0.05, 32.0);
                let actual = self.zoom / old_zoom;
                if let Some(cursor) = i.pointer.hover_pos() {
                    let pivot = cursor - canvas_rect.min;
                    self.pan_offset = pivot - (pivot - self.pan_offset) * actual;
                }
            }
        });

        // ── Cursor icon ───────────────────────────────────────────────────────
        let (middle_down, ctrl_held, over_canvas) = ui.input(|i| {
            (
                i.pointer.button_down(egui::PointerButton::Middle),
                i.modifiers.ctrl,
                i.pointer
                    .hover_pos()
                    .map(|p| canvas_rect.contains(p))
                    .unwrap_or(false),
            )
        });
        if middle_down {
            ui.ctx().set_cursor_icon(egui::CursorIcon::AllScroll);
        } else if ctrl_held && over_canvas {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ZoomIn);
        }

        // ── Draw image ───────────────────────────────────────────────────────
        let image_tl = canvas_rect.min + self.pan_offset;
        painter.image(
            tex.id(),
            Rect::from_min_size(image_tl, display_size),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        // ── Overlay: full-resolution viewport preview ─────────────────────
        // Drawn on top of the base image — sharp because it's 1:1 pixels.
        if let Some(overlay_img) = &state.preview_overlay {
            let new_hash = compute_hash(overlay_img);
            if self.overlay_texture.is_none() || new_hash != self.overlay_last_hash {
                self.overlay_texture = Some(ui.ctx().load_texture(
                    "canvas_overlay",
                    image_to_egui(overlay_img),
                    TextureOptions::LINEAR,
                ));
                self.overlay_last_hash = new_hash;
            }
            if let (Some(ol_tex), Some([ol_x, ol_y, ol_w, ol_h])) =
                (&self.overlay_texture, &state.preview_overlay_rect)
            {
                let ol_tl =
                    image_tl + Vec2::new(*ol_x as f32 * self.zoom, *ol_y as f32 * self.zoom);
                let ol_size = Vec2::new(*ol_w as f32 * self.zoom, *ol_h as f32 * self.zoom);
                painter.image(
                    ol_tex.id(),
                    Rect::from_min_size(ol_tl, ol_size),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        } else {
            self.overlay_texture = None;
            self.overlay_last_hash = 0;
        }

        // ── Crop selection (primary drag only) ───────────────────────────────
        // crop_start/crop_end are stored in image coordinates so the selection
        // tracks correctly when the user pans or zooms after drawing it.
        if resp.drag_started_by(egui::PointerButton::Primary) {
            self.crop_start = resp
                .interact_pointer_pos()
                .map(|p| screen_to_image(p, image_tl, self.zoom));
            self.crop_end = self.crop_start;
        }
        if resp.dragged_by(egui::PointerButton::Primary) {
            self.crop_end = resp
                .interact_pointer_pos()
                .map(|p| screen_to_image(p, image_tl, self.zoom));
        }
        if resp.drag_stopped_by(egui::PointerButton::Primary)
            && let (Some(start), Some(end)) = (self.crop_start, self.crop_end)
        {
            let (x, y, w, h) = image_to_crop(start, end, img_w, img_h);
            state.crop_x = x;
            state.crop_y = y;
            state.crop_w = w;
            state.crop_h = h;
        }

        // ── Clear selection: right-click or Escape ───────────────────────────
        if resp.secondary_clicked() {
            self.crop_start = None;
            self.crop_end = None;
        }
        ui.input(|i| {
            if i.key_pressed(egui::Key::Escape) {
                self.crop_start = None;
                self.crop_end = None;
            }
        });

        // ── Marching-ants overlay ─────────────────────────────────────────────
        if let (Some(start), Some(end)) = (self.crop_start, self.crop_end) {
            // Convert image-space coords back to screen space for drawing.
            let sel = Rect::from_two_pos(
                image_to_screen(start, image_tl, self.zoom),
                image_to_screen(end, image_tl, self.zoom),
            );
            if sel.width() > 2.0 && sel.height() > 2.0 {
                let time = ui.input(|i| i.time) as f32;
                draw_marching_ants(&painter, sel, time);
                // Cap animation repaints at ~60 fps — uncapped repaints can
                // overwhelm the compositor and cause flickering.
                ui.ctx().request_repaint_after(Duration::from_millis(16));
            }
        }

        // ── Zoom controls (bottom strip) ─────────────────────────────────────
        ui.horizontal(|ui| {
            if ui.small_button("−").clicked() {
                self.zoom = (self.zoom * 0.8).max(0.05);
            }
            ui.label(format!("{:.0}%", self.zoom * 100.0));
            if ui.small_button("+").clicked() {
                self.zoom = (self.zoom * 1.25).min(32.0);
            }
            if ui.small_button("Fit").clicked() {
                self.zoom = fit_zoom(img_w, img_h, canvas_size);
                self.pan_offset = Vec2::ZERO;
            }
            if ui.small_button("1:1").clicked() {
                self.zoom = 1.0;
                self.pan_offset = Vec2::ZERO;
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Coordinate helpers
// ---------------------------------------------------------------------------

/// Convert a screen position to image-space coordinates.
fn screen_to_image(pos: Pos2, image_tl: Pos2, zoom: f32) -> Pos2 {
    Pos2::new((pos.x - image_tl.x) / zoom, (pos.y - image_tl.y) / zoom)
}

/// Convert an image-space position to screen coordinates.
fn image_to_screen(pos: Pos2, image_tl: Pos2, zoom: f32) -> Pos2 {
    Pos2::new(pos.x * zoom + image_tl.x, pos.y * zoom + image_tl.y)
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

// ---------------------------------------------------------------------------
// Marching-ants drawing
// ---------------------------------------------------------------------------

fn draw_marching_ants(painter: &egui::Painter, rect: Rect, time: f32) {
    const DASH: f32 = 8.0;
    const GAP: f32 = 4.0;
    const SPEED: f32 = 15.0;

    let offset = (time * SPEED).rem_euclid(DASH + GAP);
    painter.rect_stroke(rect, 0.0, Stroke::new(2.0, Color32::WHITE));

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
            Stroke::new(1.0, Color32::BLACK),
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

// ---------------------------------------------------------------------------
// Texture / hash helpers
// ---------------------------------------------------------------------------

fn image_to_egui(image: &Image) -> ColorImage {
    use rayon::prelude::*;
    let pixels: Vec<Color32> = image
        .data
        .par_chunks_exact(4)
        .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
        .collect();
    ColorImage {
        size: [image.width as usize, image.height as usize],
        pixels,
    }
}

fn fit_zoom(img_w: u32, img_h: u32, available: Vec2) -> f32 {
    (available.x / img_w as f32)
        .min(available.y / img_h as f32)
        .max(0.05)
}

fn compute_hash(image: &Image) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // Sample every 128th byte — fast, but catches any pixel change.
    image.data.len().hash(&mut h);
    for byte in image.data.iter().step_by(128) {
        byte.hash(&mut h);
    }
    h.finish()
}
