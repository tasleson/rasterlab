use std::sync::{Arc, mpsc};

use egui::Context;
use rasterlab_core::{
    Image,
    formats::FormatRegistry,
    ops::{BlackAndWhiteOp, CropOp, HistogramData, LevelsOp, RotateOp, SharpenOp},
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
    RenderComplete(Arc<Image>, Box<HistogramData>),
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
                BgMessage::RenderComplete(img, hist) => {
                    self.histogram = Some(*hist);
                    self.rendered = Some(img);
                    self.loading = false;
                    let elapsed_ms = self
                        .render_start
                        .take()
                        .map(|t| t.elapsed().as_millis())
                        .unwrap_or(0);
                    self.status = format!("Ready  ({} ms)", elapsed_ms);
                    // Re-render if a slider changed while this render was in-flight.
                    if self.needs_rerender {
                        self.needs_rerender = false;
                        self.request_render();
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

    pub fn push_bw(&mut self) {
        let op: Box<dyn Operation> = match self.bw_mode_idx {
            1 => Box::new(BlackAndWhiteOp::average()),
            2 => Box::new(BlackAndWhiteOp::perceptual()),
            _ => Box::new(BlackAndWhiteOp::luminance()),
        };
        self.push_op(op);
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
    /// The pipeline is NOT moved to the thread — instead we serialise the active
    /// operations to JSON (cheap: just parameters, no pixels) and deserialise
    /// them in the worker.  The source image is shared via `Arc`.
    pub fn request_render(&mut self) {
        let Some(pipeline) = &self.pipeline else {
            return;
        };
        if self.loading {
            // Another render is in-flight; mark dirty so we re-render after it.
            self.needs_rerender = true;
            return;
        }

        // Snapshot active ops as JSON — very cheap
        let source = Arc::clone(pipeline.source());
        let mut ops_json: Vec<serde_json::Value> = pipeline
            .ops()
            .iter()
            .take(pipeline.cursor())
            .filter(|e| e.enabled)
            .filter_map(|e| serde_json::to_value(&e.operation).ok())
            .collect();

        // Append preview levels op if active (not yet committed to pipeline)
        if self.levels_preview_active {
            let preview: Box<dyn Operation> = Box::new(LevelsOp::new(
                self.levels_black,
                self.levels_white,
                self.levels_mid,
            ));
            if let Ok(v) = serde_json::to_value(&preview) {
                ops_json.push(v);
            }
        }

        self.loading = true;
        self.status = "Rendering…".into();
        self.render_start = Some(std::time::Instant::now());

        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();

        std::thread::Builder::new()
            .name("rasterlab-render".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let msg = match render_in_thread(source, ops_json) {
                    Ok((img, hist)) => BgMessage::RenderComplete(Arc::new(img), Box::new(hist)),
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
// Free function: runs in the render thread
// ---------------------------------------------------------------------------

fn render_in_thread(
    source: Arc<Image>,
    ops_json: Vec<serde_json::Value>,
) -> Result<(Image, HistogramData), String> {
    let mut current = source.deep_clone();

    for json_val in ops_json {
        // Deserialise operation (only parameters, no pixel data)
        let op: Box<dyn Operation> =
            serde_json::from_value(json_val).map_err(|e| format!("Deserialise op: {}", e))?;

        current = op
            .apply(&current)
            .map_err(|e| format!("Op '{}' failed: {}", op.name(), e))?;
    }

    // Compute histogram in this thread so the main thread never does heavy work.
    let hist = HistogramData::compute(&current);
    Ok((current, hist))
}
