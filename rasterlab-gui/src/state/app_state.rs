use std::sync::{Arc, mpsc};

use egui::Context;
use rasterlab_core::{
    Image,
    formats::FormatRegistry,
    ops::{
        BlackAndWhiteOp, BrightnessContrastOp, CropOp, CurvesOp, FlipOp, HistogramData, LevelsOp,
        RotateOp, SaturationOp, SharpenOp, VignetteOp,
    },
    pipeline::EditPipeline,
    traits::format_handler::EncodeOptions,
    traits::operation::Operation,
};

// ---------------------------------------------------------------------------
// Background-thread messaging
// ---------------------------------------------------------------------------

enum BgMessage {
    ImageLoaded {
        path: std::path::PathBuf,
        image: Image,
    },
    /// Render finished; histogram was also computed in the same thread.
    RenderComplete {
        image: Arc<Image>,
        hist: Box<HistogramData>,
        /// One image per op in `ops[start_index..cursor]`.  Entry `k` is the
        /// image state after op `start_index + k` was processed (unchanged for
        /// disabled ops).  Safe to store into the pipeline step cache only when
        /// `cache_gen` still matches `pipeline.step_cache_gen()`.
        intermediates: Vec<Arc<Image>>,
        /// The op index from which this render started.
        start_index: usize,
        /// Snapshot of `pipeline.step_cache_gen()` taken when the render began.
        cache_gen: u64,
        /// True when this was a downsampled preview render.  A full-res render
        /// will be queued automatically after this result is displayed.
        is_preview: bool,
    },
    Error(String),
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

pub struct AppState {
    pub registry: FormatRegistry,
    pub pipeline: Option<EditPipeline>,
    pub rendered: Option<Arc<Image>>,
    pub histogram: Option<HistogramData>,
    pub loading: bool,
    pub status: String,
    pub last_path: Option<std::path::PathBuf>,
    pub encode_opts: EncodeOptions,
    /// Incremented each time a new file is opened. Canvas uses this to know
    /// when to reset zoom/pan vs. just updating the texture.
    pub image_generation: u64,

    // Background thread channel
    bg_tx: mpsc::Sender<BgMessage>,
    bg_rx: mpsc::Receiver<BgMessage>,
    // egui context — needed to wake up the UI after background work completes
    ctx: Context,

    // ── Tool panel inputs ─────────────────────────────────────────────────
    pub crop_x: u32,
    pub crop_y: u32,
    pub crop_w: u32,
    pub crop_h: u32,
    pub rotate_deg: f32,
    pub sharpen_strength: f32,
    pub bw_mode_idx: usize,
    /// Channel mixer weights for the ChannelMixer B&W mode.
    pub bw_mixer_r: f32,
    pub bw_mixer_g: f32,
    pub bw_mixer_b: f32,
    /// When true, a BlackAndWhiteOp preview is appended to each render.
    pub bw_preview_active: bool,

    // ── Brightness / Contrast tool ────────────────────────────────────────
    pub bc_brightness: f32,
    pub bc_contrast: f32,
    pub bc_preview_active: bool,

    // ── Saturation tool ───────────────────────────────────────────────────
    pub saturation: f32,
    pub sat_preview_active: bool,

    // ── Curves tool ───────────────────────────────────────────────────────
    /// Control points `[input, output]` in `[0,1]`, sorted by input.
    pub curve_points: Vec<[f32; 2]>,
    pub curve_preview_active: bool,
    /// Index of the control point currently being dragged in the curve editor.
    pub curve_dragging_idx: Option<usize>,

    // ── Vignette tool ─────────────────────────────────────────────────────
    pub vignette_strength: f32,
    pub vignette_radius: f32,
    pub vignette_feather: f32,
    /// When true, a VignetteOp preview is appended to each render.
    pub vignette_preview_active: bool,

    // ── Levels tool ───────────────────────────────────────────────────────
    /// Live slider values for the levels tool (not yet committed to pipeline).
    pub levels_black: f32,
    pub levels_mid: f32,
    pub levels_white: f32,
    /// When true, a LevelsOp preview is appended to each render.
    pub levels_preview_active: bool,
    /// Set when a slider changes while a render is in-flight; triggers a
    /// follow-up render as soon as the current one completes.
    needs_rerender: bool,
    /// Wall-clock time at which the most recent render thread was spawned.
    render_start: Option<std::time::Instant>,
}

impl AppState {
    pub fn new(ctx: Context) -> Self {
        let (bg_tx, bg_rx) = mpsc::channel();
        Self {
            registry: FormatRegistry::with_builtins(),
            pipeline: None,
            rendered: None,
            histogram: None,
            loading: false,
            status: "Welcome to RasterLab — open an image to begin.".into(),
            last_path: None,
            encode_opts: EncodeOptions::default(),
            image_generation: 0,
            bg_tx,
            bg_rx,
            ctx,
            crop_x: 0,
            crop_y: 0,
            crop_w: 0,
            crop_h: 0,
            rotate_deg: 0.0,
            sharpen_strength: 1.0,
            bw_mode_idx: 0,
            bw_mixer_r: 0.2126,
            bw_mixer_g: 0.7152,
            bw_mixer_b: 0.0722,
            bw_preview_active: false,
            bc_brightness: 0.0,
            bc_contrast: 0.0,
            bc_preview_active: false,
            saturation: 1.0,
            sat_preview_active: false,
            curve_points: vec![[0.0, 0.0], [1.0, 1.0]],
            curve_preview_active: false,
            curve_dragging_idx: None,
            vignette_strength: 0.5,
            vignette_radius: 0.65,
            vignette_feather: 0.5,
            vignette_preview_active: false,
            levels_black: 0.0,
            levels_mid: 1.0,
            levels_white: 1.0,
            levels_preview_active: false,
            needs_rerender: false,
            render_start: None,
        }
    }

    // -----------------------------------------------------------------------
    // Background message pump — call once per frame from update()
    // -----------------------------------------------------------------------

    pub fn poll_background(&mut self) {
        while let Ok(msg) = self.bg_rx.try_recv() {
            match msg {
                BgMessage::ImageLoaded { path, image } => {
                    let w = image.width;
                    let h = image.height;
                    self.crop_w = w;
                    self.crop_h = h;
                    self.last_path = Some(path.clone());
                    self.status = format!("Opened {}  ({}×{})", path.display(), w, h);
                    self.pipeline = Some(EditPipeline::new(image));
                    self.loading = false;
                    self.image_generation += 1;
                    // Kick off initial render
                    self.request_render();
                }
                BgMessage::RenderComplete {
                    image,
                    hist,
                    intermediates,
                    start_index,
                    cache_gen,
                    is_preview,
                } => {
                    self.histogram = Some(*hist);
                    self.rendered = Some(image);
                    self.loading = false;

                    if !is_preview {
                        // Only report timing and populate the step cache for
                        // full-res renders; preview intermediates are low-res.
                        let elapsed_ms = self
                            .render_start
                            .take()
                            .map(|t| t.elapsed().as_millis())
                            .unwrap_or(0);
                        self.status = format!("Ready  ({} ms)", elapsed_ms);
                        if let Some(pipeline) = &mut self.pipeline
                            && cache_gen == pipeline.step_cache_gen()
                        {
                            pipeline.store_steps(start_index, intermediates);
                        }
                    }

                    if self.needs_rerender {
                        // Slider changed again while this render was in-flight;
                        // start a fresh preview cycle.
                        self.needs_rerender = false;
                        self.request_render_inner(false);
                    } else if is_preview {
                        // Preview displayed — follow up with a full-res render.
                        self.request_render_inner(true);
                    }
                }
                BgMessage::Error(e) => {
                    self.status = format!("Error: {}", e);
                    self.loading = false;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // File I/O
    // -----------------------------------------------------------------------

    /// Begin loading `path` in a background thread.
    pub fn open_file(&mut self, path: std::path::PathBuf) {
        self.loading = true;
        self.status = format!("Loading {}…", path.display());

        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();

        std::thread::Builder::new()
            .name("rasterlab-load".into())
            .stack_size(32 * 1024 * 1024) // 32 MiB — some RAW decoders are deep
            .spawn(move || {
                let registry = FormatRegistry::with_builtins();
                let msg = match registry.decode_file(&path) {
                    Ok(image) => BgMessage::ImageLoaded { path, image },
                    Err(e) => BgMessage::Error(e.to_string()),
                };
                let _ = tx.send(msg);
                ctx.request_repaint();
            })
            .expect("failed to spawn load thread");
    }

    pub fn save_file(&mut self, path: std::path::PathBuf) {
        let Some(rendered) = &self.rendered else {
            self.status = "Nothing to save — render first".into();
            return;
        };
        match self
            .registry
            .encode_file(rendered, &path, &self.encode_opts)
        {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&path, &bytes) {
                    self.status = format!("Write failed: {}", e);
                } else {
                    self.status = format!("Saved {} bytes → {}", bytes.len(), path.display());
                }
            }
            Err(e) => {
                self.status = format!("Encode failed: {}", e);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Pipeline mutations (always followed by request_render)
    // -----------------------------------------------------------------------

    pub fn push_crop(&mut self) {
        self.push_op(Box::new(CropOp::new(
            self.crop_x,
            self.crop_y,
            self.crop_w,
            self.crop_h,
        )));
    }
    pub fn push_rotate_arbitrary(&mut self) {
        self.push_op(Box::new(RotateOp::arbitrary(self.rotate_deg)));
    }
    pub fn push_rotate_90(&mut self) {
        self.push_op(Box::new(RotateOp::cw90()));
    }
    pub fn push_rotate_180(&mut self) {
        self.push_op(Box::new(RotateOp::cw180()));
    }
    pub fn push_rotate_270(&mut self) {
        self.push_op(Box::new(RotateOp::cw270()));
    }
    pub fn push_sharpen(&mut self) {
        self.push_op(Box::new(SharpenOp::new(self.sharpen_strength)));
    }

    pub fn push_flip_horizontal(&mut self) {
        self.push_op(Box::new(FlipOp::horizontal()));
    }

    pub fn push_flip_vertical(&mut self) {
        self.push_op(Box::new(FlipOp::vertical()));
    }

    pub fn update_bc_preview(&mut self) {
        self.bc_preview_active = true;
        self.request_render();
    }

    pub fn cancel_bc_preview(&mut self) {
        if self.bc_preview_active {
            self.bc_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_bc(&mut self) {
        self.bc_preview_active = false;
        self.push_op(Box::new(BrightnessContrastOp::new(
            self.bc_brightness,
            self.bc_contrast,
        )));
        self.bc_brightness = 0.0;
        self.bc_contrast = 0.0;
    }

    pub fn reset_bc(&mut self) {
        self.bc_brightness = 0.0;
        self.bc_contrast = 0.0;
        self.cancel_bc_preview();
    }

    pub fn update_sat_preview(&mut self) {
        self.sat_preview_active = true;
        self.request_render();
    }

    pub fn cancel_sat_preview(&mut self) {
        if self.sat_preview_active {
            self.sat_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_saturation(&mut self) {
        self.sat_preview_active = false;
        self.push_op(Box::new(SaturationOp::new(self.saturation)));
        self.saturation = 1.0;
    }

    pub fn reset_saturation(&mut self) {
        self.saturation = 1.0;
        self.cancel_sat_preview();
    }

    pub fn update_curve_preview(&mut self) {
        self.curve_preview_active = true;
        self.request_render();
    }

    pub fn cancel_curve_preview(&mut self) {
        if self.curve_preview_active {
            self.curve_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_curves(&mut self) {
        self.curve_preview_active = false;
        self.push_op(Box::new(CurvesOp {
            points: self.curve_points.clone(),
        }));
        self.curve_points = vec![[0.0, 0.0], [1.0, 1.0]];
    }

    pub fn reset_curves(&mut self) {
        self.curve_points = vec![[0.0, 0.0], [1.0, 1.0]];
        self.curve_dragging_idx = None;
        self.cancel_curve_preview();
    }

    pub fn update_vignette_preview(&mut self) {
        self.vignette_preview_active = true;
        self.request_render();
    }

    pub fn cancel_vignette_preview(&mut self) {
        if self.vignette_preview_active {
            self.vignette_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_vignette(&mut self) {
        self.vignette_preview_active = false;
        self.push_op(Box::new(VignetteOp::new(
            self.vignette_strength,
            self.vignette_radius,
            self.vignette_feather,
        )));
    }
    /// Update the live levels preview and trigger a re-render.
    /// Call this whenever a levels slider changes.
    pub fn update_levels_preview(&mut self) {
        self.levels_preview_active = true;
        self.request_render();
    }

    /// Commit the current levels settings as a permanent pipeline operation.
    pub fn apply_levels(&mut self) {
        self.levels_preview_active = false;
        self.push_op(Box::new(LevelsOp::new(
            self.levels_black,
            self.levels_white,
            self.levels_mid,
        )));
        // Reset sliders for the next use
        self.levels_black = 0.0;
        self.levels_mid = 1.0;
        self.levels_white = 1.0;
    }

    /// Discard the live levels preview without committing.
    pub fn reset_levels(&mut self) {
        self.levels_black = 0.0;
        self.levels_mid = 1.0;
        self.levels_white = 1.0;
        if self.levels_preview_active {
            self.levels_preview_active = false;
            self.request_render();
        }
    }

    /// Show a live 1/4-scale preview of the selected B&W mode.
    /// Call this whenever the mode combobox changes.
    pub fn update_bw_preview(&mut self) {
        self.bw_preview_active = true;
        self.request_render();
    }

    /// Discard the live B&W preview without committing.
    pub fn cancel_bw_preview(&mut self) {
        if self.bw_preview_active {
            self.bw_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_bw(&mut self) {
        self.bw_preview_active = false;
        let op: Box<dyn Operation> = self.make_bw_op();
        self.push_op(op);
    }

    fn make_bw_op(&self) -> Box<dyn Operation> {
        match self.bw_mode_idx {
            1 => Box::new(BlackAndWhiteOp::average()),
            2 => Box::new(BlackAndWhiteOp::perceptual()),
            3 => Box::new(BlackAndWhiteOp::channel_mixer(
                self.bw_mixer_r,
                self.bw_mixer_g,
                self.bw_mixer_b,
            )),
            _ => Box::new(BlackAndWhiteOp::luminance()),
        }
    }

    pub fn remove_op(&mut self, index: usize) {
        if self.pipeline.as_mut().is_some_and(|p| p.remove_op(index)) {
            self.request_render();
        }
    }
    pub fn reorder_op(&mut self, from: usize, to: usize) {
        if self
            .pipeline
            .as_mut()
            .is_some_and(|p| p.reorder_op(from, to))
        {
            self.request_render();
        }
    }
    pub fn toggle_op(&mut self, index: usize) {
        if self.pipeline.as_mut().is_some_and(|p| p.toggle_op(index)) {
            self.request_render();
        }
    }
    pub fn undo(&mut self) {
        if self.pipeline.as_mut().is_some_and(|p| p.undo()) {
            self.request_render();
        }
    }
    pub fn redo(&mut self) {
        if self.pipeline.as_mut().is_some_and(|p| p.redo()) {
            self.request_render();
        }
    }

    fn push_op(&mut self, op: Box<dyn Operation>) {
        if let Some(p) = &mut self.pipeline {
            p.push_op(op);
            self.request_render();
        }
    }

    // -----------------------------------------------------------------------
    // Background rendering
    // -----------------------------------------------------------------------

    /// Kick off a background render of the current pipeline.
    ///
    /// When `levels_preview_active` this renders at [`PREVIEW_SCALE`] so that
    /// slider feedback is immediate, then automatically queues a full-res render
    /// once the preview is displayed.
    pub fn request_render(&mut self) {
        self.request_render_inner(false);
    }

    /// `force_full_res` bypasses the downsampled-preview path even when a
    /// preview op is active.  Used internally to follow up a preview render
    /// with a full-resolution render.
    fn request_render_inner(&mut self, force_full_res: bool) {
        let Some(pipeline) = &self.pipeline else {
            return;
        };
        if self.loading {
            // Another render is in-flight; mark dirty so we re-render after it.
            self.needs_rerender = true;
            return;
        }

        // Render at reduced scale when a preview op is active so ops run on
        // a fraction of the pixels (~16× fewer at 25%).  Full-res renders are
        // queued automatically once the preview is displayed.
        let is_preview = (self.levels_preview_active
            || self.bw_preview_active
            || self.vignette_preview_active
            || self.bc_preview_active
            || self.sat_preview_active
            || self.curve_preview_active)
            && !force_full_res;
        let preview_scale = if is_preview {
            Some(PREVIEW_SCALE)
        } else {
            None
        };

        // Find the best cached starting point — may skip all committed ops
        // if the full pipeline result is already in the cache.
        let (start_idx, start_image) = pipeline.best_cached_start();
        let cache_gen = pipeline.step_cache_gen();

        // Committed ops from start_idx to cursor.  `None` entries represent
        // disabled ops (image passes through unchanged).
        let committed_ops: Vec<Option<serde_json::Value>> = pipeline.ops()
            [start_idx..pipeline.cursor()]
            .iter()
            .map(|e| {
                if e.enabled {
                    serde_json::to_value(&e.operation).ok()
                } else {
                    None
                }
            })
            .collect();

        // Preview op — applied on top of committed result but NOT cached.
        // Levels takes priority if both previews are somehow active simultaneously.
        let preview_op = if self.levels_preview_active {
            let preview: Box<dyn Operation> = Box::new(LevelsOp::new(
                self.levels_black,
                self.levels_white,
                self.levels_mid,
            ));
            serde_json::to_value(&preview).ok()
        } else if self.bw_preview_active {
            serde_json::to_value(self.make_bw_op()).ok()
        } else if self.bc_preview_active {
            let preview: Box<dyn Operation> = Box::new(BrightnessContrastOp::new(
                self.bc_brightness,
                self.bc_contrast,
            ));
            serde_json::to_value(&preview).ok()
        } else if self.sat_preview_active {
            let preview: Box<dyn Operation> = Box::new(SaturationOp::new(self.saturation));
            serde_json::to_value(&preview).ok()
        } else if self.curve_preview_active {
            let preview: Box<dyn Operation> = Box::new(CurvesOp {
                points: self.curve_points.clone(),
            });
            serde_json::to_value(&preview).ok()
        } else if self.vignette_preview_active {
            let preview: Box<dyn Operation> = Box::new(VignetteOp::new(
                self.vignette_strength,
                self.vignette_radius,
                self.vignette_feather,
            ));
            serde_json::to_value(&preview).ok()
        } else {
            None
        };

        self.loading = true;
        self.status = "Rendering…".into();
        self.render_start = Some(std::time::Instant::now());

        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();

        std::thread::Builder::new()
            .name("rasterlab-render".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let msg =
                    match render_in_thread(start_image, committed_ops, preview_op, preview_scale) {
                        Ok((image, hist, intermediates)) => BgMessage::RenderComplete {
                            image,
                            hist: Box::new(hist),
                            intermediates,
                            start_index: start_idx,
                            cache_gen,
                            is_preview,
                        },
                        Err(e) => BgMessage::Error(e),
                    };
                let _ = tx.send(msg);
                ctx.request_repaint();
            })
            .expect("failed to spawn render thread");
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    pub fn can_undo(&self) -> bool {
        self.pipeline.as_ref().is_some_and(|p| p.can_undo())
    }
    pub fn can_redo(&self) -> bool {
        self.pipeline.as_ref().is_some_and(|p| p.can_redo())
    }
}

// ---------------------------------------------------------------------------
// Free functions: run in the render thread
// ---------------------------------------------------------------------------

/// Linear scale factor used for the fast downsampled preview.
/// 0.25 = 1/4 width × 1/4 height = 1/16 the pixels → ~16× faster ops.
const PREVIEW_SCALE: f32 = 0.25;

type RenderResult = Result<(Arc<Image>, HistogramData, Vec<Arc<Image>>), String>;

/// Nearest-neighbour downsample via rayon row-parallel copy.
fn downsample_nn(img: &Image, scale: f32) -> Image {
    use rayon::prelude::*;
    let new_w = ((img.width as f32 * scale) as u32).max(1);
    let new_h = ((img.height as f32 * scale) as u32).max(1);
    let mut out = Image::new(new_w, new_h);
    let x_ratio = img.width as f32 / new_w as f32;
    let y_ratio = img.height as f32 / new_h as f32;
    let src_w = img.width as usize;
    out.data
        .par_chunks_mut(new_w as usize * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let src_y = (y as f32 * y_ratio) as usize;
            for x in 0..new_w as usize {
                let src_x = (x as f32 * x_ratio) as usize;
                let src_off = (src_y * src_w + src_x) * 4;
                let dst_off = x * 4;
                row[dst_off..dst_off + 4].copy_from_slice(&img.data[src_off..src_off + 4]);
            }
        });
    out
}

/// Run committed ops then an optional preview op.
///
/// When `preview_scale` is `Some(s)`, the starting image is downsampled to
/// `s` of its original dimensions before any ops run — all work then happens
/// on a small image, so the result is fast but low-resolution.  Intermediates
/// are not returned for preview renders (they are low-res and must not be
/// stored in the full-res step cache).
fn render_in_thread(
    start_image: Arc<Image>,
    committed_ops: Vec<Option<serde_json::Value>>,
    preview_op: Option<serde_json::Value>,
    preview_scale: Option<f32>,
) -> RenderResult {
    // Downsample if this is a preview render; otherwise use the image as-is.
    let mut current: Arc<Image> = match preview_scale {
        Some(scale) => Arc::new(downsample_nn(start_image.as_ref(), scale)),
        None => start_image,
    };
    // Intermediates are only collected for full-res renders.
    let mut intermediates = if preview_scale.is_none() {
        Vec::with_capacity(committed_ops.len())
    } else {
        Vec::new()
    };

    for op_json in committed_ops {
        if let Some(json) = op_json {
            let op: Box<dyn Operation> =
                serde_json::from_value(json).map_err(|e| format!("Deserialise op: {}", e))?;
            current = Arc::new(
                op.apply(current.as_ref())
                    .map_err(|e| format!("Op '{}' failed: {}", op.name(), e))?,
            );
        }
        if preview_scale.is_none() {
            // Record state at this pipeline position (Arc clone — no pixel copy for disabled ops).
            intermediates.push(Arc::clone(&current));
        }
    }

    // Apply preview op on top of committed result without caching it.
    if let Some(json) = preview_op {
        let op: Box<dyn Operation> =
            serde_json::from_value(json).map_err(|e| format!("Deserialise preview op: {}", e))?;
        current = Arc::new(
            op.apply(current.as_ref())
                .map_err(|e| format!("Op '{}' (preview) failed: {}", op.name(), e))?,
        );
    }

    // Compute histogram in this thread so the main thread never does heavy work.
    let hist = HistogramData::compute(current.as_ref());
    Ok((current, hist, intermediates))
}
