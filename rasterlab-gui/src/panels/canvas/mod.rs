//! Central image viewer with zoom/pan and on-canvas tool overlays.
//!
//! This module owns the frame: view state, the toolbar and zoom strip, the
//! presentation texture for the pipeline output, and the pointer plumbing.
//! Each on-canvas behaviour lives in its own sibling module:
//!
//! - [`crop`] — selection rectangle, handles, marching ants
//! - [`mask`] — gradient-mask placement and preview overlay
//! - [`heal`] — spot-heal placement and dragging
//! - [`straighten`] — horizon line and grid
//! - [`split`] — before/after comparison and its divider
//! - [`texture`] — uploading images at their on-screen size
//! - [`coords`] — screen ↔ image ↔ normalised conversions

mod coords;
mod crop;
mod heal;
mod mask;
mod split;
mod straighten;
mod texture;

use std::sync::Arc;

use egui::{Color32, Pos2, Rect, Stroke, TextureHandle, Ui, Vec2};

use crate::panels::tools::crop::CropTool;
use crate::panels::tools::heal::HealTool;
use crate::panels::tools::perspective::PerspectiveTool;
use crate::panels::tools::straighten::StraightenTool;
use crate::state::{AppState, SplitMode};

use self::crop::CropDragMode;
use self::texture::{ContentId, PresentationTexture};

/// Geometry and pointer context shared by the on-canvas overlays, so each tool
/// module sees the same view without threading a dozen arguments through.
struct CanvasView {
    canvas_rect: Rect,
    /// Screen position of the image's top-left corner.
    image_tl: Pos2,
    /// On-screen size of the image, in logical points.
    display_size: Vec2,
    /// Dimensions of the rendered image (may be a downsampled preview).
    img_w: u32,
    img_h: u32,
    over_canvas: bool,
    /// Middle or right button held — the pan gesture.
    middle_down: bool,
    ctrl_held: bool,
}

/// Persistent state for the canvas panel.
pub struct CanvasState {
    pub zoom: f32,
    pan_offset: Vec2,
    /// The pipeline output currently on screen.
    texture: PresentationTexture,
    /// Generation counter from AppState — resets view on a new file open.
    last_generation: u64,
    /// Dimensions of the last rendered image — resets view when they change (crop, rotate 90/270).
    last_img_dims: (u32, u32),
    /// Canvas size on the previous frame — triggers a refit when the window is resized.
    last_canvas_size: Vec2,
    crop_start: Option<Pos2>,
    crop_end: Option<Pos2>,
    /// Tracks entry/exit from the Looks panel's fixed 2:1 crop workflow.
    sprocket_crop_was_active: bool,
    /// Active drag mode for the crop selection (create, move, or resize a handle).
    crop_drag: Option<CropDragMode>,
    /// Image-space pointer position captured when `crop_drag` started — used to
    /// translate the rect during Moving without accumulating drift.
    crop_drag_start_ptr: Pos2,
    /// Rect at the moment `crop_drag` started (image-space corners).
    crop_drag_start_rect: Rect,
    /// Overlay texture for full-resolution viewport previews.
    overlay_texture: PresentationTexture,
    /// "Before" texture for split view — source image with geometric ops applied.
    before_texture: PresentationTexture,
    /// "After" texture for vs-previous-step split mode (pipeline through op N).
    after_step_texture: PresentationTexture,
    /// Position of the split divider as a fraction of canvas width (0.0–1.0).
    split_ratio: f32,
    /// True while the user is dragging the split divider.
    split_dragging: bool,
    /// Semi-transparent mask preview overlay texture.
    mask_overlay_texture: Option<TextureHandle>,
    /// Hash of the mask params that produced the current overlay texture.
    mask_overlay_hash: u64,
    /// Drag-start position (normalised [0, 1] image coords) for interactive mask placement.
    mask_drag_start: Option<Pos2>,
    /// Dragging index for heal spots: (spot_index, is_src_circle).
    heal_dragging: Option<(usize, bool)>,
    /// Endpoints of the horizon line being dragged, in image coordinates.
    /// None when not active.
    straighten_line: Option<[Pos2; 2]>,
    /// Index of the endpoint being dragged (0 = left, 1 = right), or None.
    straighten_dragging: Option<usize>,
}

impl Default for CanvasState {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            pan_offset: Vec2::ZERO,
            texture: PresentationTexture::default(),
            last_generation: 0,
            last_img_dims: (0, 0),
            last_canvas_size: Vec2::ZERO,
            crop_start: None,
            crop_end: None,
            sprocket_crop_was_active: false,
            crop_drag: None,
            crop_drag_start_ptr: Pos2::ZERO,
            crop_drag_start_rect: Rect::ZERO,
            overlay_texture: PresentationTexture::default(),
            before_texture: PresentationTexture::default(),
            after_step_texture: PresentationTexture::default(),
            split_ratio: 0.5,
            split_dragging: false,
            mask_overlay_texture: None,
            mask_overlay_hash: 0,
            mask_drag_start: None,
            heal_dragging: None,
            straighten_line: None,
            straighten_dragging: None,
        }
    }
}

impl CanvasState {
    pub fn ui(&mut self, ui: &mut Ui, state: &mut AppState) {
        self.draw_split_toolbar(ui, state);

        let Some(image) = state.rendered.as_ref() else {
            let placeholder = if state.loading {
                "Loading…"
            } else {
                "Open an image to begin"
            };
            ui.centered_and_justified(|ui| {
                ui.label(
                    egui::RichText::new(placeholder)
                        .size(22.0)
                        .color(Color32::from_gray(120)),
                );
            });
            return;
        };

        let img_w = image.width;
        let img_h = image.height;

        // ── Reserve bottom strip for zoom controls before computing canvas rect ──
        let available = ui.available_rect_before_wrap();
        let bar_height = 24.0;
        let canvas_size = Vec2::new(
            available.width(),
            (available.height() - bar_height).max(1.0),
        );
        let canvas_rect = Rect::from_min_size(available.min, canvas_size);
        let pixels_per_point = ui.ctx().pixels_per_point();

        self.reset_view_if_needed(state, canvas_size, pixels_per_point, img_w, img_h);
        self.sync_sprocket_crop(state);

        // ── Build a high-quality presentation texture ─────────────────────
        // See [`PresentationTexture`]: the canvas uploads the image reduced to
        // roughly its physical on-screen size rather than at full resolution.
        let effective_zoom = self.zoom / state.rendered_scale;
        self.texture.sync(
            ui.ctx(),
            "canvas_image",
            image,
            ContentId::Shared(Arc::clone(image)),
            effective_zoom * pixels_per_point,
        );

        // ── Upload split-view textures ────────────────────────────────────
        let (anchor_idx, effective_mode) = split::resolve_mode(state);
        if state.split_view {
            self.sync_split_textures(ui, state, anchor_idx, effective_mode, pixels_per_point);
            // Crop selection doesn't apply in split view.
            self.crop_start = None;
            self.crop_end = None;
        }

        self.publish_preview_viewport(state, canvas_size, img_w, img_h);

        // Extract texture ID before any &mut self calls to satisfy the borrow checker.
        let tex_id = self.texture.id().expect("synced above");
        // When the rendered image is a downsampled preview, scale up the zoom
        // so it fills the same screen area as the full-res image would.
        let display_size = Vec2::new(img_w as f32 * effective_zoom, img_h as f32 * effective_zoom);
        let image_tl = canvas_rect.min + self.pan_offset;

        let (resp, painter) = ui.allocate_painter(canvas_size, egui::Sense::click_and_drag());

        self.handle_pan_and_zoom(ui, canvas_rect, pixels_per_point);

        let (middle_down, ctrl_held, over_canvas) = ui.input(|i| {
            (
                i.pointer.button_down(egui::PointerButton::Middle)
                    || i.pointer.button_down(egui::PointerButton::Secondary),
                i.modifiers.ctrl,
                i.pointer
                    .hover_pos()
                    .map(|p| canvas_rect.contains(p))
                    .unwrap_or(false),
            )
        });

        let view = CanvasView {
            canvas_rect,
            image_tl,
            display_size,
            img_w,
            img_h,
            over_canvas,
            middle_down,
            ctrl_held,
        };

        if state.split_view {
            self.draw_split_view(ui, &painter, state, &view, effective_mode, tex_id);
        } else {
            self.draw_normal_view(ui, &resp, &painter, state, &view, tex_id);
        }

        self.draw_mask_overlay(ui, state, &view);
        self.draw_zoom_controls(ui, canvas_size, pixels_per_point, img_w, img_h);
    }

    // ── Frame setup ──────────────────────────────────────────────────────────

    /// The before/after toggle, comparison mode, and op picker.
    fn draw_split_toolbar(&mut self, ui: &mut Ui, state: &mut AppState) {
        // Precompute pipeline data up-front so the toolbar closure doesn't need
        // to reborrow `state` while the rendered image is held below.
        let op_names: Vec<String> = state
            .pipeline()
            .map(|p| {
                p.ops()
                    .iter()
                    .enumerate()
                    .map(|(i, e)| format!("{}. {}", i + 1, e.operation.name()))
                    .collect()
            })
            .unwrap_or_default();
        let op_count = op_names.len();
        let is_editing = state.editing.is_some();

        ui.horizontal(|ui| {
            if ui
                .selectable_label(state.split_view, "◧  Before / After")
                .clicked()
            {
                state.split_view = !state.split_view;
                if !state.split_view {
                    self.split_dragging = false;
                }
            }
            if !state.split_view {
                return;
            }
            egui::ComboBox::from_id_salt("split_mode")
                .selected_text(match state.split_mode {
                    SplitMode::VsOriginal => "vs. Original",
                    SplitMode::VsPreviousStep => "vs. Previous step",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut state.split_mode,
                        SplitMode::VsOriginal,
                        "vs. Original",
                    );
                    ui.add_enabled_ui(op_count > 0, |ui| {
                        ui.selectable_value(
                            &mut state.split_mode,
                            SplitMode::VsPreviousStep,
                            "vs. Previous step",
                        )
                        .on_disabled_hover_text("Add at least one operation to compare a step.");
                    });
                });
            // Op picker — only visible when vs-previous-step mode is active
            // and the user is NOT currently editing an op (in that case the
            // focus is pinned to the op under edit).
            if state.split_mode == SplitMode::VsPreviousStep && !is_editing && op_count > 0 {
                let default_idx = op_count - 1;
                let mut selected = state.split_focus.unwrap_or(default_idx).min(default_idx);
                let current_label = op_names
                    .get(selected)
                    .cloned()
                    .unwrap_or_else(|| "(none)".into());
                egui::ComboBox::from_id_salt("split_focus_op")
                    .selected_text(current_label)
                    .show_ui(ui, |ui| {
                        for (i, name) in op_names.iter().enumerate() {
                            ui.selectable_value(&mut selected, i, name);
                        }
                    });
                state.split_focus = Some(selected);
            }
        });
    }

    /// Reset zoom/pan when a new file is opened OR when dimensions change
    /// (crop, rotate 90°/270°). Sharpen, B&W, rotate 180° etc. preserve
    /// zoom/pan. Dimension changes caused by downsampled preview renders are
    /// ignored — we don't want to reset the view every time a 1/4-scale
    /// preview arrives.
    fn reset_view_if_needed(
        &mut self,
        state: &AppState,
        canvas_size: Vec2,
        pixels_per_point: f32,
        img_w: u32,
        img_h: u32,
    ) {
        let img_gen = state.image_generation;
        let dims_changed = (img_w, img_h) != self.last_img_dims && !state.rendered_is_preview;
        let canvas_resized =
            canvas_size != self.last_canvas_size && self.last_canvas_size != Vec2::ZERO;
        if img_gen != self.last_generation || dims_changed || canvas_resized {
            self.zoom = clamp_zoom(fit_zoom(img_w, img_h, canvas_size), pixels_per_point);
            self.pan_offset = Vec2::ZERO;
            self.crop_start = None;
            self.crop_end = None;
            if img_gen != self.last_generation {
                // New file opened — drop the cached before-texture so split
                // view doesn't show the previous image's pixels.  A fresh
                // pipeline starts at geometric_gen() == 0, which would
                // otherwise collide with the previous image's cached hash.
                self.before_texture.clear();
                self.after_step_texture.clear();
            }
            self.last_generation = img_gen;
            self.last_img_dims = (img_w, img_h);
        }
        self.last_canvas_size = canvas_size;
    }

    /// The sprocket look reuses the regular crop handles, but initializes them
    /// from its own fixed-2:1 workflow instead of requiring a fresh freehand
    /// drag. Reinitialize after a canvas resize, which clears the transient
    /// on-canvas rectangle.
    fn sync_sprocket_crop(&mut self, state: &AppState) {
        let active = state.tools.sprocket_crop_active;
        if active
            && (!self.sprocket_crop_was_active
                || self.crop_start.is_none()
                || self.crop_end.is_none())
        {
            if let Some(crop) = state.tools.find::<CropTool>() {
                self.crop_start = Some(Pos2::new(crop.x as f32, crop.y as f32));
                self.crop_end = Some(Pos2::new(
                    crop.x.saturating_add(crop.w) as f32,
                    crop.y.saturating_add(crop.h) as f32,
                ));
            }
        } else if !active && self.sprocket_crop_was_active {
            self.crop_start = None;
            self.crop_end = None;
            self.crop_drag = None;
        }
        self.sprocket_crop_was_active = active;
    }

    /// Publish the visible image region so preview renders can be restricted to
    /// the pixels the user can actually see.
    fn publish_preview_viewport(
        &self,
        state: &mut AppState,
        canvas_size: Vec2,
        img_w: u32,
        img_h: u32,
    ) {
        let scale = state.rendered_scale;
        let full_w = (img_w as f32 / scale) as u32;
        let full_h = (img_h as f32 / scale) as u32;
        let vis_x0 = (-self.pan_offset.x / self.zoom).max(0.0) as u32;
        let vis_y0 = (-self.pan_offset.y / self.zoom).max(0.0) as u32;
        let vis_x1 = ((canvas_size.x - self.pan_offset.x) / self.zoom).min(full_w as f32) as u32;
        let vis_y1 = ((canvas_size.y - self.pan_offset.y) / self.zoom).min(full_h as f32) as u32;
        let vp_w = vis_x1.saturating_sub(vis_x0).max(1);
        let vp_h = vis_y1.saturating_sub(vis_y0).max(1);
        state.preview_viewport = Some([vis_x0, vis_y0, vp_w, vp_h]);
    }

    /// Middle/right-mouse pan plus Ctrl+scroll-wheel zoom.
    ///
    /// In egui 0.34 Ctrl+scroll is translated into `zoom_delta()`, not
    /// `smooth_scroll_delta()`.
    fn handle_pan_and_zoom(&mut self, ui: &Ui, canvas_rect: Rect, pixels_per_point: f32) {
        ui.input(|i| {
            let over = i
                .pointer
                .hover_pos()
                .map(|p| canvas_rect.contains(p))
                .unwrap_or(false);
            if (i.pointer.button_down(egui::PointerButton::Middle)
                || i.pointer.button_down(egui::PointerButton::Secondary))
                && over
            {
                self.pan_offset += i.pointer.delta();
            }
            let zoom_factor = i.zoom_delta();
            if zoom_factor != 1.0 && over {
                let old_zoom = self.zoom;
                self.zoom = clamp_zoom(self.zoom * zoom_factor, pixels_per_point);
                let actual = self.zoom / old_zoom;
                if let Some(cursor) = i.pointer.hover_pos() {
                    let pivot = cursor - canvas_rect.min;
                    self.pan_offset = pivot - (pivot - self.pan_offset) * actual;
                }
            }
        });
    }

    fn draw_zoom_controls(
        &mut self,
        ui: &mut Ui,
        canvas_size: Vec2,
        pixels_per_point: f32,
        img_w: u32,
        img_h: u32,
    ) {
        ui.horizontal(|ui| {
            if ui.small_button("−").clicked() {
                self.zoom = clamp_zoom(self.zoom * 0.8, pixels_per_point);
            }
            ui.label(format!(
                "{:.0}%",
                physical_zoom(self.zoom, pixels_per_point) * 100.0
            ));
            if ui.small_button("+").clicked() {
                self.zoom = clamp_zoom(self.zoom * 1.25, pixels_per_point);
            }
            if ui.small_button("Fit").clicked() {
                self.zoom = clamp_zoom(fit_zoom(img_w, img_h, canvas_size), pixels_per_point);
                self.pan_offset = Vec2::ZERO;
            }
            if ui.small_button("1:1").clicked() {
                let old_zoom = self.zoom;
                self.zoom = one_to_one_zoom(pixels_per_point);
                // Keep the current view center at the same image location.
                let center = canvas_size * 0.5;
                let img_center = (center - self.pan_offset) / old_zoom;
                self.pan_offset = center - img_center * self.zoom;
            }
        });
    }

    // ── Drawing ──────────────────────────────────────────────────────────────

    /// Draw the full-resolution viewport preview on top of the rendered image.
    ///
    /// `clip` restricts it to the "after" half when split view is active.
    fn draw_preview_overlay(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        state: &AppState,
        image_tl: Pos2,
        clip: Option<Rect>,
    ) {
        let Some(overlay_img) = &state.preview_overlay else {
            self.overlay_texture.clear();
            return;
        };
        // The overlay is always rendered at full resolution, so it is placed
        // and scaled by the raw zoom, not the preview-corrected effective zoom.
        self.overlay_texture.sync(
            ctx,
            "canvas_overlay",
            overlay_img,
            ContentId::Shared(Arc::clone(overlay_img)),
            self.zoom * ctx.pixels_per_point(),
        );
        let (Some(tex_id), Some([ol_x, ol_y, ol_w, ol_h])) =
            (self.overlay_texture.id(), state.preview_overlay_rect)
        else {
            return;
        };
        let ol_tl = image_tl + Vec2::new(ol_x as f32 * self.zoom, ol_y as f32 * self.zoom);
        let ol_size = Vec2::new(ol_w as f32 * self.zoom, ol_h as f32 * self.zoom);
        let painter = match clip {
            Some(rect) => painter.with_clip_rect(rect),
            None => painter.clone(),
        };
        painter.image(
            tex_id,
            Rect::from_min_size(ol_tl, ol_size),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
    }

    /// Draw the image and hand the pointer to whichever on-canvas tool is
    /// active. Exactly one tool owns the primary drag; crop is the fallback.
    fn draw_normal_view(
        &mut self,
        ui: &mut Ui,
        resp: &egui::Response,
        painter: &egui::Painter,
        state: &mut AppState,
        view: &CanvasView,
        tex_id: egui::TextureId,
    ) {
        let over_canvas = view.over_canvas;

        // ── Cursor icon ──────────────────────────────────────────────────────
        if state.tools.mask_sel > 0 && over_canvas {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        } else if view.middle_down && over_canvas {
            ui.ctx().set_cursor_icon(egui::CursorIcon::AllScroll);
        } else if view.ctrl_held && over_canvas {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ZoomIn);
        }

        // ── Straighten line reset when tool is deactivated ───────────────────
        if !state
            .tools
            .find::<StraightenTool>()
            .is_some_and(|t| t.active)
        {
            self.straighten_line = None;
        }

        // ── Draw image ───────────────────────────────────────────────────────
        painter.image(
            tex_id,
            Rect::from_min_size(view.image_tl, view.display_size),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        self.draw_preview_overlay(ui.ctx(), painter, state, view.image_tl, None);

        if state.tools.mask_sel > 0 {
            self.handle_mask(ui, painter, state, view);
        } else if state.tools.find::<HealTool>().is_some_and(|t| t.active) {
            self.handle_heal(ui, painter, state, view);
        } else if state
            .tools
            .find::<StraightenTool>()
            .is_some_and(|t| t.active)
        {
            self.handle_straighten(ui, painter, state, view);
        } else {
            self.handle_crop(ui, resp, painter, state, view);
        }

        draw_perspective_grid(painter, state, view);
    }
}

/// Alignment grid shown whenever the Perspective section is open.
fn draw_perspective_grid(painter: &egui::Painter, state: &AppState, view: &CanvasView) {
    if !state.prefs.is_tool_open("perspective") {
        return;
    }
    let img_rect = Rect::from_min_size(view.image_tl, view.display_size);
    let clipped = painter.with_clip_rect(img_rect);
    let grid_stroke = Stroke::new(0.5_f32, Color32::from_white_alpha(70));
    let persp = state.tools.find::<PerspectiveTool>().unwrap();
    let grid_cols = persp.grid_cols;
    let grid_rows = persp.grid_rows;
    let cols = grid_cols.max(1) as f32;
    let rows = grid_rows.max(1) as f32;
    for c in 1..grid_cols {
        let t = c as f32 / cols;
        let x = img_rect.min.x + img_rect.width() * t;
        clipped.line_segment(
            [Pos2::new(x, img_rect.min.y), Pos2::new(x, img_rect.max.y)],
            grid_stroke,
        );
    }
    for r in 1..grid_rows {
        let t = r as f32 / rows;
        let y = img_rect.min.y + img_rect.height() * t;
        clipped.line_segment(
            [Pos2::new(img_rect.min.x, y), Pos2::new(img_rect.max.x, y)],
            grid_stroke,
        );
    }
}

// ---------------------------------------------------------------------------
// Zoom
// ---------------------------------------------------------------------------

/// Logical-point scale at which the whole image fits the canvas.  Callers
/// clamp the result; the floor here only guards against a degenerate canvas.
fn fit_zoom(img_w: u32, img_h: u32, available: Vec2) -> f32 {
    (available.x / img_w as f32)
        .min(available.y / img_h as f32)
        .max(f32::MIN_POSITIVE)
}

/// Zoom limits, expressed in source pixels per *framebuffer* pixel so that the
/// usable range is the same on a HiDPI display as on a 1× one.
const MIN_PHYSICAL_ZOOM: f32 = 0.05;
const MAX_PHYSICAL_ZOOM: f32 = 32.0;

/// Convert the logical-point canvas scale to source pixels per framebuffer pixel.
fn physical_zoom(logical_zoom: f32, pixels_per_point: f32) -> f32 {
    logical_zoom * pixels_per_point
}

/// Logical-point scale that maps one source pixel to one framebuffer pixel.
fn one_to_one_zoom(pixels_per_point: f32) -> f32 {
    1.0 / pixels_per_point.max(f32::MIN_POSITIVE)
}

/// Hold a logical zoom inside the physical limits for this display.
fn clamp_zoom(logical_zoom: f32, pixels_per_point: f32) -> f32 {
    let unit = one_to_one_zoom(pixels_per_point);
    logical_zoom.clamp(MIN_PHYSICAL_ZOOM * unit, MAX_PHYSICAL_ZOOM * unit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_to_one_is_one_source_pixel_per_physical_pixel() {
        for pixels_per_point in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let logical_zoom = one_to_one_zoom(pixels_per_point);
            assert!(
                (physical_zoom(logical_zoom, pixels_per_point) - 1.0).abs() < f32::EPSILON,
                "pixels_per_point={pixels_per_point}, logical_zoom={logical_zoom}"
            );
        }
    }

    /// Zoom limits are physical, so the reachable range is identical whatever
    /// the display scale.
    #[test]
    fn zoom_clamp_range_is_display_independent() {
        for pixels_per_point in [1.0, 1.5, 2.0] {
            let widest = clamp_zoom(0.0001, pixels_per_point);
            let tightest = clamp_zoom(1e6, pixels_per_point);
            assert!(
                (physical_zoom(widest, pixels_per_point) - MIN_PHYSICAL_ZOOM).abs() < 1e-4,
                "ppp={pixels_per_point}"
            );
            assert!(
                (physical_zoom(tightest, pixels_per_point) - MAX_PHYSICAL_ZOOM).abs() < 1e-3,
                "ppp={pixels_per_point}"
            );
        }
    }
}
