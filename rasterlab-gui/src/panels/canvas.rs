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
    /// "Before" texture for split view — source image with geometric ops applied.
    before_texture: Option<TextureHandle>,
    before_hash: u64,
    /// Logical pixel dimensions of the before image (may differ from source after rotate/crop).
    before_logical_size: (u32, u32),
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
            texture: None,
            last_hash: 0,
            last_generation: 0,
            last_img_dims: (0, 0),
            last_canvas_size: Vec2::ZERO,
            crop_start: None,
            crop_end: None,
            overlay_texture: None,
            overlay_last_hash: 0,
            before_texture: None,
            before_hash: 0,
            before_logical_size: (0, 0),
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

        // ── Viewport toolbar ──────────────────────────────────────────────────
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
        });

        // ── Reserve bottom strip for zoom controls before computing canvas rect ──
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

        // ── Upload "before" texture when split view is active ─────────────────
        // "Before" is the source image with only geometric ops applied (rotate,
        // flip, crop) so both sides of the split share the same orientation.
        if state.split_view {
            if let Some(pipeline) = state.pipeline() {
                let geo_gen = pipeline.geometric_gen();
                if self.before_texture.is_none() || geo_gen != self.before_hash {
                    match pipeline.render_geometric_only() {
                        Ok(geo_img) => {
                            self.before_logical_size = (geo_img.width, geo_img.height);
                            self.before_texture = Some(ui.ctx().load_texture(
                                "canvas_before",
                                image_to_egui(&geo_img),
                                TextureOptions::LINEAR,
                            ));
                            self.before_hash = geo_gen;
                        }
                        Err(e) => {
                            eprintln!("render_geometric_only failed: {e}");
                        }
                    }
                }
            }
            // Crop selection doesn't apply in split view.
            self.crop_start = None;
            self.crop_end = None;
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
        {
            let rs = state.rendered_scale;
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

        // Extract texture ID before any &mut self calls to satisfy the borrow checker.
        let tex_id = self.texture.as_ref().unwrap().id();
        // When the rendered image is a downsampled preview, scale up the zoom
        // so it fills the same screen area as the full-res image would.
        let effective_zoom = self.zoom / state.rendered_scale;
        let display_size = Vec2::new(img_w as f32 * effective_zoom, img_h as f32 * effective_zoom);
        let image_tl = canvas_rect.min + self.pan_offset;

        let (resp, painter) = ui.allocate_painter(canvas_size, egui::Sense::click_and_drag());

        // ── Middle-mouse pan + Ctrl+scroll-wheel zoom ────────────────────────
        // In egui 0.34 Ctrl+scroll is translated into zoom_delta() (not smooth_scroll_delta).
        ui.input(|i| {
            if i.pointer.button_down(egui::PointerButton::Middle) {
                self.pan_offset += i.pointer.delta();
            }
            let zoom_factor = i.zoom_delta();
            let over = i
                .pointer
                .hover_pos()
                .map(|p| canvas_rect.contains(p))
                .unwrap_or(false);
            if zoom_factor != 1.0 && over {
                let old_zoom = self.zoom;
                self.zoom = (self.zoom * zoom_factor).clamp(0.05, 32.0);
                let actual = self.zoom / old_zoom;
                if let Some(cursor) = i.pointer.hover_pos() {
                    let pivot = cursor - canvas_rect.min;
                    self.pan_offset = pivot - (pivot - self.pan_offset) * actual;
                }
            }
        });

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

        if state.split_view {
            self.draw_split_view(
                ui,
                &resp,
                &painter,
                state,
                canvas_rect,
                image_tl,
                display_size,
                tex_id,
                middle_down,
                ctrl_held,
                over_canvas,
            );
        } else {
            self.draw_normal_view(
                ui,
                &resp,
                &painter,
                state,
                canvas_rect,
                image_tl,
                display_size,
                tex_id,
                img_w,
                img_h,
                middle_down,
                ctrl_held,
                over_canvas,
            );
        }

        // ── Mask overlay ──────────────────────────────────────────────────────
        // Rendered at 256×256 and scaled to the image area so the user can see
        // where the next masked Apply will take effect.
        if state.tools.mask_sel > 0 {
            let mh = mask_params_hash(state);
            if self.mask_overlay_texture.is_none() || mh != self.mask_overlay_hash {
                self.mask_overlay_texture = Some(ui.ctx().load_texture(
                    "mask_overlay",
                    build_mask_preview(state, 256, 256),
                    TextureOptions::LINEAR,
                ));
                self.mask_overlay_hash = mh;
            }
            if let Some(mol_tex) = &self.mask_overlay_texture {
                // The overlay covers the image area in screen space.
                let rs = state.rendered_scale;
                let full_w = img_w as f32 / rs;
                let full_h = img_h as f32 / rs;
                let overlay_rect = Rect::from_min_size(
                    image_tl,
                    Vec2::new(full_w * self.zoom, full_h * self.zoom),
                );
                // Use a clipped painter so it stays inside the canvas area.
                let clipped = ui.painter().with_clip_rect(canvas_rect);
                clipped.image(
                    mol_tex.id(),
                    overlay_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        } else {
            self.mask_overlay_texture = None;
            self.mask_overlay_hash = 0;
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
                let old_zoom = self.zoom;
                self.zoom = 1.0;
                // Keep the current view center at the same image location.
                let center = canvas_size * 0.5;
                let img_center = (center - self.pan_offset) / old_zoom;
                self.pan_offset = center - img_center * self.zoom;
            }
        });
    }

    // ── Split view rendering ─────────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn draw_split_view(
        &mut self,
        ui: &mut Ui,
        _resp: &egui::Response,
        painter: &egui::Painter,
        state: &mut AppState,
        canvas_rect: Rect,
        image_tl: Pos2,
        display_size: Vec2,
        after_tex_id: egui::TextureId,
        middle_down: bool,
        ctrl_held: bool,
        over_canvas: bool,
    ) {
        // split_x is a fraction of the *image* display width, not the canvas width,
        // so it stays aligned to the image regardless of rotation or letterboxing.
        let split_x = image_tl.x + display_size.x * self.split_ratio;
        let left_clip = Rect::from_min_max(canvas_rect.min, Pos2::new(split_x, canvas_rect.max.y));
        let right_clip = Rect::from_min_max(Pos2::new(split_x, canvas_rect.min.y), canvas_rect.max);

        // ── Draw before (source + geometric ops, left half) ──────────────────
        if let Some(before_tex) = &self.before_texture {
            // Use the recorded logical size of the geometric image (may differ
            // from source after rotate/flip/crop) — always full-res so no
            // rendered_scale correction is needed.
            let (bw, bh) = self.before_logical_size;
            let before_size = if bw > 0 && bh > 0 {
                Vec2::new(bw as f32 * self.zoom, bh as f32 * self.zoom)
            } else {
                display_size
            };
            painter.with_clip_rect(left_clip).image(
                before_tex.id(),
                Rect::from_min_size(image_tl, before_size),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        // ── Draw after (rendered image, right half) ──────────────────────────
        painter.with_clip_rect(right_clip).image(
            after_tex_id,
            Rect::from_min_size(image_tl, display_size),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        // ── Preview overlay on the after (right) side only ──────────────────
        if let Some(overlay_img) = &state.preview_overlay {
            let oh = compute_hash(overlay_img);
            if self.overlay_texture.is_none() || oh != self.overlay_last_hash {
                self.overlay_texture = Some(ui.ctx().load_texture(
                    "canvas_overlay",
                    image_to_egui(overlay_img),
                    TextureOptions::LINEAR,
                ));
                self.overlay_last_hash = oh;
            }
            if let (Some(ol_tex), Some([ol_x, ol_y, ol_w, ol_h])) =
                (&self.overlay_texture, &state.preview_overlay_rect)
            {
                let ol_tl =
                    image_tl + Vec2::new(*ol_x as f32 * self.zoom, *ol_y as f32 * self.zoom);
                let ol_size = Vec2::new(*ol_w as f32 * self.zoom, *ol_h as f32 * self.zoom);
                painter.with_clip_rect(right_clip).image(
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

        // ── Divider drag interaction ─────────────────────────────────────────
        // Use raw pointer input rather than resp events — resp drag state can
        // become stale after Ctrl+scroll zoom or middle-mouse pan, causing the
        // divider to stop responding until split view is toggled.
        let (ptr_pos, primary_pressed, primary_down) = ui.input(|i| {
            (
                i.pointer.hover_pos(),
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_down(egui::PointerButton::Primary),
            )
        });
        let near_divider = ptr_pos
            .map(|p| (p.x - split_x).abs() < 6.0 && canvas_rect.contains(p))
            .unwrap_or(false);

        if primary_pressed && near_divider {
            self.split_dragging = true;
        }
        if !primary_down {
            self.split_dragging = false;
        }
        if self.split_dragging
            && let Some(p) = ptr_pos
        {
            self.split_ratio = ((p.x - image_tl.x) / display_size.x).clamp(0.05, 0.95);
        }

        // Cursor priority: divider drag > middle-mouse pan > ctrl zoom.
        if near_divider || self.split_dragging {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        } else if middle_down {
            ui.ctx().set_cursor_icon(egui::CursorIcon::AllScroll);
        } else if ctrl_held && over_canvas {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ZoomIn);
        }

        // ── Draw divider line ────────────────────────────────────────────────
        // Recompute split_x after any drag update this frame.
        let sx = image_tl.x + display_size.x * self.split_ratio;
        let top = Pos2::new(sx, canvas_rect.min.y);
        let bot = Pos2::new(sx, canvas_rect.max.y);
        painter.line_segment([top, bot], Stroke::new(3.0, Color32::from_black_alpha(80)));
        painter.line_segment([top, bot], Stroke::new(1.0, Color32::WHITE));

        // Small circular handle at the vertical midpoint.
        let mid = Pos2::new(sx, canvas_rect.center().y);
        painter.circle_filled(mid, 7.0, Color32::from_black_alpha(100));
        painter.circle_stroke(mid, 7.0, Stroke::new(1.5, Color32::WHITE));

        // ── Labels ──────────────────────────────────────────────────────────
        let font = egui::FontId::proportional(11.0);
        let label_y = canvas_rect.min.y + 8.0;
        painter.with_clip_rect(left_clip).text(
            Pos2::new(sx - 10.0, label_y),
            egui::Align2::RIGHT_TOP,
            "BEFORE",
            font.clone(),
            Color32::from_black_alpha(160),
        );
        painter.with_clip_rect(left_clip).text(
            Pos2::new(sx - 11.0, label_y + 1.0),
            egui::Align2::RIGHT_TOP,
            "BEFORE",
            font.clone(),
            Color32::WHITE,
        );
        painter.with_clip_rect(right_clip).text(
            Pos2::new(sx + 10.0, label_y),
            egui::Align2::LEFT_TOP,
            "AFTER",
            font.clone(),
            Color32::from_black_alpha(160),
        );
        painter.with_clip_rect(right_clip).text(
            Pos2::new(sx + 11.0, label_y + 1.0),
            egui::Align2::LEFT_TOP,
            "AFTER",
            font,
            Color32::WHITE,
        );
    }

    // ── Normal single-image view ─────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    fn draw_normal_view(
        &mut self,
        ui: &mut Ui,
        resp: &egui::Response,
        painter: &egui::Painter,
        state: &mut AppState,
        canvas_rect: Rect,
        image_tl: Pos2,
        display_size: Vec2,
        tex_id: egui::TextureId,
        img_w: u32,
        img_h: u32,
        middle_down: bool,
        ctrl_held: bool,
        over_canvas: bool,
    ) {
        // ── Cursor icon ──────────────────────────────────────────────────────
        if state.tools.mask_sel > 0 && over_canvas {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        } else if middle_down {
            ui.ctx().set_cursor_icon(egui::CursorIcon::AllScroll);
        } else if ctrl_held && over_canvas {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ZoomIn);
        }

        // ── Straighten line reset when tool is deactivated ───────────────────
        if !state.tools.straighten_active {
            self.straighten_line = None;
        }

        // ── Draw image ───────────────────────────────────────────────────────
        painter.image(
            tex_id,
            Rect::from_min_size(image_tl, display_size),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        // ── Overlay: full-resolution viewport preview ─────────────────────
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

        if state.tools.mask_sel > 0 {
            // ── Mask drag: click-drag on canvas to define the mask ────────────
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
                        1 => update_linear_mask(state, start, end),
                        2 => update_radial_mask(state, start, end),
                        _ => {}
                    }
                }
            } else {
                self.mask_drag_start = None;
            }

            // ── Draw mask handles ────────────────────────────────────────────
            match state.tools.mask_sel {
                1 => draw_linear_mask_handles(painter, state, image_tl, display_size, canvas_rect),
                2 => draw_radial_mask_handles(painter, state, image_tl, display_size, canvas_rect),
                _ => {}
            }
        } else if state.tools.heal_active {
            // ── Heal tool ─────────────────────────────────────────────────────
            // Clear crop selection while heal mode is active.
            self.crop_start = None;
            self.crop_end = None;

            let (ptr_pos, primary_pressed, primary_released, primary_down, secondary_clicked) = ui
                .input(|i| {
                    (
                        i.pointer.hover_pos(),
                        i.pointer.button_pressed(egui::PointerButton::Primary),
                        i.pointer.button_released(egui::PointerButton::Primary),
                        i.pointer.button_down(egui::PointerButton::Primary),
                        i.pointer.button_clicked(egui::PointerButton::Secondary),
                    )
                });

            if over_canvas {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
            }

            // Draw cursor ring at hover position
            if let Some(ptr) = ptr_pos
                && over_canvas
            {
                let r_screen = state.tools.heal_radius as f32 * self.zoom;
                painter.circle_stroke(
                    ptr,
                    r_screen,
                    egui::Stroke::new(1.5, Color32::from_white_alpha(200)),
                );
                painter.circle_stroke(
                    ptr,
                    r_screen,
                    egui::Stroke::new(0.5, Color32::from_black_alpha(120)),
                );
            }

            // Hit-test existing spots for drag / remove
            let hit_spot = ptr_pos.and_then(|ptr| {
                let img_pos = screen_to_image(ptr, image_tl, self.zoom);
                let handle_r_img = (8.0 / self.zoom).max(state.tools.heal_radius as f32 * 0.4);
                state
                    .tools
                    .heal_spots
                    .iter()
                    .enumerate()
                    .find_map(|(i, spot)| {
                        let dst = Pos2::new(spot.dest_x as f32, spot.dest_y as f32);
                        let src = Pos2::new(spot.src_x as f32, spot.src_y as f32);
                        if (img_pos - dst).length() < handle_r_img {
                            Some((i, false))
                        } else if (img_pos - src).length() < handle_r_img {
                            Some((i, true))
                        } else {
                            None
                        }
                    })
            });

            // Start drag on mouse-down; hitting an existing spot begins a drag
            if primary_pressed && let Some((idx, is_src)) = hit_spot {
                self.heal_dragging = Some((idx, is_src));
            }

            // Continue drag while held
            if primary_down && let (Some((idx, is_src)), Some(ptr)) = (self.heal_dragging, ptr_pos)
            {
                let img_pos = screen_to_image(ptr, image_tl, self.zoom);
                if let Some(spot) = state.tools.heal_spots.get_mut(idx) {
                    if is_src {
                        spot.src_x = img_pos.x as i32;
                        spot.src_y = img_pos.y as i32;
                    } else {
                        let dx = img_pos.x as i32 - spot.dest_x;
                        let dy = img_pos.y as i32 - spot.dest_y;
                        spot.dest_x = img_pos.x as i32;
                        spot.dest_y = img_pos.y as i32;
                        spot.src_x += dx;
                        spot.src_y += dy;
                    }
                }
            }

            // On release: place a new spot only if this press was not a drag
            if primary_released {
                if self.heal_dragging.is_none()
                    && hit_spot.is_none()
                    && let Some(ptr) = ptr_pos
                    && over_canvas
                {
                    let img_pos = screen_to_image(ptr, image_tl, self.zoom);
                    state.heal_place_spot(img_pos.x as i32, img_pos.y as i32);
                }
                self.heal_dragging = None;
            }

            // Right-click removes nearest spot
            if secondary_clicked && let Some((idx, _)) = hit_spot {
                state.tools.heal_spots.remove(idx);
            }

            // Draw spot overlays
            for (i, spot) in state.tools.heal_spots.iter().enumerate() {
                let dst_screen = image_to_screen(
                    Pos2::new(spot.dest_x as f32, spot.dest_y as f32),
                    image_tl,
                    self.zoom,
                );
                let src_screen = image_to_screen(
                    Pos2::new(spot.src_x as f32, spot.src_y as f32),
                    image_tl,
                    self.zoom,
                );
                let r_screen = spot.radius as f32 * self.zoom;

                let hovered_src = matches!(hit_spot, Some((hi, true)) if hi == i);
                let hovered_dst = matches!(hit_spot, Some((hi, false)) if hi == i);

                // Arrow from src to dst
                painter.arrow(
                    src_screen,
                    dst_screen - src_screen,
                    egui::Stroke::new(1.0, Color32::from_white_alpha(180)),
                );

                // Source circle (green)
                painter.circle_stroke(
                    src_screen,
                    r_screen,
                    egui::Stroke::new(
                        if hovered_src { 3.0 } else { 1.5 },
                        Color32::from_rgb(80, 200, 80),
                    ),
                );
                painter.circle_filled(src_screen, 4.0, Color32::from_rgb(80, 200, 80));

                // Dest circle (red)
                painter.circle_stroke(
                    dst_screen,
                    r_screen,
                    egui::Stroke::new(
                        if hovered_dst { 3.0 } else { 1.5 },
                        Color32::from_rgb(220, 60, 60),
                    ),
                );
                painter.circle_filled(dst_screen, 4.0, Color32::from_rgb(220, 60, 60));
            }
        } else if state.tools.straighten_active {
            // ── Straighten tool ──────────────────────────────────────────────
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
                self.straighten_line =
                    Some([Pos2::new(cx - half_w, cy), Pos2::new(cx + half_w, cy)]);
                state.tools.straighten_angle = 0.0;
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
            }

            if primary_released {
                self.straighten_dragging = None;
            }

            // Recompute angle from current line.
            if let Some([p0, p1]) = self.straighten_line {
                let dx = p1.x - p0.x;
                let dy = p1.y - p0.y;
                let line_angle = dy.atan2(dx).to_degrees();
                state.tools.straighten_angle = -line_angle;
                state.update_straighten_preview();
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
                    Stroke::new(0.5, Color32::from_white_alpha(60)),
                );
                clipped.line_segment(
                    [Pos2::new(img_rect.min.x, y), Pos2::new(img_rect.max.x, y)],
                    Stroke::new(0.5, Color32::from_white_alpha(60)),
                );
            }

            // ── Horizon line ─────────────────────────────────────────────────
            if let Some([p0, p1]) = self.straighten_line {
                let s0 = image_to_screen(p0, image_tl, self.zoom);
                let s1 = image_to_screen(p1, image_tl, self.zoom);
                // Shadow
                painter.line_segment([s0, s1], Stroke::new(2.5, Color32::from_black_alpha(100)));
                // Main line
                painter.line_segment([s0, s1], Stroke::new(1.5, Color32::WHITE));
                // Endpoints
                for &ep in &[s0, s1] {
                    painter.circle_filled(ep, 6.0, Color32::from_black_alpha(100));
                    painter.circle_stroke(ep, 6.0, Stroke::new(1.5, Color32::WHITE));
                }
                // Angle label near midpoint
                let mid = Pos2::new((s0.x + s1.x) / 2.0, (s0.y + s1.y) / 2.0 - 14.0);
                let angle_text = format!("{:.2}°", state.tools.straighten_angle);
                painter.text(
                    mid,
                    egui::Align2::CENTER_BOTTOM,
                    &angle_text,
                    egui::FontId::proportional(12.0),
                    Color32::WHITE,
                );
            }
        } else {
            // ── Crop selection (primary drag only) ───────────────────────────
            if resp.drag_started_by(egui::PointerButton::Primary) {
                self.crop_start = resp
                    .interact_pointer_pos()
                    .map(|p| screen_to_image(p, image_tl, self.zoom));
                self.crop_end = self.crop_start;
            }
            if resp.dragged_by(egui::PointerButton::Primary)
                && let Some(raw_end) = resp
                    .interact_pointer_pos()
                    .map(|p| screen_to_image(p, image_tl, self.zoom))
            {
                // Constrain the drag-end for aspect-ratio-locked crops
                self.crop_end = Some(constrain_drag_end(
                    self.crop_start.unwrap_or(raw_end),
                    raw_end,
                    state.tools.crop_aspect_ratio(),
                ));
            }
            if resp.drag_stopped_by(egui::PointerButton::Primary)
                && let (Some(start), Some(end)) = (self.crop_start, self.crop_end)
            {
                let (x, y, w, h) = image_to_crop(start, end, img_w, img_h);
                let (cx, cy, cw, ch) =
                    constrain_aspect(x, y, w, h, img_w, img_h, state.tools.crop_aspect_ratio());
                state.tools.crop_x = cx;
                state.tools.crop_y = cy;
                state.tools.crop_w = cw;
                state.tools.crop_h = ch;
            }

            // ── Clear selection: right-click or Escape ───────────────────────
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

            // ── Marching-ants overlay ─────────────────────────────────────────
            if let (Some(start), Some(end)) = (self.crop_start, self.crop_end) {
                let sel = Rect::from_two_pos(
                    image_to_screen(start, image_tl, self.zoom),
                    image_to_screen(end, image_tl, self.zoom),
                );
                if sel.width() > 2.0 && sel.height() > 2.0 {
                    let time = ui.input(|i| i.time) as f32;
                    draw_marching_ants(painter, sel, time);
                    ui.ctx().request_repaint_after(Duration::from_millis(16));
                }
            }
        }
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
        Stroke::new(2.0, Color32::WHITE),
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
// Normalised ↔ screen helpers and interactive mask placement
// ---------------------------------------------------------------------------

/// Convert a screen position to normalised [0, 1] image coordinates.
fn screen_to_norm(screen: Pos2, image_tl: Pos2, display_size: Vec2) -> Pos2 {
    Pos2::new(
        (screen.x - image_tl.x) / display_size.x,
        (screen.y - image_tl.y) / display_size.y,
    )
}

/// Convert a normalised [0, 1] image position to screen coordinates.
fn norm_to_screen(norm: Pos2, image_tl: Pos2, display_size: Vec2) -> Pos2 {
    Pos2::new(
        image_tl.x + norm.x * display_size.x,
        image_tl.y + norm.y * display_size.y,
    )
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

    let shadow = Stroke::new(3.0, Color32::from_black_alpha(160));
    let white = Stroke::new(1.5, Color32::WHITE);

    painter.line_segment([a_s, b_s], shadow);
    painter.line_segment([a_s, b_s], white);

    for &pt in &[a_s, center_s, b_s] {
        painter.circle_filled(pt, 6.0, Color32::from_black_alpha(160));
        painter.circle_stroke(pt, 6.0, Stroke::new(1.5, Color32::WHITE));
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
        Stroke::new(3.0, Color32::from_black_alpha(160)),
    );
    draw_ellipse_stroke(&painter, center_s, rx, ry, Stroke::new(1.5, Color32::WHITE));

    // Crosshair at centre.
    let arm = 8.0_f32;
    painter.line_segment(
        [
            center_s - Vec2::new(arm, 0.0),
            center_s + Vec2::new(arm, 0.0),
        ],
        Stroke::new(1.5, Color32::WHITE),
    );
    painter.line_segment(
        [
            center_s - Vec2::new(0.0, arm),
            center_s + Vec2::new(0.0, arm),
        ],
        Stroke::new(1.5, Color32::WHITE),
    );
    painter.circle_filled(center_s, 4.0, Color32::from_black_alpha(160));
    painter.circle_stroke(center_s, 4.0, Stroke::new(1.5, Color32::WHITE));
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

// ---------------------------------------------------------------------------
// Mask overlay helpers
// ---------------------------------------------------------------------------

/// Hash the current mask parameters so the overlay texture is only rebuilt
/// when something actually changes.
fn mask_params_hash(state: &crate::state::AppState) -> u64 {
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
fn build_mask_preview(state: &crate::state::AppState, w: usize, h: usize) -> ColorImage {
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

// ---------------------------------------------------------------------------
// Texture / hash helpers
// ---------------------------------------------------------------------------

/// wgpu's hard limit on texture dimensions (D3D12/Metal/Vulkan minimum guarantee).
const MAX_TEXTURE_DIM: usize = 8192;

fn image_to_egui(image: &Image) -> ColorImage {
    let orig_w = image.width as usize;
    let orig_h = image.height as usize;

    // Integer downsample factor: smallest value that brings both dimensions
    // under MAX_TEXTURE_DIM.  For most images factor == 1 (no-op).
    let factor = orig_w.max(orig_h).div_ceil(MAX_TEXTURE_DIM).max(1);

    let tex_w = orig_w / factor;
    let tex_h = orig_h / factor;

    let pixels: Vec<Color32> = if factor == 1 {
        // Fast path: sequential, memory-bandwidth-bound.  Parallelising with
        // rayon adds coordination overhead that outweighs any gain on Apple
        // Silicon (~14 ms parallel vs ~7 ms serial in benchmarks).
        image
            .data
            .chunks_exact(4)
            .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]))
            .collect()
    } else {
        // Nearest-neighbour integer downsample to fit within the GPU texture
        // size limit.  Only triggered for very large sensors (e.g. >8192 px).
        (0..tex_h)
            .flat_map(|y| {
                let src_row = (y * factor) * orig_w;
                (0..tex_w).map(move |x| {
                    let src = (src_row + x * factor) * 4;
                    Color32::from_rgba_unmultiplied(
                        image.data[src],
                        image.data[src + 1],
                        image.data[src + 2],
                        image.data[src + 3],
                    )
                })
            })
            .collect()
    };

    ColorImage {
        size: [tex_w, tex_h],
        pixels,
        source_size: egui::Vec2::new(orig_w as f32, orig_h as f32),
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

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

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
