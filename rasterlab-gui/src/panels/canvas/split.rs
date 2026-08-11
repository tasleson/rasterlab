//! Before/after split view: resolving what the two halves show, rendering and
//! caching their textures, and the draggable divider.

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;

use egui::{Color32, Pos2, Rect, Stroke, Ui, Vec2};

use crate::state::{AppState, SplitMode};

use super::texture::ContentId;
use super::{CanvasState, CanvasView};

/// Which op the vs-previous-step comparison is anchored to, and the mode that
/// is actually achievable given the current pipeline.
///
/// The editing session takes precedence over the user's op picker; both fall
/// back to the last op in the stack. With no ops at all, vs-previous-step has
/// nothing to compare and degrades to vs-original.
pub(super) fn resolve_mode(state: &AppState) -> (Option<usize>, SplitMode) {
    if !state.split_view {
        return (None, SplitMode::VsOriginal);
    }
    let op_count = state.pipeline().map(|p| p.ops().len()).unwrap_or(0);
    let anchor_idx = (op_count > 0).then(|| {
        let last = op_count - 1;
        state
            .editing
            .map(|s| s.op_index)
            .or(state.split_focus)
            .unwrap_or(last)
            .min(last)
    });
    let mode = match state.split_mode {
        SplitMode::VsPreviousStep if anchor_idx.is_some() => SplitMode::VsPreviousStep,
        _ => SplitMode::VsOriginal,
    };
    (anchor_idx, mode)
}

impl CanvasState {
    /// Render and upload whichever "before"/"after" textures the current split
    /// mode needs, reusing the cached ones when nothing relevant has changed.
    pub(super) fn sync_split_textures(
        &mut self,
        ui: &Ui,
        state: &AppState,
        anchor_idx: Option<usize>,
        mode: SplitMode,
        pixels_per_point: f32,
    ) {
        // Compute the live-edit preview op BEFORE borrowing the pipeline:
        // while an edit session is active, the op at `anchor_idx` is disabled in
        // the pipeline, so `render_prefix(n+1)` would skip it and produce the
        // same pixels as `render_prefix(n)`.  We need to apply the tool's live
        // preview op to synthesise the "after" side.
        let edit_preview_op: Option<Box<dyn rasterlab_core::traits::operation::Operation>> =
            if state.editing.is_some() && mode == SplitMode::VsPreviousStep {
                state.tools.preview_op()
            } else {
                None
            };
        let preview_hash = edit_preview_op
            .as_deref()
            .and_then(|op| serde_json::to_string(op).ok())
            .map(|s| hash_key(&s))
            .unwrap_or(0);

        let Some(pipeline) = state.pipeline() else {
            return;
        };
        let step_gen = pipeline.step_cache_gen();
        let geo_gen = pipeline.geometric_gen();
        let split_scale = self.zoom * pixels_per_point;

        match mode {
            SplitMode::VsOriginal => {
                // Invalidate after_step texture — it's unused in this mode.
                self.after_step_texture.clear();
                // Key distinguishes the two modes so switching modes forces a
                // refresh even if geo_gen happens to match.
                let content = ContentId::Generation(hash_key(&(0u64, geo_gen)));
                if self.before_texture.is_stale(&content, split_scale) {
                    match pipeline.render_geometric_only() {
                        Ok(img) => self.before_texture.upload(
                            ui.ctx(),
                            "canvas_before",
                            &img,
                            content,
                            split_scale,
                        ),
                        Err(e) => eprintln!("render_geometric_only failed: {e}"),
                    }
                }
            }
            SplitMode::VsPreviousStep => {
                let n = anchor_idx.unwrap_or(0);
                let before_content =
                    ContentId::Generation(hash_key(&(1u64, step_gen, n as u64, 0u64)));
                let after_content = ContentId::Generation(hash_key(&(
                    1u64,
                    step_gen,
                    n as u64,
                    1u64,
                    preview_hash,
                )));
                if self.before_texture.is_stale(&before_content, split_scale) {
                    match pipeline.render_prefix(n) {
                        Ok(img) => self.before_texture.upload(
                            ui.ctx(),
                            "canvas_before",
                            &img,
                            before_content,
                            split_scale,
                        ),
                        Err(e) => eprintln!("render_prefix({n}) failed: {e}"),
                    }
                }
                if self
                    .after_step_texture
                    .is_stale(&after_content, split_scale)
                {
                    let after_result = match &edit_preview_op {
                        Some(op) => pipeline.render_prefix(n).and_then(|img| {
                            let owned = match Arc::try_unwrap(img) {
                                Ok(img) => img,
                                Err(arc) => arc.as_ref().deep_clone(),
                            };
                            op.apply(owned).map(Arc::new).map_err(|e| {
                                rasterlab_core::error::RasterError::Pipeline(format!(
                                    "Preview op '{}' failed: {}",
                                    op.name(),
                                    e
                                ))
                            })
                        }),
                        None => pipeline.render_prefix(n + 1),
                    };
                    match after_result {
                        Ok(img) => self.after_step_texture.upload(
                            ui.ctx(),
                            "canvas_after_step",
                            &img,
                            after_content,
                            split_scale,
                        ),
                        Err(e) => eprintln!("split-view after render failed: {e}"),
                    }
                }
            }
        }
    }

    /// Draw both halves, the divider, and the BEFORE/AFTER labels.
    pub(super) fn draw_split_view(
        &mut self,
        ui: &mut Ui,
        painter: &egui::Painter,
        state: &AppState,
        view: &CanvasView,
        mode: SplitMode,
        fallback_tex_id: egui::TextureId,
    ) {
        let CanvasView {
            canvas_rect,
            image_tl,
            display_size,
            over_canvas,
            middle_down,
            ctrl_held,
            ..
        } = *view;

        // In vs-previous-step mode the "after" side shows the image through the
        // editing op, not the final pipeline output.
        let (after_tex_id, after_logical_size) = match mode {
            SplitMode::VsPreviousStep => (
                self.after_step_texture.id().unwrap_or(fallback_tex_id),
                self.after_step_texture.source_size,
            ),
            SplitMode::VsOriginal => (fallback_tex_id, (0, 0)),
        };

        // split_x is a fraction of the *image* display width, not the canvas width,
        // so it stays aligned to the image regardless of rotation or letterboxing.
        let split_x = image_tl.x + display_size.x * self.split_ratio;
        let left_clip = Rect::from_min_max(canvas_rect.min, Pos2::new(split_x, canvas_rect.max.y));
        let right_clip = Rect::from_min_max(Pos2::new(split_x, canvas_rect.min.y), canvas_rect.max);

        // ── Draw before (source + geometric ops, left half) ──────────────────
        if let Some(before_tex_id) = self.before_texture.id() {
            // Use the recorded logical size of the geometric image (may differ
            // from source after rotate/flip/crop) — always full-res so no
            // rendered_scale correction is needed.
            let (bw, bh) = self.before_texture.source_size;
            let before_size = if bw > 0 && bh > 0 {
                Vec2::new(bw as f32 * self.zoom, bh as f32 * self.zoom)
            } else {
                display_size
            };
            painter.with_clip_rect(left_clip).image(
                before_tex_id,
                Rect::from_min_size(image_tl, before_size),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        // ── Draw after (rendered image, right half) ──────────────────────────
        let (aw, ah) = after_logical_size;
        let after_size = if aw > 0 && ah > 0 {
            Vec2::new(aw as f32 * self.zoom, ah as f32 * self.zoom)
        } else {
            display_size
        };
        painter.with_clip_rect(right_clip).image(
            after_tex_id,
            Rect::from_min_size(image_tl, after_size),
            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );

        // Preview overlay belongs on the after (right) side only.
        self.draw_preview_overlay(ui.ctx(), painter, state, image_tl, Some(right_clip));

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
        } else if middle_down && over_canvas {
            ui.ctx().set_cursor_icon(egui::CursorIcon::AllScroll);
        } else if ctrl_held && over_canvas {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ZoomIn);
        }

        // ── Draw divider line ────────────────────────────────────────────────
        // Recompute split_x after any drag update this frame.
        let sx = image_tl.x + display_size.x * self.split_ratio;
        let top = Pos2::new(sx, canvas_rect.min.y);
        let bot = Pos2::new(sx, canvas_rect.max.y);
        painter.line_segment(
            [top, bot],
            Stroke::new(3.0_f32, Color32::from_black_alpha(80)),
        );
        painter.line_segment([top, bot], Stroke::new(1.0_f32, Color32::WHITE));

        // Small circular handle at the vertical midpoint.
        let mid = Pos2::new(sx, canvas_rect.center().y);
        painter.circle_filled(mid, 7.0, Color32::from_black_alpha(100));
        painter.circle_stroke(mid, 7.0, Stroke::new(1.5_f32, Color32::WHITE));

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
}

fn hash_key<T: Hash>(t: &T) -> u64 {
    let mut h = DefaultHasher::new();
    t.hash(&mut h);
    h.finish()
}
