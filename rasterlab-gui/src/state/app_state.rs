use std::sync::{Arc, mpsc};

use crate::prefs::Prefs;

use egui::Context;
use rasterlab_core::{
    Image,
    formats::FormatRegistry,
    ops::{
        BlackAndWhiteOp, BlurOp, BrightnessContrastOp, ColorBalanceOp, ColorSpaceConversion,
        ColorSpaceOp, CropOp, CurvesOp, DenoiseOp, FauxHdrOp, FlipOp, GrainOp, HighlightsShadowsOp,
        HistogramData, HslPanelOp, HueShiftOp, LevelsOp, LutOp, PerspectiveOp, ResampleMode,
        ResizeOp, RotateOp, SaturationOp, SepiaOp, SharpenOp, SplitToneOp, VibranceOp, VignetteOp,
        WhiteBalanceOp,
    },
    pipeline::EditPipeline,
    project::{RlabFile, RlabMeta},
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
        /// Verbatim bytes of the source file, kept for future `.rlab` saves.
        original_bytes: Vec<u8>,
    },
    /// A `.rlab` project file was successfully decoded.
    ProjectLoaded {
        path: std::path::PathBuf,
        rlab: Box<RlabFile>,
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
    /// Persistent GUI preferences (tool panel open/closed states, etc.).
    pub prefs: Prefs,
    pub registry: FormatRegistry,
    pub pipeline: Option<EditPipeline>,
    pub rendered: Option<Arc<Image>>,
    pub histogram: Option<HistogramData>,
    pub loading: bool,
    pub status: String,
    pub last_path: Option<std::path::PathBuf>,
    /// Verbatim bytes of the currently loaded source file (for `.rlab` saves).
    pub original_bytes: Option<Vec<u8>>,
    /// Path of the open `.rlab` project file.  `None` when an image was opened
    /// directly and has not yet been saved as a project.
    pub project_path: Option<std::path::PathBuf>,
    /// `true` when there are unsaved changes since the last project save.
    pub is_dirty: bool,
    /// `created_at` timestamp from the last project load/save, preserved on
    /// in-place re-saves so the original creation date is not lost.
    pub project_created_at: Option<u64>,
    pub encode_opts: EncodeOptions,
    /// When `true`, apply a resize step before encoding.
    pub export_resize_enabled: bool,
    pub export_resize_w: u32,
    pub export_resize_h: u32,
    pub export_resize_mode: ResampleMode,
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
    pub sharpen_preview_active: bool,
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

    // ── Vibrance tool ─────────────────────────────────────────────────────
    pub vibrance: f32,
    pub vibrance_preview_active: bool,

    // ── Sepia tool ────────────────────────────────────────────────────────
    pub sepia_strength: f32,
    pub sepia_preview_active: bool,

    // ── Split Tone tool ───────────────────────────────────────────────────
    pub split_shadow_hue: f32,
    pub split_shadow_sat: f32,
    pub split_highlight_hue: f32,
    pub split_highlight_sat: f32,
    pub split_balance: f32,
    pub split_preview_active: bool,

    // ── Resize tool ───────────────────────────────────────────────────────
    pub resize_w: u32,
    pub resize_h: u32,
    pub resize_mode: ResampleMode,
    pub resize_lock_aspect: bool,

    // ── Blur tool ─────────────────────────────────────────────────────────
    pub blur_radius: f32,

    // ── Denoise tool ──────────────────────────────────────────────────────
    pub denoise_strength: f32,
    pub denoise_radius: u32,

    // ── Perspective tool ──────────────────────────────────────────────────
    /// Corner offsets `[[tl_x, tl_y], [tr_x, tr_y], [br_x, br_y], [bl_x, bl_y]]`
    /// as fractions of image width/height in `[-1, 1]`.
    pub perspective_corners: [[f32; 2]; 4],

    // ── Color Space Conversion tool ───────────────────────────────────────
    pub color_space_conversion: ColorSpaceConversion,

    // ── LUT tool ──────────────────────────────────────────────────────────
    /// Loaded LUT op, or `None` if no LUT has been loaded.
    pub lut_op: Option<LutOp>,
    /// Blend strength for the loaded LUT.
    pub lut_strength: f32,
    /// Display name of the loaded LUT file.
    pub lut_name: String,
    pub lut_preview_active: bool,
    /// Set to true by the tools panel to ask app.rs to open the LUT file dialog.
    pub lut_dialog_requested: bool,

    // ── Hue Shift tool ────────────────────────────────────────────────────
    pub hue_degrees: f32,
    pub hue_preview_active: bool,

    // ── Highlights & Shadows tool ─────────────────────────────────────────
    pub hl_highlights: f32,
    pub hl_shadows: f32,
    pub hl_preview_active: bool,

    // ── White Balance tool ────────────────────────────────────────────────
    pub wb_temperature: f32,
    pub wb_tint: f32,
    pub wb_preview_active: bool,

    // ── Faux HDR tool ─────────────────────────────────────────────────────
    pub hdr_strength: f32,
    pub hdr_preview_active: bool,

    // ── Grain tool ────────────────────────────────────────────────────────
    pub grain_strength: f32,
    pub grain_size: f32,
    pub grain_seed: u64,
    /// When true, a GrainOp preview is appended to each render (always full-res).
    pub grain_preview_active: bool,

    // ── Color Balance tool ────────────────────────────────────────────────
    /// `[shadows, midtones, highlights]` on each axis.
    pub cb_cyan_red: [f32; 3],
    pub cb_magenta_green: [f32; 3],
    pub cb_yellow_blue: [f32; 3],
    pub cb_preview_active: bool,

    // ── HSL Panel tool ────────────────────────────────────────────────────
    /// Per-band hue shifts in degrees (8 bands: Reds … Magentas).
    pub hsl_hue: [f32; 8],
    pub hsl_sat: [f32; 8],
    pub hsl_lum: [f32; 8],
    pub hsl_preview_active: bool,

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
            prefs: Prefs::load(),
            registry: FormatRegistry::with_builtins(),
            pipeline: None,
            rendered: None,
            histogram: None,
            loading: false,
            status: "Welcome to RasterLab — open an image to begin.".into(),
            last_path: None,
            original_bytes: None,
            project_path: None,
            is_dirty: false,
            project_created_at: None,
            encode_opts: EncodeOptions::default(),
            export_resize_enabled: false,
            export_resize_w: 0,
            export_resize_h: 0,
            export_resize_mode: ResampleMode::Bicubic,
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
            sharpen_preview_active: false,
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
            vibrance: 0.0,
            vibrance_preview_active: false,
            sepia_strength: 1.0,
            sepia_preview_active: false,
            split_shadow_hue: 220.0,
            split_shadow_sat: 0.20,
            split_highlight_hue: 40.0,
            split_highlight_sat: 0.15,
            split_balance: 0.0,
            split_preview_active: false,
            resize_w: 0,
            resize_h: 0,
            resize_mode: ResampleMode::Bicubic,
            resize_lock_aspect: true,
            blur_radius: 2.0,
            denoise_strength: 0.1,
            denoise_radius: 3,
            perspective_corners: [[0.0; 2]; 4],
            color_space_conversion: ColorSpaceConversion::SrgbToDisplayP3,
            lut_op: None,
            lut_strength: 1.0,
            lut_name: String::new(),
            lut_preview_active: false,
            lut_dialog_requested: false,
            hue_degrees: 0.0,
            hue_preview_active: false,
            hl_highlights: 0.0,
            hl_shadows: 0.0,
            hl_preview_active: false,
            wb_temperature: 0.0,
            wb_tint: 0.0,
            wb_preview_active: false,
            vignette_strength: 0.5,
            vignette_radius: 0.65,
            vignette_feather: 0.5,
            vignette_preview_active: false,
            hdr_strength: 0.8,
            hdr_preview_active: false,
            grain_strength: 0.10,
            grain_size: 1.8,
            grain_seed: 42,
            grain_preview_active: false,
            cb_cyan_red: [0.0; 3],
            cb_magenta_green: [0.0; 3],
            cb_yellow_blue: [0.0; 3],
            cb_preview_active: false,
            hsl_hue: [0.0; 8],
            hsl_sat: [0.0; 8],
            hsl_lum: [0.0; 8],
            hsl_preview_active: false,
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
                BgMessage::ImageLoaded {
                    path,
                    image,
                    original_bytes,
                } => {
                    let w = image.width;
                    let h = image.height;
                    self.crop_w = w;
                    self.crop_h = h;
                    self.resize_w = w;
                    self.resize_h = h;
                    self.last_path = Some(path.clone());
                    self.original_bytes = Some(original_bytes);
                    self.project_path = None;
                    self.is_dirty = false;
                    self.project_created_at = None;
                    self.status = format!("Opened {}  ({}×{})", path.display(), w, h);
                    self.pipeline = Some(EditPipeline::new(image));
                    self.loading = false;
                    self.image_generation += 1;
                    self.request_render();
                }
                BgMessage::ProjectLoaded { path, rlab, image } => {
                    let w = image.width;
                    let h = image.height;
                    self.crop_w = w;
                    self.crop_h = h;
                    self.resize_w = w;
                    self.resize_h = h;
                    self.last_path = rlab
                        .meta
                        .source_path
                        .as_deref()
                        .map(std::path::PathBuf::from)
                        .or_else(|| Some(path.clone()));
                    self.project_created_at = Some(rlab.meta.created_at);
                    self.original_bytes = Some(rlab.original_bytes.clone());
                    self.project_path = Some(path.clone());
                    self.is_dirty = false;
                    self.status = format!("Opened {}  ({}×{})", path.display(), w, h);
                    let mut pipeline = EditPipeline::new(image);
                    if let Err(e) = pipeline.load_state(rlab.pipeline_state) {
                        self.status = format!("Warning: could not restore edit stack: {}", e);
                    }
                    self.pipeline = Some(pipeline);
                    self.loading = false;
                    self.image_generation += 1;
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
    ///
    /// Dispatches on the file extension: `.rlab` files are loaded as projects
    /// (restoring the full edit stack); all other files are loaded as source images.
    pub fn open_file(&mut self, path: std::path::PathBuf) {
        self.loading = true;
        self.status = format!("Loading {}…", path.display());

        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();

        let is_project = path
            .extension()
            .map(|e| e.eq_ignore_ascii_case("rlab"))
            .unwrap_or(false);

        std::thread::Builder::new()
            .name("rasterlab-load".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let msg = if is_project {
                    match RlabFile::read(&path) {
                        Ok(rlab) => {
                            let registry = FormatRegistry::with_builtins();
                            let hint = rlab.meta.source_path.as_deref().map(std::path::Path::new);
                            match registry.decode_bytes(&rlab.original_bytes, hint) {
                                Ok(image) => BgMessage::ProjectLoaded {
                                    path,
                                    rlab: Box::new(rlab),
                                    image,
                                },
                                Err(e) => BgMessage::Error(e.to_string()),
                            }
                        }
                        Err(e) => BgMessage::Error(e.to_string()),
                    }
                } else {
                    // Read the raw bytes for storage in .rlab saves, then decode.
                    match std::fs::read(&path) {
                        Ok(original_bytes) => {
                            let registry = FormatRegistry::with_builtins();
                            match registry.decode_file(&path) {
                                Ok(image) => BgMessage::ImageLoaded {
                                    path,
                                    image,
                                    original_bytes,
                                },
                                Err(e) => BgMessage::Error(e.to_string()),
                            }
                        }
                        Err(e) => BgMessage::Error(e.to_string()),
                    }
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

        // Optionally resize before encoding.
        let resized_buf;
        let to_save: &Image =
            if self.export_resize_enabled && self.export_resize_w > 0 && self.export_resize_h > 0 {
                let op = ResizeOp::new(
                    self.export_resize_w,
                    self.export_resize_h,
                    self.export_resize_mode,
                );
                match op.apply(rendered.as_ref()) {
                    Ok(img) => {
                        resized_buf = img;
                        &resized_buf
                    }
                    Err(e) => {
                        self.status = format!("Export resize failed: {}", e);
                        return;
                    }
                }
            } else {
                rendered.as_ref()
            };

        match self.registry.encode_file(to_save, &path, &self.encode_opts) {
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

    /// Export the current edit stack to a JSON file consumable by the CLI.
    ///
    /// The resulting file can be passed to `rasterlab process --load-pipeline <path>`
    /// or `rasterlab batch --load-pipeline <path>` to replay the same edits on
    /// any image without opening the GUI.
    pub fn export_edit_stack_json(&mut self, path: std::path::PathBuf) {
        let Some(pipeline) = &self.pipeline else {
            self.status = "No edit stack to export".into();
            return;
        };
        let state = match pipeline.save_state() {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Export failed: {}", e);
                return;
            }
        };
        let json = match serde_json::to_string_pretty(&state) {
            Ok(j) => j,
            Err(e) => {
                self.status = format!("JSON serialisation failed: {}", e);
                return;
            }
        };
        match std::fs::write(&path, json) {
            Ok(()) => self.status = format!("Edit stack exported → {}", path.display()),
            Err(e) => self.status = format!("Export failed: {}", e),
        }
    }

    /// Save the current project to `path` as a `.rlab` file.
    pub fn save_project(&mut self, path: std::path::PathBuf) {
        let Some(original_bytes) = self.original_bytes.clone() else {
            self.status = "Nothing to save — open an image first".into();
            return;
        };
        let Some(pipeline) = &self.pipeline else {
            self.status = "Nothing to save — no active pipeline".into();
            return;
        };

        let pipeline_state = match pipeline.save_state() {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Save failed (pipeline): {}", e);
                return;
            }
        };

        let source = pipeline.source();
        let (w, h) = (source.width, source.height);
        let source_path = self
            .last_path
            .as_deref()
            .and_then(|p| p.to_str())
            .map(String::from);
        let app_version = env!("CARGO_PKG_VERSION").to_string();

        let mut meta = RlabMeta::new(app_version, source_path, w, h);
        // Preserve the original creation timestamp on in-place re-saves.
        if let Some(created_at) = self.project_created_at {
            meta.created_at = created_at;
        }
        meta = meta.touch();

        let created_at = meta.created_at;
        let rlab = RlabFile::new(meta, original_bytes, pipeline_state, None);
        match rlab.write(&path) {
            Ok(()) => {
                self.project_created_at = Some(created_at);
                self.project_path = Some(path.clone());
                self.is_dirty = false;
                self.status = format!("Saved → {}", path.display());
            }
            Err(e) => {
                self.status = format!("Save failed: {}", e);
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
        self.sharpen_preview_active = false;
        self.push_op(Box::new(SharpenOp::new(self.sharpen_strength)));
    }

    pub fn update_sharpen_preview(&mut self) {
        self.sharpen_preview_active = true;
        self.request_render();
    }

    pub fn cancel_sharpen_preview(&mut self) {
        if self.sharpen_preview_active {
            self.sharpen_preview_active = false;
            self.request_render();
        }
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

    pub fn update_vibrance_preview(&mut self) {
        self.vibrance_preview_active = true;
        self.request_render();
    }

    pub fn cancel_vibrance_preview(&mut self) {
        if self.vibrance_preview_active {
            self.vibrance_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_vibrance(&mut self) {
        self.vibrance_preview_active = false;
        self.push_op(Box::new(VibranceOp::new(self.vibrance)));
        self.vibrance = 0.0;
    }

    pub fn reset_vibrance(&mut self) {
        self.vibrance = 0.0;
        self.cancel_vibrance_preview();
    }

    pub fn push_sepia(&mut self) {
        self.sepia_preview_active = false;
        self.push_op(Box::new(SepiaOp::new(self.sepia_strength)));
        self.sepia_strength = 1.0;
    }

    pub fn update_sepia_preview(&mut self) {
        self.sepia_preview_active = true;
        self.request_render();
    }

    pub fn cancel_sepia_preview(&mut self) {
        if self.sepia_preview_active {
            self.sepia_preview_active = false;
            self.request_render();
        }
    }

    pub fn reset_sepia(&mut self) {
        self.sepia_strength = 1.0;
        self.cancel_sepia_preview();
    }

    pub fn push_split_tone(&mut self) {
        self.split_preview_active = false;
        self.push_op(Box::new(SplitToneOp::new(
            self.split_shadow_hue,
            self.split_shadow_sat,
            self.split_highlight_hue,
            self.split_highlight_sat,
            self.split_balance,
        )));
    }

    pub fn update_split_preview(&mut self) {
        self.split_preview_active = true;
        self.request_render();
    }

    pub fn cancel_split_preview(&mut self) {
        if self.split_preview_active {
            self.split_preview_active = false;
            self.request_render();
        }
    }

    pub fn reset_split_tone(&mut self) {
        let defaults = SplitToneOp::default();
        self.split_shadow_hue = defaults.shadow_hue;
        self.split_shadow_sat = defaults.shadow_sat;
        self.split_highlight_hue = defaults.highlight_hue;
        self.split_highlight_sat = defaults.highlight_sat;
        self.split_balance = defaults.balance;
        self.cancel_split_preview();
    }

    pub fn push_resize(&mut self) {
        self.push_op(Box::new(ResizeOp::new(
            self.resize_w,
            self.resize_h,
            self.resize_mode,
        )));
    }

    pub fn push_blur(&mut self) {
        self.push_op(Box::new(BlurOp::new(self.blur_radius)));
    }

    pub fn push_denoise(&mut self) {
        self.push_op(Box::new(DenoiseOp::new(
            self.denoise_strength,
            self.denoise_radius,
        )));
    }

    pub fn push_perspective(&mut self) {
        self.push_op(Box::new(PerspectiveOp::new(self.perspective_corners)));
        self.perspective_corners = [[0.0; 2]; 4];
    }

    pub fn reset_perspective(&mut self) {
        self.perspective_corners = [[0.0; 2]; 4];
    }

    pub fn push_color_space(&mut self) {
        self.push_op(Box::new(ColorSpaceOp::new(self.color_space_conversion)));
    }

    /// Load a `.cube` file from `path` into `lut_op`.  Reports status on
    /// success or failure.
    pub fn load_lut(&mut self, path: std::path::PathBuf) {
        match std::fs::read_to_string(&path) {
            Ok(src) => match LutOp::from_cube_str(&src, self.lut_strength) {
                Ok(mut op) => {
                    op.strength = self.lut_strength;
                    self.lut_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.status = format!("Loaded LUT: {}", self.lut_name);
                    self.lut_op = Some(op);
                    self.lut_preview_active = false;
                }
                Err(e) => {
                    self.status = format!("LUT parse error: {}", e);
                }
            },
            Err(e) => {
                self.status = format!("LUT read error: {}", e);
            }
        }
    }

    /// Apply the currently loaded LUT (with current strength) to the pipeline.
    pub fn push_lut(&mut self) {
        if let Some(mut op) = self.lut_op.clone() {
            self.lut_preview_active = false;
            op.strength = self.lut_strength;
            self.push_op(Box::new(op));
        }
    }

    pub fn update_lut_preview(&mut self) {
        self.lut_preview_active = true;
        self.request_render();
    }

    pub fn cancel_lut_preview(&mut self) {
        if self.lut_preview_active {
            self.lut_preview_active = false;
            self.request_render();
        }
    }

    pub fn update_hue_preview(&mut self) {
        self.hue_preview_active = true;
        self.request_render();
    }

    pub fn cancel_hue_preview(&mut self) {
        if self.hue_preview_active {
            self.hue_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_hue(&mut self) {
        self.hue_preview_active = false;
        self.push_op(Box::new(HueShiftOp::new(self.hue_degrees)));
        self.hue_degrees = 0.0;
    }

    pub fn reset_hue(&mut self) {
        self.hue_degrees = 0.0;
        self.cancel_hue_preview();
    }

    pub fn update_hl_preview(&mut self) {
        self.hl_preview_active = true;
        self.request_render();
    }

    pub fn cancel_hl_preview(&mut self) {
        if self.hl_preview_active {
            self.hl_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_hl(&mut self) {
        self.hl_preview_active = false;
        self.push_op(Box::new(HighlightsShadowsOp::new(
            self.hl_highlights,
            self.hl_shadows,
        )));
        self.hl_highlights = 0.0;
        self.hl_shadows = 0.0;
    }

    pub fn reset_hl(&mut self) {
        self.hl_highlights = 0.0;
        self.hl_shadows = 0.0;
        self.cancel_hl_preview();
    }

    pub fn update_wb_preview(&mut self) {
        self.wb_preview_active = true;
        self.request_render();
    }

    pub fn cancel_wb_preview(&mut self) {
        if self.wb_preview_active {
            self.wb_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_wb(&mut self) {
        self.wb_preview_active = false;
        self.push_op(Box::new(WhiteBalanceOp::new(
            self.wb_temperature,
            self.wb_tint,
        )));
        self.wb_temperature = 0.0;
        self.wb_tint = 0.0;
    }

    pub fn reset_wb(&mut self) {
        self.wb_temperature = 0.0;
        self.wb_tint = 0.0;
        self.cancel_wb_preview();
    }

    pub fn update_hdr_preview(&mut self) {
        self.hdr_preview_active = true;
        self.request_render();
    }

    pub fn cancel_hdr_preview(&mut self) {
        if self.hdr_preview_active {
            self.hdr_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_hdr(&mut self) {
        self.hdr_preview_active = false;
        self.push_op(Box::new(FauxHdrOp::new(self.hdr_strength)));
    }

    pub fn reset_hdr(&mut self) {
        self.hdr_strength = 0.8;
        self.cancel_hdr_preview();
    }

    pub fn update_grain_preview(&mut self) {
        self.grain_preview_active = true;
        self.request_render();
    }

    pub fn cancel_grain_preview(&mut self) {
        if self.grain_preview_active {
            self.grain_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_grain(&mut self) {
        self.grain_preview_active = false;
        self.push_op(Box::new(GrainOp::new(
            self.grain_strength,
            self.grain_size,
            self.grain_seed,
        )));
        self.grain_seed = self.grain_seed.wrapping_add(1);
    }

    pub fn reset_grain(&mut self) {
        self.grain_strength = 0.10;
        self.grain_size = 1.8;
        self.grain_seed = 42;
        self.cancel_grain_preview();
    }

    pub fn update_cb_preview(&mut self) {
        self.cb_preview_active = true;
        self.request_render();
    }

    pub fn cancel_cb_preview(&mut self) {
        if self.cb_preview_active {
            self.cb_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_cb(&mut self) {
        self.cb_preview_active = false;
        self.push_op(Box::new(ColorBalanceOp::new(
            self.cb_cyan_red,
            self.cb_magenta_green,
            self.cb_yellow_blue,
        )));
        self.cb_cyan_red = [0.0; 3];
        self.cb_magenta_green = [0.0; 3];
        self.cb_yellow_blue = [0.0; 3];
    }

    pub fn reset_cb(&mut self) {
        self.cb_cyan_red = [0.0; 3];
        self.cb_magenta_green = [0.0; 3];
        self.cb_yellow_blue = [0.0; 3];
        self.cancel_cb_preview();
    }

    pub fn update_hsl_preview(&mut self) {
        self.hsl_preview_active = true;
        self.request_render();
    }

    pub fn cancel_hsl_preview(&mut self) {
        if self.hsl_preview_active {
            self.hsl_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_hsl(&mut self) {
        self.hsl_preview_active = false;
        self.push_op(Box::new(HslPanelOp::new(
            self.hsl_hue,
            self.hsl_sat,
            self.hsl_lum,
        )));
        self.hsl_hue = [0.0; 8];
        self.hsl_sat = [0.0; 8];
        self.hsl_lum = [0.0; 8];
    }

    pub fn reset_hsl(&mut self) {
        self.hsl_hue = [0.0; 8];
        self.hsl_sat = [0.0; 8];
        self.hsl_lum = [0.0; 8];
        self.cancel_hsl_preview();
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
            self.cancel_all_previews();
            self.request_render();
        }
    }
    pub fn reorder_op(&mut self, from: usize, to: usize) {
        if self
            .pipeline
            .as_mut()
            .is_some_and(|p| p.reorder_op(from, to))
        {
            self.cancel_all_previews();
            self.request_render();
        }
    }
    pub fn toggle_op(&mut self, index: usize) {
        if self.pipeline.as_mut().is_some_and(|p| p.toggle_op(index)) {
            self.cancel_all_previews();
            self.request_render();
        }
    }
    pub fn undo(&mut self) {
        if self.pipeline.as_mut().is_some_and(|p| p.undo()) {
            self.is_dirty = true;
            self.cancel_all_previews();
            self.request_render();
        }
    }
    pub fn redo(&mut self) {
        if self.pipeline.as_mut().is_some_and(|p| p.redo()) {
            self.is_dirty = true;
            self.cancel_all_previews();
            self.request_render();
        }
    }

    fn push_op(&mut self, op: Box<dyn Operation>) {
        self.cancel_all_previews();
        if let Some(p) = &mut self.pipeline {
            p.push_op(op);
            self.is_dirty = true;
            self.request_render();
        }
    }

    /// One-click auto-enhance: stretch levels to the 0.5/99.5 percentile,
    /// boost saturation slightly, apply a mild sharpen.  Pushes three ops
    /// as a single atomic batch (one render fired at the end).
    pub fn push_auto_enhance(&mut self) {
        if self.pipeline.is_none() || self.histogram.is_none() {
            return;
        }
        let (black, white) = {
            let hist = self.histogram.as_ref().unwrap();
            percentile_levels(&hist.luma, 0.005, 0.995)
        };
        self.cancel_all_previews();
        let pipeline = self.pipeline.as_mut().unwrap();
        pipeline.push_op(Box::new(LevelsOp::new(black, white, 1.0)));
        pipeline.push_op(Box::new(SaturationOp::new(1.1)));
        pipeline.push_op(Box::new(SharpenOp::new(0.5)));
        self.is_dirty = true;
        self.request_render();
    }

    /// Silently dismiss every tool preview without committing any of them.
    ///
    /// Called automatically whenever the pipeline is mutated through any means
    /// other than a tool's own "Apply" button, so the committed state is always
    /// visible unobscured.  Slider/curve values are preserved so the user can
    /// resume adjusting after the other operation is complete.
    fn cancel_all_previews(&mut self) {
        self.levels_preview_active = false;
        self.bw_preview_active = false;
        self.bc_preview_active = false;
        self.sat_preview_active = false;
        self.sepia_preview_active = false;
        self.sharpen_preview_active = false;
        self.split_preview_active = false;
        self.lut_preview_active = false;
        self.curve_preview_active = false;
        self.curve_dragging_idx = None;
        self.vignette_preview_active = false;
        self.vibrance_preview_active = false;
        self.hue_preview_active = false;
        self.hl_preview_active = false;
        self.wb_preview_active = false;
        self.hdr_preview_active = false;
        self.grain_preview_active = false;
        self.cb_preview_active = false;
        self.hsl_preview_active = false;
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
            || self.sepia_preview_active
            || self.sharpen_preview_active
            || self.split_preview_active
            || self.lut_preview_active
            || self.curve_preview_active
            || self.hdr_preview_active
            || self.wb_preview_active
            || self.hl_preview_active
            || self.hue_preview_active
            || self.vibrance_preview_active
            || self.cb_preview_active
            || self.hsl_preview_active)
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
        } else if self.sepia_preview_active {
            let preview: Box<dyn Operation> = Box::new(SepiaOp::new(self.sepia_strength));
            serde_json::to_value(&preview).ok()
        } else if self.sharpen_preview_active {
            let preview: Box<dyn Operation> = Box::new(SharpenOp::new(self.sharpen_strength));
            serde_json::to_value(&preview).ok()
        } else if self.split_preview_active {
            let preview: Box<dyn Operation> = Box::new(SplitToneOp::new(
                self.split_shadow_hue,
                self.split_shadow_sat,
                self.split_highlight_hue,
                self.split_highlight_sat,
                self.split_balance,
            ));
            serde_json::to_value(&preview).ok()
        } else if self.lut_preview_active {
            self.lut_op.as_ref().and_then(|op| {
                let mut preview = op.clone();
                preview.strength = self.lut_strength;
                let boxed: Box<dyn Operation> = Box::new(preview);
                serde_json::to_value(&boxed).ok()
            })
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
        } else if self.vibrance_preview_active {
            let preview: Box<dyn Operation> = Box::new(VibranceOp::new(self.vibrance));
            serde_json::to_value(&preview).ok()
        } else if self.hue_preview_active {
            let preview: Box<dyn Operation> = Box::new(HueShiftOp::new(self.hue_degrees));
            serde_json::to_value(&preview).ok()
        } else if self.hl_preview_active {
            let preview: Box<dyn Operation> = Box::new(HighlightsShadowsOp::new(
                self.hl_highlights,
                self.hl_shadows,
            ));
            serde_json::to_value(&preview).ok()
        } else if self.wb_preview_active {
            let preview: Box<dyn Operation> =
                Box::new(WhiteBalanceOp::new(self.wb_temperature, self.wb_tint));
            serde_json::to_value(&preview).ok()
        } else if self.hdr_preview_active {
            let preview: Box<dyn Operation> = Box::new(FauxHdrOp::new(self.hdr_strength));
            serde_json::to_value(&preview).ok()
        } else if self.grain_preview_active {
            let preview: Box<dyn Operation> = Box::new(GrainOp::new(
                self.grain_strength,
                self.grain_size,
                self.grain_seed,
            ));
            serde_json::to_value(&preview).ok()
        } else if self.cb_preview_active {
            let preview: Box<dyn Operation> = Box::new(ColorBalanceOp::new(
                self.cb_cyan_red,
                self.cb_magenta_green,
                self.cb_yellow_blue,
            ));
            serde_json::to_value(&preview).ok()
        } else if self.hsl_preview_active {
            let preview: Box<dyn Operation> =
                Box::new(HslPanelOp::new(self.hsl_hue, self.hsl_sat, self.hsl_lum));
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
/// Find the black and white points for auto-levels by clipping the histogram
/// at `lo_pct` and `hi_pct` percentiles of the cumulative pixel count.
/// Returns `(black, white)` as fractions in `[0.0, 1.0]`.
fn percentile_levels(hist: &[u64; 256], lo_pct: f64, hi_pct: f64) -> (f32, f32) {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return (0.0, 1.0);
    }
    let lo_target = ((total as f64 * lo_pct).ceil() as u64).max(1);
    let hi_target = ((total as f64 * (1.0 - hi_pct)).ceil() as u64).max(1);

    let mut black = 0usize;
    let mut cumsum = 0u64;
    for (i, &count) in hist.iter().enumerate() {
        cumsum += count;
        if cumsum >= lo_target {
            black = i;
            break;
        }
    }

    let mut white = 255usize;
    cumsum = 0;
    for (i, &count) in hist.iter().enumerate().rev() {
        cumsum += count;
        if cumsum >= hi_target {
            white = i;
            break;
        }
    }

    if white <= black {
        return (0.0, 1.0); // degenerate — don't adjust
    }
    (black as f32 / 255.0, white as f32 / 255.0)
}

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
