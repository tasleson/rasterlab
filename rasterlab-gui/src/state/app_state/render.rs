//! Render coordination: choosing what to render, spawning the background
//! render, folding results back into [`AppState`], and the status text that
//! reports which backend is doing the work.

use std::{sync::Arc, time::Duration};

use rasterlab_core::{
    Image, cancel as core_cancel,
    ops::{MaskedOp, NoiseReductionOp, NrMethod},
    traits::operation::Operation,
};
use rasterlab_render::{PREVIEW_SCALE, RenderMeta, RenderRequest, RenderResult};

use super::AppState;

/// A full-resolution non-local-means preview kept so that pressing Apply can
/// reuse the pixels the user is already looking at instead of recomputing them.
#[derive(Clone)]
pub(super) struct ReusableNrPreview {
    pub(super) copy_index: usize,
    pub(super) cursor: usize,
    pub(super) cache_gen: u64,
    pub(super) signature: NrPreviewSignature,
    pub(super) image: Arc<Image>,
}

/// The parameters that must match for a cached NR preview to still be valid.
#[derive(Clone, PartialEq)]
pub(super) struct NrPreviewSignature {
    method: NrMethod,
    luma_strength: f32,
    color_strength: f32,
    detail_preservation: f32,
}

impl NrPreviewSignature {
    fn from_op(op: &NoiseReductionOp) -> Self {
        Self {
            method: op.method.clone(),
            luma_strength: op.luma_strength,
            color_strength: op.color_strength,
            detail_preservation: op.detail_preservation,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProcessingBackend {
    Cpu,
    Gpu,
    Mixed,
}

impl ProcessingBackend {
    fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Gpu => "GPU",
            Self::Mixed => "CPU/GPU",
        }
    }
}

/// Predict which backend(s) the upcoming render will use, mirroring the
/// batching and threshold decisions `rasterlab-render` makes internally.
fn estimate_processing_backend(
    start_image: &Image,
    committed_ops: &[Option<Box<dyn Operation>>],
    preview_op: Option<&dyn Operation>,
    preview_scale: Option<f32>,
    preview_viewport: Option<[u32; 4]>,
    overlay_viewport: Option<[u32; 4]>,
    has_gpu: bool,
) -> ProcessingBackend {
    let committed_pixels = preview_scale
        .map(|scale| scaled_pixel_count(start_image.width, start_image.height, scale))
        .unwrap_or_else(|| start_image.pixel_count());
    let preview_pixels = overlay_viewport
        .map(|[_x, _y, w, h]| w as usize * h as usize)
        .or_else(|| {
            preview_viewport.and_then(|[_x, _y, w, h]| {
                preview_scale.map(|scale| scaled_pixel_count(w, h, scale))
            })
        })
        .unwrap_or(committed_pixels);

    let mut saw_cpu = false;
    let mut saw_gpu = false;

    let mut index = 0;
    while index < committed_ops.len() {
        let Some(op) = &committed_ops[index] else {
            index += 1;
            continue;
        };
        if has_gpu && rasterlab_gpu::supports(op.as_ref()) {
            let start = index;
            let mut end = index + 1;
            while end < committed_ops.len() {
                let Some(next_op) = &committed_ops[end] else {
                    break;
                };
                if !rasterlab_gpu::supports(next_op.as_ref()) {
                    break;
                }
                end += 1;
            }
            if end - start > 1 {
                let ops = committed_ops[start..end]
                    .iter()
                    .filter_map(|op| op.as_deref())
                    .collect::<Vec<_>>();
                if rasterlab_render::would_use_gpu_for_batch(&ops, committed_pixels, has_gpu) {
                    saw_gpu = true;
                    index = end;
                    continue;
                }
            }
        }
        if rasterlab_render::would_use_gpu_for_operation(op.as_ref(), committed_pixels, has_gpu) {
            saw_gpu = true;
        } else {
            saw_cpu = true;
        }
        index += 1;
    }
    if let Some(op) = preview_op {
        if rasterlab_render::would_use_gpu_for_operation(op, preview_pixels, has_gpu) {
            saw_gpu = true;
        } else {
            saw_cpu = true;
        }
    }

    match (saw_cpu, saw_gpu) {
        (false, true) => ProcessingBackend::Gpu,
        (true, true) => ProcessingBackend::Mixed,
        _ => ProcessingBackend::Cpu,
    }
}

fn scaled_pixel_count(width: u32, height: u32, scale: f32) -> usize {
    let scaled_width = ((width as f32 * scale) as u32).max(1);
    let scaled_height = ((height as f32 * scale) as u32).max(1);
    scaled_width as usize * scaled_height as usize
}

fn format_processing_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f32();
    if secs < 10.0 {
        format!("{secs:.1} s")
    } else {
        format!("{} s", elapsed.as_secs())
    }
}

impl AppState {
    // -----------------------------------------------------------------------
    // Result handling
    // -----------------------------------------------------------------------

    /// Fold a completed background render back into the visible state.
    pub(super) fn on_render_result(&mut self, result: RenderResult) {
        match result {
            RenderResult::Complete {
                image,
                hist,
                intermediates,
                start_index,
                cache_gen,
                is_preview,
                follow_up_full_res,
                overlay_rect,
            } => {
                self.histogram = Some(*hist);
                self.loading = false;
                self.nr_in_flight = false;
                let reusable_nr_key = self.pending_nr_preview_key.take();

                if let Some(rect) = overlay_rect {
                    self.preview_overlay = Some(image);
                    self.preview_overlay_rect = Some(rect);
                } else {
                    if let Some((copy_index, cursor, key_cache_gen, signature)) = reusable_nr_key
                        && !self.needs_rerender
                        && cache_gen == key_cache_gen
                    {
                        self.reusable_nr_preview = Some(ReusableNrPreview {
                            copy_index,
                            cursor,
                            cache_gen,
                            signature,
                            image: Arc::clone(&image),
                        });
                    }
                    self.rendered = Some(image);
                    self.rendered_is_preview = is_preview;
                    self.rendered_scale = if is_preview { PREVIEW_SCALE } else { 1.0 };
                    if !is_preview {
                        self.preview_overlay = None;
                        self.preview_overlay_rect = None;
                    }
                }

                if !is_preview && overlay_rect.is_none() {
                    let elapsed_ms = self
                        .render_start
                        .take()
                        .map(|t| t.elapsed().as_millis())
                        .unwrap_or(0);
                    self.status = self.render_ready_status("Ready", elapsed_ms);
                    self.render_backend = None;
                    if let Some(pipeline) = self.pipeline_mut()
                        && cache_gen == pipeline.step_cache_gen()
                    {
                        pipeline.store_sparse_steps(start_index, intermediates);
                    }
                } else if !follow_up_full_res {
                    let elapsed_ms = self
                        .render_start
                        .take()
                        .map(|t| t.elapsed().as_millis())
                        .unwrap_or(0);
                    self.status = self.render_ready_status("Preview ready", elapsed_ms);
                    self.render_backend = None;
                }

                if self.needs_rerender {
                    self.needs_rerender = false;
                    self.request_render_inner(false);
                } else if follow_up_full_res && (is_preview || overlay_rect.is_some()) {
                    self.request_render_inner(true);
                }
            }
            RenderResult::Error(e) => {
                self.clear_render_in_flight();
                self.status = format!("Error: {}", e);
            }
            RenderResult::Cancelled => {
                self.clear_render_in_flight();
                self.status = "Cancelled".into();
                if self.needs_rerender {
                    self.needs_rerender = false;
                    self.request_render_inner(false);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Status reporting
    // -----------------------------------------------------------------------

    /// True while a render that includes a noise-reduction op is running.
    /// Used by the tools panel to decide whether to show the NR Cancel button.
    pub fn nr_in_flight(&self) -> bool {
        self.nr_in_flight && self.loading
    }

    pub(super) fn update_processing_status(&mut self) {
        let Some(start) = self.render_start else {
            return;
        };
        let Some(backend) = self.render_backend else {
            return;
        };

        let elapsed = start.elapsed();
        self.status = if elapsed >= Duration::from_secs(1) {
            format!(
                "Processing ({})… {}",
                backend.label(),
                format_processing_elapsed(elapsed)
            )
        } else {
            format!("Processing ({})…", backend.label())
        };
        self.ctx.request_repaint_after(Duration::from_millis(250));
    }

    fn render_ready_status(&self, label: &str, elapsed_ms: u128) -> String {
        match self.render_backend {
            Some(backend) => format!("{label}  ({}, {elapsed_ms} ms)", backend.label()),
            None => format!("{label}  ({elapsed_ms} ms)"),
        }
    }

    // -----------------------------------------------------------------------
    // Reusable noise-reduction preview
    // -----------------------------------------------------------------------

    /// Commit the cached full-resolution NLM preview as a pipeline op without
    /// re-running the (very slow) operation. Returns `false` when the cached
    /// preview does not match the op being pushed, leaving state untouched.
    pub(super) fn try_push_reusable_nr_preview(&mut self, op: &dyn Operation) -> bool {
        if self.tools.current_mask_shape().is_some() {
            return false;
        }
        let Some(nr) = op
            .as_any()
            .and_then(|any| any.downcast_ref::<NoiseReductionOp>())
            .filter(|nr| nr.method == NrMethod::NonLocalMeans)
        else {
            return false;
        };
        let Some(preview) = self.reusable_nr_preview.as_ref() else {
            return false;
        };
        let Some(store) = self.copies.as_ref() else {
            return false;
        };
        let pipeline = store.active_pipeline();
        if preview.copy_index != store.active_index()
            || preview.cursor != pipeline.cursor()
            || preview.cache_gen != pipeline.step_cache_gen()
            || preview.signature != NrPreviewSignature::from_op(nr)
        {
            return false;
        }

        let image = Arc::clone(&preview.image);
        self.tools.cancel_all_previews();
        self.preview_overlay = None;
        self.preview_overlay_rect = None;
        self.pending_nr_preview_key = None;
        self.reusable_nr_preview = None;

        if let Some(store) = &mut self.copies {
            let pipeline = store.active_pipeline_mut();
            let start_index = pipeline.cursor();
            pipeline.push_op(op.clone_box());
            pipeline.store_steps(start_index, vec![Arc::clone(&image)]);
        }
        self.rendered = Some(image);
        self.rendered_is_preview = false;
        self.rendered_scale = 1.0;
        self.mark_dirty();
        self.status = "Applied Noise Reduction from preview".into();
        true
    }

    // -----------------------------------------------------------------------
    // Dispatch
    // -----------------------------------------------------------------------

    /// Kick off a background render of the current pipeline.
    ///
    /// When a tool preview is active this renders at [`PREVIEW_SCALE`] so that
    /// slider feedback is immediate, then automatically queues a full-res render
    /// once the preview is displayed.
    pub fn request_render(&mut self) {
        self.request_render_inner(false);
    }

    pub fn cancel_render(&mut self) {
        self.tools.cancel_all_previews();
        self.preview_overlay = None;
        self.preview_overlay_rect = None;
        self.pending_nr_preview_key = None;
        self.reusable_nr_preview = None;
        self.needs_rerender = false;
        self.render_backend = None;
        if self.loading {
            core_cancel::request();
            self.status = "Cancelling...".into();
            self.ctx.request_repaint();
        } else {
            self.status = "Cancelled".into();
        }
    }

    /// `force_full_res` bypasses the downsampled-preview path even when a
    /// preview op is active.  Used internally to follow up a preview render
    /// with a full-resolution render.
    pub(super) fn request_render_inner(&mut self, force_full_res: bool) {
        if self.copies.is_none() {
            return;
        }
        if self.loading {
            // Another render is in-flight; mark dirty so we re-render after it.
            self.needs_rerender = true;
            return;
        }

        // Preview op — applied on top of committed result but NOT cached.
        let preview_op: Option<Box<dyn Operation>> = self.tools.preview_op().map(|preview| {
            let edit_mask = self.editing.and_then(|session| {
                self.pipeline()
                    .and_then(|pipeline| pipeline.ops().get(session.op_index))
                    .and_then(|entry| entry.operation.as_any())
                    .and_then(|any| any.downcast_ref::<MaskedOp>())
                    .map(|masked| masked.mask.clone())
            });
            if let Some(mask) = edit_mask {
                Box::new(MaskedOp {
                    inner: preview,
                    mask,
                }) as Box<dyn Operation>
            } else {
                preview
            }
        });
        let reusable_nr_signature = preview_op
            .as_deref()
            .map(|op| {
                op.as_any()
                    .and_then(|any| any.downcast_ref::<MaskedOp>())
                    .map_or(op, |masked| masked.inner.as_ref())
            })
            .and_then(|op| op.as_any())
            .and_then(|any| any.downcast_ref::<NoiseReductionOp>())
            .filter(|nr| nr.method == NrMethod::NonLocalMeans)
            .map(NrPreviewSignature::from_op);
        let reusable_nr_preview = reusable_nr_signature.is_some() && !force_full_res;

        // Render at reduced scale when a preview op is active so ops run on
        // a fraction of the pixels (~16× fewer at 25%). Manual NLM previews
        // are full-resolution so Apply can reuse the exact result.
        let preview_requested = self.tools.any_preview_active() && !force_full_res;
        let is_preview = preview_requested && !reusable_nr_preview;
        let preview_scale = if is_preview {
            Some(PREVIEW_SCALE)
        } else {
            None
        };

        // Collect all pipeline-derived data in a scoped borrow so the borrow
        // is dropped before we call self methods below.
        let (start_idx, cache_gen, committed_ops, pipeline_cursor) = {
            let pipeline = self.pipeline().unwrap();
            let (si, _) = pipeline.best_cached_start();
            let cg = pipeline.step_cache_gen();
            let co: Vec<Option<Box<dyn Operation>>> = pipeline.ops()[si..pipeline.cursor()]
                .iter()
                .map(|e| {
                    if e.enabled {
                        Some(e.operation.clone_box())
                    } else {
                        None
                    }
                })
                .collect();
            (si, cg, co, pipeline.cursor())
        };

        // Obtain the starting image for the render thread.
        //
        // For committed full-resolution renders, vacate the cache slot so the
        // render thread gets the sole Arc reference and can avoid a deep clone.
        // Preview results are never written into the committed pipeline cache,
        // so both reduced and full-resolution previews keep its best image in
        // place. Tools can then reliably use the pre-preview dimensions when
        // constructing follow-up geometric operations such as auto-crop.
        let start_image = if is_preview || preview_op.is_some() {
            self.pipeline().unwrap().best_cached_start().1
        } else {
            self.pipeline_mut().unwrap().take_start_for_render().1
        };

        let follow_up_full_res = preview_op.is_some() && reusable_nr_signature.is_none();

        // Track whether the upcoming render involves noise reduction so the UI
        // can show a Cancel button while the (potentially slow) NLM runs.
        let nr_in_flight = preview_op
            .as_deref()
            .is_some_and(|op| op.name() == "noise_reduction")
            || committed_ops
                .iter()
                .flatten()
                .any(|op| op.name() == "noise_reduction");

        // Clear any cancel request left over from a previous render.
        core_cancel::reset();

        // Use the overlay path when the entire pipeline is cached (committed_ops
        // is empty) and we have a known viewport — run the preview op only on the
        // visible pixels at full resolution, return as an overlay.
        let all_cached = start_idx >= pipeline_cursor;
        let overlay_viewport = if is_preview && all_cached {
            self.preview_viewport
        } else {
            None
        };
        // Fall back to downsampled-blit if overlay path isn't available.
        let preview_viewport = if is_preview && overlay_viewport.is_none() {
            self.preview_viewport
        } else {
            None
        };

        let render_backend = estimate_processing_backend(
            start_image.as_ref(),
            &committed_ops,
            preview_op.as_deref(),
            preview_scale,
            preview_viewport,
            overlay_viewport,
            self.gpu.is_some(),
        );

        self.loading = true;
        self.nr_in_flight = nr_in_flight;
        self.render_start = Some(std::time::Instant::now());
        self.render_backend = Some(render_backend);
        self.update_processing_status();
        self.pending_nr_preview_key = reusable_nr_signature.and_then(|signature| {
            self.copies
                .as_ref()
                .map(|store| (store.active_index(), pipeline_cursor, cache_gen, signature))
        });

        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();

        let request = RenderRequest {
            start_image,
            committed_ops,
            preview_op,
            preview_scale,
            preview_viewport,
            overlay_viewport,
            gpu: self.gpu.clone(),
        };
        let meta = RenderMeta {
            start_index: start_idx,
            cache_gen,
            is_preview,
            follow_up_full_res,
        };
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        if let Err(e) = rasterlab_render::spawn_render(request, meta, tx, repaint) {
            // Nothing was sent and nothing will be, so this is the only chance
            // to release the in-flight state set just above. Leaving `loading`
            // set would make every later render request a silent no-op.
            self.clear_render_in_flight();
            self.status = format!("Error: could not start the render thread: {e}");
        }
    }

    /// Release the state that marks a render as running, discarding whatever
    /// the run would have produced.
    ///
    /// Used by the terminal paths with nothing to show for the work — error,
    /// cancellation, and a render thread that never started. Completion clears
    /// the same fields itself, but consumes `render_start` and
    /// `pending_nr_preview_key` on the way rather than dropping them.
    fn clear_render_in_flight(&mut self) {
        self.loading = false;
        self.nr_in_flight = false;
        self.render_start = None;
        self.render_backend = None;
        self.pending_nr_preview_key = None;
    }
}
