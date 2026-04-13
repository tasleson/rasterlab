use std::sync::{Arc, mpsc};

use crate::{prefs::Prefs, state::VirtualCopyStore};

use egui::Context;
use rasterlab_core::{
    Image, cancel as core_cancel,
    formats::FormatRegistry,
    ops::{
        BlackAndWhiteOp, BlurOp, BrightnessContrastOp, ClarityTextureOp, ColorBalanceOp,
        ColorSpaceOp, CropOp, CurvesOp, DenoiseOp, FauxHdrOp, FlipOp, FocusStackOp, GrainOp,
        HealOp, HealSpot, HighlightsShadowsOp, HistogramData, HslPanelOp, HueShiftOp, LevelsOp,
        LutOp, MaskedOp, NoiseReductionOp, NrMethod, PanoramaOp, PerspectiveOp, ResizeOp, RotateOp,
        SaturationOp, SepiaOp, ShadowExposureOp, SharpenOp, SplitToneOp, VibranceOp, VignetteOp,
        WhiteBalanceOp,
    },
    pipeline::EditPipeline,
    project::{RlabFile, RlabMeta},
    traits::operation::Operation,
};

use super::{EditSession, EditingTool, ToolState, load_op_into_tools};

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
        /// When Some, this image is a full-resolution crop of just the visible
        /// viewport — it should be drawn as an overlay at [x,y,w,h] in
        /// full-res image coordinates rather than replacing `state.rendered`.
        overlay_rect: Option<[u32; 4]>,
    },
    Error(String),
    /// The render thread aborted because [`core_cancel::request`] was called.
    Cancelled,
}

/// What the split "before/after" view compares against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitMode {
    /// Left = source with geometric ops only; right = full pipeline output.
    VsOriginal,
    /// Left = pipeline through op N-1; right = pipeline through op N, where
    /// N is the index of the op currently being edited.  Falls back to
    /// `VsOriginal` when no op is under edit.
    VsPreviousStep,
}

// ---------------------------------------------------------------------------
// AppState
// ---------------------------------------------------------------------------

pub struct AppState {
    /// Persistent GUI preferences (tool panel open/closed states, etc.).
    pub prefs: Prefs,
    pub registry: FormatRegistry,
    /// All virtual copies for the open image.  `None` when no image is loaded.
    pub copies: Option<VirtualCopyStore>,
    /// When `Some`, a rename dialog is open for the copy at that index.
    /// The `String` is the live text being edited; `Pos2` is the screen
    /// position of the tab that triggered the rename, used to anchor the dialog.
    pub rename_pending: Option<(usize, String, egui::Pos2)>,
    pub rendered: Option<Arc<Image>>,
    /// True while the canvas is displaying a downsampled preview render.
    pub rendered_is_preview: bool,
    /// Scale of the current `rendered` image vs the full-res committed result.
    /// 1.0 for full-res, PREVIEW_SCALE (0.25) for a preview render.
    pub rendered_scale: f32,
    /// Visible region of the rendered image [x, y, w, h] in image-pixel coords.
    /// Updated by the canvas every frame; used to restrict preview renders to
    /// only the pixels the user can actually see.
    pub preview_viewport: Option<[u32; 4]>,
    /// Full-resolution preview of just the visible viewport, rendered on top of
    /// `rendered` by the canvas.  None when no preview is active.
    pub preview_overlay: Option<Arc<Image>>,
    /// Position of `preview_overlay` in full-res image pixel coordinates.
    pub preview_overlay_rect: Option<[u32; 4]>,
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
    /// Incremented each time a new file is opened. Canvas uses this to know
    /// when to reset zoom/pan vs. just updating the texture.
    pub image_generation: u64,
    /// When true the canvas renders a split before/after view.
    pub split_view: bool,
    /// What the split view compares against.
    pub split_mode: SplitMode,
    /// Op index anchoring vs-previous-step mode when no op is under edit.
    /// `None` means "last op in the pipeline".
    pub split_focus: Option<usize>,
    /// When `Some`, forces every tool-panel CollapsingHeader open/closed for
    /// one frame.  Cleared by the tools panel after use.
    pub tools_force_open: Option<bool>,

    // Background thread channel
    bg_tx: mpsc::Sender<BgMessage>,
    bg_rx: mpsc::Receiver<BgMessage>,
    // egui context — needed to wake up the UI after background work completes
    ctx: Context,

    /// All per-tool input fields, preview flags, and export settings.
    pub tools: ToolState,

    /// Set when a slider changes while a render is in-flight; triggers a
    /// follow-up render as soon as the current one completes.
    needs_rerender: bool,
    /// Wall-clock time at which the most recent render thread was spawned.
    render_start: Option<std::time::Instant>,
    /// True when the in-flight render includes a noise-reduction op (either
    /// as the active preview or as a committed pipeline step).  Drives the
    /// visibility of the NR Cancel button so the user can abort a slow NLM run.
    nr_in_flight: bool,

    // ── Autosave ────────────────────────────────────────────────────────────
    /// Unix timestamp identifying the current editing session.  Used as the
    /// autosave filename stem.  Set when a source image is opened; cleared
    /// when a project is loaded (which has its own save path).
    pub autosave_session_id: Option<u64>,
    /// Set to `true` by every pipeline mutation; cleared once the autosave
    /// has been written.  Checked in `poll_background` each frame.
    autosave_pending: bool,
    /// When `Some`, the next `ImageLoaded` message will restore these virtual
    /// copy states rather than starting a fresh pipeline.
    autosave_restore: Option<(Vec<rasterlab_core::project::SavedCopy>, usize)>,
    /// Session ID to reuse when performing an autosave restore, so that the
    /// original autosave file is correctly cleaned up on project save.
    autosave_restore_session_id: Option<u64>,

    /// When `Some`, the user is editing an existing pipeline op rather than
    /// creating a new one.  While active, most tool panel sections and edit
    /// stack buttons are disabled; only the tool matching `editing.tool` is
    /// interactive, and its Apply button replaces the op instead of pushing.
    pub editing: Option<EditSession>,
}

impl AppState {
    pub fn new(ctx: Context) -> Self {
        let (bg_tx, bg_rx) = mpsc::channel();
        Self {
            prefs: Prefs::load(),
            registry: FormatRegistry::with_builtins(),
            copies: None,
            rename_pending: None,
            rendered: None,
            rendered_is_preview: false,
            rendered_scale: 1.0,
            preview_overlay: None,
            preview_overlay_rect: None,
            preview_viewport: None,
            histogram: None,
            loading: false,
            status: "Welcome to RasterLab — open an image to begin.".into(),
            last_path: None,
            original_bytes: None,
            project_path: None,
            is_dirty: false,
            project_created_at: None,
            image_generation: 0,
            split_view: false,
            split_mode: SplitMode::VsOriginal,
            split_focus: None,
            tools_force_open: None,
            bg_tx,
            bg_rx,
            ctx,
            tools: ToolState::new(),
            needs_rerender: false,
            render_start: None,
            nr_in_flight: false,
            autosave_session_id: None,
            autosave_pending: false,
            autosave_restore: None,
            autosave_restore_session_id: None,
            editing: None,
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
                    self.tools.crop_w = w;
                    self.tools.crop_h = h;
                    self.tools.resize_w = w;
                    self.tools.resize_h = h;
                    self.last_path = Some(path.clone());
                    self.original_bytes = Some(original_bytes);
                    self.project_path = None;
                    self.is_dirty = false;
                    self.project_created_at = None;
                    self.status = format!("Opened {}  ({}×{})", path.display(), w, h);
                    self.rename_pending = None;

                    // Determine the session ID: reuse the one from an autosave
                    // restore (for correct cleanup on save) or mint a fresh one.
                    self.autosave_session_id = Some(
                        self.autosave_restore_session_id
                            .take()
                            .unwrap_or_else(crate::autosave::unix_now),
                    );
                    self.autosave_pending = false;

                    if let Some((saved_copies, saved_active)) = self.autosave_restore.take() {
                        let image_arc = Arc::new(image);
                        match VirtualCopyStore::load_from_saved(
                            Arc::clone(&image_arc),
                            saved_copies,
                            saved_active,
                        ) {
                            Ok(store) => {
                                self.copies = Some(store);
                                self.mark_dirty();
                            }
                            Err(e) => {
                                self.status =
                                    format!("Warning: could not restore edit stack: {}", e);
                                self.copies = Some(VirtualCopyStore::new(
                                    "Copy 1".into(),
                                    EditPipeline::new_virtual_copy(image_arc),
                                ));
                            }
                        }
                    } else {
                        self.copies = Some(VirtualCopyStore::new(
                            "Copy 1".into(),
                            EditPipeline::new(image),
                        ));
                    }

                    self.prefs.push_recent(path);
                    self.prefs.save();
                    self.loading = false;
                    self.image_generation += 1;
                    self.request_render();
                }
                BgMessage::ProjectLoaded { path, rlab, image } => {
                    let w = image.width;
                    let h = image.height;
                    self.tools.crop_w = w;
                    self.tools.crop_h = h;
                    self.tools.resize_w = w;
                    self.tools.resize_h = h;
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
                    self.rename_pending = None;
                    // Mint a new autosave session so edits made after opening
                    // a project are recoverable if the user quits without saving.
                    // The autosave file is deleted on the next successful save.
                    self.autosave_session_id = Some(crate::autosave::unix_now());
                    self.autosave_pending = false;
                    match VirtualCopyStore::load_from_saved(
                        Arc::new(image),
                        rlab.copies,
                        rlab.active_copy_index,
                    ) {
                        Ok(store) => self.copies = Some(store),
                        Err(e) => {
                            self.status = format!("Warning: could not restore edit stack: {}", e);
                        }
                    }
                    self.prefs.push_recent(path);
                    self.prefs.save();
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
                    overlay_rect,
                } => {
                    self.histogram = Some(*hist);
                    self.loading = false;
                    self.nr_in_flight = false;

                    if let Some(rect) = overlay_rect {
                        // Viewport overlay — draw on top of the existing base render.
                        // Don't touch state.rendered so the canvas never goes blank.
                        self.preview_overlay = Some(image);
                        self.preview_overlay_rect = Some(rect);
                    } else {
                        // Full-res or fallback-scale render — update the base image.
                        self.rendered = Some(image);
                        self.rendered_is_preview = is_preview;
                        self.rendered_scale = if is_preview { PREVIEW_SCALE } else { 1.0 };
                        if !is_preview {
                            self.preview_overlay = None;
                            self.preview_overlay_rect = None;
                        }
                    }

                    if !is_preview && overlay_rect.is_none() {
                        // Only report timing and populate the step cache for
                        // full-res renders; preview intermediates are low-res.
                        let elapsed_ms = self
                            .render_start
                            .take()
                            .map(|t| t.elapsed().as_millis())
                            .unwrap_or(0);
                        self.status = format!("Ready  ({} ms)", elapsed_ms);
                        if let Some(pipeline) = self.pipeline_mut()
                            && cache_gen == pipeline.step_cache_gen()
                        {
                            pipeline.store_steps(start_index, intermediates);
                        }
                    }

                    if self.needs_rerender {
                        self.needs_rerender = false;
                        self.request_render_inner(false);
                    } else if is_preview || overlay_rect.is_some() {
                        // Preview displayed — follow up with a full-res render.
                        self.request_render_inner(true);
                    }
                }
                BgMessage::Error(e) => {
                    self.status = format!("Error: {}", e);
                    self.loading = false;
                    self.nr_in_flight = false;
                }
                BgMessage::Cancelled => {
                    self.loading = false;
                    self.nr_in_flight = false;
                    self.render_start = None;
                    self.status = "Cancelled".into();
                    // A follow-up render may already be queued (the Cancel
                    // button clears the NR preview flag and calls
                    // request_render, which sets needs_rerender while the
                    // aborted render was still in flight).  Honour it now so
                    // the canvas returns to the committed state.
                    if self.needs_rerender {
                        self.needs_rerender = false;
                        self.request_render_inner(false);
                    }
                }
            }
        }
        self.maybe_write_autosave();
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
        let to_save: &Image = if self.tools.export_resize_enabled
            && self.tools.export_resize_w > 0
            && self.tools.export_resize_h > 0
        {
            let op = ResizeOp::new(
                self.tools.export_resize_w,
                self.tools.export_resize_h,
                self.tools.export_resize_mode,
            );
            match op.apply(rendered.as_ref().deep_clone()) {
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

        match self
            .registry
            .encode_file(to_save, &path, &self.tools.encode_opts)
        {
            Ok(bytes) => {
                if let Err(e) = std::fs::write(&path, &bytes) {
                    self.status = format!("Write failed: {}", e);
                } else {
                    self.status = format!("Saved {} bytes → {}", bytes.len(), path.display());
                    // Exporting a rendered image counts as preserving the
                    // user's work, so clear the dirty flag — this keeps the
                    // exit confirmation from firing after a successful export.
                    self.is_dirty = false;
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
        let Some(pipeline) = self.pipeline() else {
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
        let Some(store) = &self.copies else {
            self.status = "Nothing to save — no active pipeline".into();
            return;
        };

        let (copies_saved, active_idx) = match store.save_states() {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Save failed (pipeline): {}", e);
                return;
            }
        };

        let source = store.source();
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
        let rlab = RlabFile::new(meta, original_bytes, copies_saved, active_idx, None);
        match rlab.write(&path) {
            Ok(()) => {
                self.project_created_at = Some(created_at);
                self.project_path = Some(path.clone());
                self.is_dirty = false;
                self.autosave_pending = false;
                // Clean up the autosave file now that the work is safely saved.
                if let Some(session_id) = self.autosave_session_id.take() {
                    crate::autosave::delete(session_id);
                }
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

    pub fn push_heal(&mut self) {
        if self.tools.heal_spots.is_empty() {
            return;
        }
        let spots = std::mem::take(&mut self.tools.heal_spots);
        self.tools.heal_active = false;
        self.push_op(Box::new(HealOp::new(spots)));
    }

    pub fn heal_place_spot(&mut self, dest_x: i32, dest_y: i32) {
        let src = if let Some(rendered) = &self.rendered {
            let (sx, sy) =
                HealOp::auto_detect_source(rendered, dest_x, dest_y, self.tools.heal_radius);
            (sx, sy)
        } else {
            (dest_x + self.tools.heal_radius as i32 * 2, dest_y)
        };
        self.tools.heal_spots.push(HealSpot {
            dest_x,
            dest_y,
            src_x: src.0,
            src_y: src.1,
            radius: self.tools.heal_radius,
        });
    }

    pub fn update_straighten_preview(&mut self) {
        self.tools.straighten_preview_active = true;
        self.request_render();
    }

    pub fn cancel_straighten_preview(&mut self) {
        if self.tools.straighten_preview_active {
            self.tools.straighten_preview_active = false;
            self.request_render();
        }
    }

    pub fn reset_straighten(&mut self) {
        self.tools.straighten_angle = 0.0;
        self.tools.straighten_active = false;
        self.cancel_straighten_preview();
    }

    pub fn push_straighten(&mut self) {
        if self.tools.straighten_angle.abs() < 0.001 {
            return;
        }
        self.tools.straighten_preview_active = false;
        let angle = self.tools.straighten_angle;
        self.tools.straighten_angle = 0.0;
        self.tools.straighten_active = false;

        // Derive pre-rotation full-res dimensions from the current render.
        let crop_op = if self.tools.straighten_crop {
            self.rendered.as_ref().map(|img| {
                let w = (img.width as f32 / self.rendered_scale).round() as u32;
                let h = (img.height as f32 / self.rendered_scale).round() as u32;
                straighten_crop_op(w, h, angle)
            })
        } else {
            None
        };

        self.cancel_all_previews();
        if let Some(store) = &mut self.copies {
            let p = store.active_pipeline_mut();
            p.push_op(Box::new(RotateOp::arbitrary(angle)));
            if let Some(crop) = crop_op {
                p.push_op(Box::new(crop));
            }
        }
        if self.copies.is_some() {
            self.mark_dirty();
            self.request_render();
        }
    }

    pub fn push_crop(&mut self) {
        self.push_op(Box::new(CropOp::new(
            self.tools.crop_x,
            self.tools.crop_y,
            self.tools.crop_w,
            self.tools.crop_h,
        )));
    }
    pub fn push_rotate_arbitrary(&mut self) {
        self.tools.rotate_preview_active = false;
        // Use lossless pixel-transposition fast paths for exact right-angle
        // rotations; fall back to bilinear for all other angles.
        let op: Box<dyn Operation> = match self.tools.rotate_deg as i32 % 360 {
            90 | -270 => Box::new(RotateOp::cw90()),
            180 | -180 => Box::new(RotateOp::cw180()),
            270 | -90 => Box::new(RotateOp::cw270()),
            _ => Box::new(RotateOp::arbitrary(self.tools.rotate_deg)),
        };
        self.push_op(op);
    }

    pub fn update_rotate_preview(&mut self) {
        self.tools.rotate_preview_active = true;
        self.request_render();
    }

    pub fn cancel_rotate_preview(&mut self) {
        if self.tools.rotate_preview_active {
            self.tools.rotate_preview_active = false;
            self.request_render();
        }
    }

    pub fn reset_rotate(&mut self) {
        self.tools.rotate_deg = 0.0;
        self.cancel_rotate_preview();
    }
    pub fn push_sharpen(&mut self) {
        self.tools.sharpen_preview_active = false;
        self.push_op(Box::new(SharpenOp::new(self.tools.sharpen_strength)));
    }

    pub fn update_sharpen_preview(&mut self) {
        self.tools.sharpen_preview_active = true;
        self.request_render();
    }

    pub fn cancel_sharpen_preview(&mut self) {
        if self.tools.sharpen_preview_active {
            self.tools.sharpen_preview_active = false;
            self.request_render();
        }
    }

    pub fn reset_sharpen(&mut self) {
        self.tools.sharpen_strength = 1.0;
        self.cancel_sharpen_preview();
    }

    pub fn push_clarity_texture(&mut self) {
        self.tools.clarity_preview_active = false;
        self.push_op(Box::new(ClarityTextureOp::new(
            self.tools.clarity,
            self.tools.texture,
        )));
    }

    pub fn update_clarity_texture_preview(&mut self) {
        self.tools.clarity_preview_active = true;
        self.request_render();
    }

    pub fn cancel_clarity_texture_preview(&mut self) {
        if self.tools.clarity_preview_active {
            self.tools.clarity_preview_active = false;
            self.request_render();
        }
    }

    pub fn reset_clarity_texture(&mut self) {
        self.tools.clarity = 0.0;
        self.tools.texture = 0.0;
        self.cancel_clarity_texture_preview();
    }

    pub fn push_flip_pending(&mut self) {
        let h = self.tools.flip_h_pending;
        let v = self.tools.flip_v_pending;
        self.tools.flip_h_pending = false;
        self.tools.flip_v_pending = false;
        self.tools.flip_preview_active = false;
        if h {
            self.push_op(Box::new(FlipOp::horizontal()));
        }
        if v {
            self.push_op(Box::new(FlipOp::vertical()));
        }
    }

    pub fn update_flip_preview(&mut self) {
        self.tools.flip_preview_active = true;
        self.request_render();
    }

    pub fn cancel_flip_preview(&mut self) {
        if self.tools.flip_preview_active {
            self.tools.flip_h_pending = false;
            self.tools.flip_v_pending = false;
            self.tools.flip_preview_active = false;
            self.request_render();
        }
    }

    pub fn update_bc_preview(&mut self) {
        self.tools.bc_preview_active = true;
        self.request_render();
    }

    pub fn cancel_bc_preview(&mut self) {
        if self.tools.bc_preview_active {
            self.tools.bc_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_bc(&mut self) {
        self.tools.bc_preview_active = false;
        self.push_op(Box::new(BrightnessContrastOp::new(
            self.tools.bc_brightness,
            self.tools.bc_contrast,
        )));
        self.tools.bc_brightness = 0.0;
        self.tools.bc_contrast = 0.0;
    }

    pub fn reset_bc(&mut self) {
        self.tools.bc_brightness = 0.0;
        self.tools.bc_contrast = 0.0;
        self.cancel_bc_preview();
    }

    pub fn update_sat_preview(&mut self) {
        self.tools.sat_preview_active = true;
        self.request_render();
    }

    pub fn cancel_sat_preview(&mut self) {
        if self.tools.sat_preview_active {
            self.tools.sat_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_saturation(&mut self) {
        self.tools.sat_preview_active = false;
        self.push_op(Box::new(SaturationOp::new(self.tools.saturation)));
        self.tools.saturation = 1.0;
    }

    pub fn reset_saturation(&mut self) {
        self.tools.saturation = 1.0;
        self.cancel_sat_preview();
    }

    pub fn update_curve_preview(&mut self) {
        self.tools.curve_preview_active = true;
        self.request_render();
    }

    pub fn cancel_curve_preview(&mut self) {
        if self.tools.curve_preview_active {
            self.tools.curve_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_curves(&mut self) {
        self.tools.curve_preview_active = false;
        self.push_op(Box::new(CurvesOp {
            points: self.tools.curve_points.clone(),
        }));
        self.tools.curve_points = vec![[0.0, 0.0], [1.0, 1.0]];
    }

    pub fn reset_curves(&mut self) {
        self.tools.curve_points = vec![[0.0, 0.0], [1.0, 1.0]];
        self.tools.curve_dragging_idx = None;
        self.cancel_curve_preview();
    }

    pub fn update_vignette_preview(&mut self) {
        self.tools.vignette_preview_active = true;
        self.request_render();
    }

    pub fn cancel_vignette_preview(&mut self) {
        if self.tools.vignette_preview_active {
            self.tools.vignette_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_vignette(&mut self) {
        self.tools.vignette_preview_active = false;
        self.push_op(Box::new(VignetteOp::new(
            self.tools.vignette_strength,
            self.tools.vignette_radius,
            self.tools.vignette_feather,
        )));
    }

    pub fn reset_vignette(&mut self) {
        self.tools.vignette_strength = 0.5;
        self.tools.vignette_radius = 0.65;
        self.tools.vignette_feather = 0.5;
        self.cancel_vignette_preview();
    }

    pub fn update_vibrance_preview(&mut self) {
        self.tools.vibrance_preview_active = true;
        self.request_render();
    }

    pub fn cancel_vibrance_preview(&mut self) {
        if self.tools.vibrance_preview_active {
            self.tools.vibrance_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_vibrance(&mut self) {
        self.tools.vibrance_preview_active = false;
        self.push_op(Box::new(VibranceOp::new(self.tools.vibrance)));
        self.tools.vibrance = 0.0;
    }

    pub fn reset_vibrance(&mut self) {
        self.tools.vibrance = 0.0;
        self.cancel_vibrance_preview();
    }

    pub fn push_sepia(&mut self) {
        self.tools.sepia_preview_active = false;
        self.push_op(Box::new(SepiaOp::new(self.tools.sepia_strength)));
        self.tools.sepia_strength = 1.0;
    }

    pub fn update_sepia_preview(&mut self) {
        self.tools.sepia_preview_active = true;
        self.request_render();
    }

    pub fn cancel_sepia_preview(&mut self) {
        if self.tools.sepia_preview_active {
            self.tools.sepia_preview_active = false;
            self.request_render();
        }
    }

    pub fn reset_sepia(&mut self) {
        self.tools.sepia_strength = 1.0;
        self.cancel_sepia_preview();
    }

    pub fn push_split_tone(&mut self) {
        self.tools.split_preview_active = false;
        self.push_op(Box::new(SplitToneOp::new(
            self.tools.split_shadow_hue,
            self.tools.split_shadow_sat,
            self.tools.split_highlight_hue,
            self.tools.split_highlight_sat,
            self.tools.split_balance,
        )));
    }

    pub fn update_split_preview(&mut self) {
        self.tools.split_preview_active = true;
        self.request_render();
    }

    pub fn cancel_split_preview(&mut self) {
        if self.tools.split_preview_active {
            self.tools.split_preview_active = false;
            self.request_render();
        }
    }

    pub fn reset_split_tone(&mut self) {
        let defaults = SplitToneOp::default();
        self.tools.split_shadow_hue = defaults.shadow_hue;
        self.tools.split_shadow_sat = defaults.shadow_sat;
        self.tools.split_highlight_hue = defaults.highlight_hue;
        self.tools.split_highlight_sat = defaults.highlight_sat;
        self.tools.split_balance = defaults.balance;
        self.cancel_split_preview();
    }

    pub fn push_resize(&mut self) {
        self.push_op(Box::new(ResizeOp::new(
            self.tools.resize_w,
            self.tools.resize_h,
            self.tools.resize_mode,
        )));
    }

    pub fn update_blur_preview(&mut self) {
        self.tools.blur_preview_active = true;
        self.request_render();
    }

    pub fn cancel_blur_preview(&mut self) {
        if self.tools.blur_preview_active {
            self.tools.blur_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_blur(&mut self) {
        self.tools.blur_preview_active = false;
        self.push_op(Box::new(BlurOp::new(self.tools.blur_radius)));
    }

    pub fn reset_blur(&mut self) {
        self.tools.blur_radius = 2.0;
        self.cancel_blur_preview();
    }

    pub fn update_denoise_preview(&mut self) {
        self.tools.denoise_preview_active = true;
        self.request_render();
    }

    pub fn cancel_denoise_preview(&mut self) {
        if self.tools.denoise_preview_active {
            self.tools.denoise_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_denoise(&mut self) {
        self.tools.denoise_preview_active = false;
        self.push_op(Box::new(DenoiseOp::new(
            self.tools.denoise_strength,
            self.tools.denoise_radius,
        )));
    }

    pub fn reset_denoise(&mut self) {
        self.tools.denoise_strength = 0.1;
        self.tools.denoise_radius = 3;
        self.cancel_denoise_preview();
    }

    pub fn update_nr_preview(&mut self) {
        self.tools.nr_preview_active = true;
        self.request_render();
    }

    pub fn cancel_nr_preview(&mut self) {
        let had_preview = self.tools.nr_preview_active;
        // Ask any in-flight noise-reduction op to abort promptly.  This is a
        // no-op when nothing is running, and the cleared flag will be reset
        // the next time a render is spawned.
        if self.loading && self.nr_in_flight {
            core_cancel::request();
            // If the user cancelled a render for a noise-reduction op they
            // had already committed via Apply, roll that op back off the
            // pipeline so the canvas returns to the pre-NR state.  The
            // preview case needs no pipeline mutation.
            if !had_preview
                && let Some(p) = self.pipeline_mut()
                && p.cursor() > 0
                && p.ops()[p.cursor() - 1].operation.name() == "noise_reduction"
            {
                p.undo();
                self.mark_dirty();
            }
        }
        if had_preview {
            self.tools.nr_preview_active = false;
        }
        // Always kick a re-render so the canvas reflects whatever the current
        // (post-cancel) pipeline state is.  When a render is already in flight
        // this only sets needs_rerender; the post-cancel handler will honour
        // it once the aborted render returns.
        self.request_render();
    }

    /// True while a render that includes a noise-reduction op is running.
    /// Used by the tools panel to decide whether to show the NR Cancel button.
    pub fn nr_in_flight(&self) -> bool {
        self.nr_in_flight && self.loading
    }

    pub fn push_noise_reduction(&mut self) {
        self.tools.nr_preview_active = false;
        self.push_op(Box::new(NoiseReductionOp {
            method: self.tools.nr_method.clone(),
            luma_strength: self.tools.nr_luma,
            color_strength: self.tools.nr_color,
            detail_preservation: self.tools.nr_detail,
        }));
    }

    pub fn reset_noise_reduction(&mut self) {
        self.tools.nr_method = NrMethod::Wavelet;
        self.tools.nr_luma = 0.3;
        self.tools.nr_color = 0.5;
        self.tools.nr_detail = 0.5;
        self.cancel_nr_preview();
    }

    pub fn panorama_add_image(&mut self, path: std::path::PathBuf) {
        self.tools
            .panorama_paths
            .push(path.to_string_lossy().into_owned());
        if self.tools.panorama_paths.len() >= 2 {
            self.tools.panorama_preview_active = true;
            self.request_render();
        }
    }

    pub fn cancel_panorama_preview(&mut self) {
        if self.tools.panorama_preview_active {
            self.tools.panorama_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_panorama(&mut self) {
        self.tools.panorama_preview_active = false;
        self.push_op(Box::new(PanoramaOp::new(
            self.tools.panorama_paths.clone(),
            self.tools.panorama_feather_px,
        )));
        self.tools.panorama_paths.clear();
    }

    pub fn reset_panorama(&mut self) {
        self.tools.panorama_paths.clear();
        self.tools.panorama_feather_px = 80;
        self.cancel_panorama_preview();
    }

    pub fn focus_stack_add_image(&mut self, path: std::path::PathBuf) {
        self.tools
            .focus_stack_paths
            .push(path.to_string_lossy().into_owned());
        if self.tools.focus_stack_paths.len() >= 2 {
            self.tools.focus_stack_preview_active = true;
            self.request_render();
        }
    }

    pub fn cancel_focus_stack_preview(&mut self) {
        if self.tools.focus_stack_preview_active {
            self.tools.focus_stack_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_focus_stack(&mut self) {
        self.tools.focus_stack_preview_active = false;
        self.push_op(Box::new(FocusStackOp::new(
            self.tools.focus_stack_paths.clone(),
        )));
        self.tools.focus_stack_paths.clear();
    }

    pub fn reset_focus_stack(&mut self) {
        self.tools.focus_stack_paths.clear();
        self.cancel_focus_stack_preview();
    }

    pub fn update_perspective_preview(&mut self) {
        self.tools.perspective_preview_active = true;
        self.request_render();
    }

    pub fn cancel_perspective_preview(&mut self) {
        if self.tools.perspective_preview_active {
            self.tools.perspective_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_perspective(&mut self) {
        self.tools.perspective_preview_active = false;
        self.push_op(Box::new(PerspectiveOp::new(self.tools.perspective_corners)));
        self.tools.perspective_corners = [[0.0; 2]; 4];
    }

    pub fn reset_perspective(&mut self) {
        self.tools.perspective_corners = [[0.0; 2]; 4];
        self.cancel_perspective_preview();
    }

    pub fn push_color_space(&mut self) {
        self.push_op(Box::new(ColorSpaceOp::new(
            self.tools.color_space_conversion,
        )));
    }

    /// Load a `.cube` file from `path` into `lut_op`.  Reports status on
    /// success or failure.
    pub fn load_lut(&mut self, path: std::path::PathBuf) {
        match std::fs::read_to_string(&path) {
            Ok(src) => match LutOp::from_cube_str(&src, self.tools.lut_strength) {
                Ok(mut op) => {
                    op.strength = self.tools.lut_strength;
                    self.tools.lut_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.status = format!("Loaded LUT: {}", self.tools.lut_name);
                    self.tools.lut_op = Some(op);
                    self.tools.lut_preview_active = false;
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
        if let Some(mut op) = self.tools.lut_op.clone() {
            self.tools.lut_preview_active = false;
            op.strength = self.tools.lut_strength;
            self.push_op(Box::new(op));
        }
    }

    pub fn update_lut_preview(&mut self) {
        self.tools.lut_preview_active = true;
        self.request_render();
    }

    pub fn cancel_lut_preview(&mut self) {
        if self.tools.lut_preview_active {
            self.tools.lut_preview_active = false;
            self.request_render();
        }
    }

    pub fn reset_lut(&mut self) {
        self.tools.lut_strength = 1.0;
        self.cancel_lut_preview();
    }

    pub fn update_hue_preview(&mut self) {
        self.tools.hue_preview_active = true;
        self.request_render();
    }

    pub fn cancel_hue_preview(&mut self) {
        if self.tools.hue_preview_active {
            self.tools.hue_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_hue(&mut self) {
        self.tools.hue_preview_active = false;
        self.push_op(Box::new(HueShiftOp::new(self.tools.hue_degrees)));
        self.tools.hue_degrees = 0.0;
    }

    pub fn reset_hue(&mut self) {
        self.tools.hue_degrees = 0.0;
        self.cancel_hue_preview();
    }

    pub fn update_hl_preview(&mut self) {
        self.tools.hl_preview_active = true;
        self.request_render();
    }

    pub fn cancel_hl_preview(&mut self) {
        if self.tools.hl_preview_active {
            self.tools.hl_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_hl(&mut self) {
        self.tools.hl_preview_active = false;
        self.push_op(Box::new(HighlightsShadowsOp::new(
            self.tools.hl_highlights,
            self.tools.hl_shadows,
        )));
        self.tools.hl_highlights = 0.0;
        self.tools.hl_shadows = 0.0;
    }

    pub fn reset_hl(&mut self) {
        self.tools.hl_highlights = 0.0;
        self.tools.hl_shadows = 0.0;
        self.cancel_hl_preview();
    }

    pub fn update_shadow_exp_preview(&mut self) {
        self.tools.shadow_exp_preview_active = true;
        self.request_render();
    }

    pub fn cancel_shadow_exp_preview(&mut self) {
        if self.tools.shadow_exp_preview_active {
            self.tools.shadow_exp_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_shadow_exp(&mut self) {
        self.tools.shadow_exp_preview_active = false;
        self.push_op(Box::new(ShadowExposureOp::new(
            self.tools.shadow_ev,
            self.tools.shadow_falloff,
        )));
        self.tools.shadow_ev = 0.0;
        self.tools.shadow_falloff = 2.0;
    }

    pub fn reset_shadow_exp(&mut self) {
        self.tools.shadow_ev = 0.0;
        self.tools.shadow_falloff = 2.0;
        self.cancel_shadow_exp_preview();
    }

    pub fn update_wb_preview(&mut self) {
        self.tools.wb_preview_active = true;
        self.request_render();
    }

    pub fn cancel_wb_preview(&mut self) {
        if self.tools.wb_preview_active {
            self.tools.wb_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_wb(&mut self) {
        self.tools.wb_preview_active = false;
        self.push_op(Box::new(WhiteBalanceOp::new(
            self.tools.wb_temperature,
            self.tools.wb_tint,
        )));
        self.tools.wb_temperature = 0.0;
        self.tools.wb_tint = 0.0;
    }

    pub fn reset_wb(&mut self) {
        self.tools.wb_temperature = 0.0;
        self.tools.wb_tint = 0.0;
        self.cancel_wb_preview();
    }

    pub fn update_hdr_preview(&mut self) {
        self.tools.hdr_preview_active = true;
        self.request_render();
    }

    pub fn cancel_hdr_preview(&mut self) {
        if self.tools.hdr_preview_active {
            self.tools.hdr_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_hdr(&mut self) {
        self.tools.hdr_preview_active = false;
        self.push_op(Box::new(FauxHdrOp::new(self.tools.hdr_strength)));
    }

    pub fn reset_hdr(&mut self) {
        self.tools.hdr_strength = 0.8;
        self.cancel_hdr_preview();
    }

    pub fn update_grain_preview(&mut self) {
        self.tools.grain_preview_active = true;
        self.request_render();
    }

    pub fn cancel_grain_preview(&mut self) {
        if self.tools.grain_preview_active {
            self.tools.grain_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_grain(&mut self) {
        self.tools.grain_preview_active = false;
        self.push_op(Box::new(GrainOp::new(
            self.tools.grain_strength,
            self.tools.grain_size,
            self.tools.grain_seed,
        )));
        self.tools.grain_seed = self.tools.grain_seed.wrapping_add(1);
    }

    pub fn reset_grain(&mut self) {
        self.tools.grain_strength = 0.10;
        self.tools.grain_size = 1.8;
        self.tools.grain_seed = 42;
        self.cancel_grain_preview();
    }

    pub fn update_cb_preview(&mut self) {
        self.tools.cb_preview_active = true;
        self.request_render();
    }

    pub fn cancel_cb_preview(&mut self) {
        if self.tools.cb_preview_active {
            self.tools.cb_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_cb(&mut self) {
        self.tools.cb_preview_active = false;
        self.push_op(Box::new(ColorBalanceOp::new(
            self.tools.cb_cyan_red,
            self.tools.cb_magenta_green,
            self.tools.cb_yellow_blue,
        )));
        self.tools.cb_cyan_red = [0.0; 3];
        self.tools.cb_magenta_green = [0.0; 3];
        self.tools.cb_yellow_blue = [0.0; 3];
    }

    pub fn reset_cb(&mut self) {
        self.tools.cb_cyan_red = [0.0; 3];
        self.tools.cb_magenta_green = [0.0; 3];
        self.tools.cb_yellow_blue = [0.0; 3];
        self.cancel_cb_preview();
    }

    pub fn update_hsl_preview(&mut self) {
        self.tools.hsl_preview_active = true;
        self.request_render();
    }

    pub fn cancel_hsl_preview(&mut self) {
        if self.tools.hsl_preview_active {
            self.tools.hsl_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_hsl(&mut self) {
        self.tools.hsl_preview_active = false;
        self.push_op(Box::new(HslPanelOp::new(
            self.tools.hsl_hue,
            self.tools.hsl_sat,
            self.tools.hsl_lum,
        )));
        self.tools.hsl_hue = [0.0; 8];
        self.tools.hsl_sat = [0.0; 8];
        self.tools.hsl_lum = [0.0; 8];
    }

    pub fn reset_hsl(&mut self) {
        self.tools.hsl_hue = [0.0; 8];
        self.tools.hsl_sat = [0.0; 8];
        self.tools.hsl_lum = [0.0; 8];
        self.cancel_hsl_preview();
    }

    /// Update the live levels preview and trigger a re-render.
    /// Call this whenever a levels slider changes.
    pub fn update_levels_preview(&mut self) {
        self.tools.levels_preview_active = true;
        self.request_render();
    }

    /// Commit the current levels settings as a permanent pipeline operation.
    pub fn apply_levels(&mut self) {
        self.tools.levels_preview_active = false;
        self.push_op(Box::new(LevelsOp::new(
            self.tools.levels_black,
            self.tools.levels_white,
            self.tools.levels_mid,
        )));
        // Reset sliders for the next use
        self.tools.levels_black = 0.0;
        self.tools.levels_mid = 1.0;
        self.tools.levels_white = 1.0;
    }

    pub fn cancel_levels_preview(&mut self) {
        if self.tools.levels_preview_active {
            self.tools.levels_preview_active = false;
            self.request_render();
        }
    }

    /// Discard the live levels preview and reset sliders.
    pub fn reset_levels(&mut self) {
        self.tools.levels_black = 0.0;
        self.tools.levels_mid = 1.0;
        self.tools.levels_white = 1.0;
        if self.tools.levels_preview_active {
            self.tools.levels_preview_active = false;
            self.request_render();
        }
    }

    /// Show a live 1/4-scale preview of the selected B&W mode.
    /// Call this whenever the mode combobox changes.
    pub fn update_bw_preview(&mut self) {
        self.tools.bw_preview_active = true;
        self.request_render();
    }

    /// Discard the live B&W preview without committing.
    pub fn cancel_bw_preview(&mut self) {
        if self.tools.bw_preview_active {
            self.tools.bw_preview_active = false;
            self.request_render();
        }
    }

    pub fn push_bw(&mut self) {
        self.tools.bw_preview_active = false;
        let op = self.tools.make_bw_op();
        self.push_op(op);
    }

    pub fn reset_bw(&mut self) {
        self.tools.bw_mode_idx = 0;
        self.tools.bw_mixer_r = 0.2126;
        self.tools.bw_mixer_g = 0.7152;
        self.tools.bw_mixer_b = 0.0722;
        self.cancel_bw_preview();
    }

    pub fn remove_op(&mut self, index: usize) {
        if self.pipeline_mut().is_some_and(|p| p.remove_op(index)) {
            self.mark_dirty();
            self.cancel_all_previews();
            self.request_render();
        }
    }
    pub fn reorder_op(&mut self, from: usize, to: usize) {
        if self.pipeline_mut().is_some_and(|p| p.reorder_op(from, to)) {
            self.mark_dirty();
            self.cancel_all_previews();
            self.request_render();
        }
    }
    pub fn toggle_op(&mut self, index: usize) {
        if self.pipeline_mut().is_some_and(|p| p.toggle_op(index)) {
            self.mark_dirty();
            self.cancel_all_previews();
            self.request_render();
        }
    }
    pub fn undo(&mut self) {
        if self.pipeline_mut().is_some_and(|p| p.undo()) {
            self.mark_dirty();
            self.cancel_all_previews();
            self.request_render();
        }
    }
    pub fn redo(&mut self) {
        if self.pipeline_mut().is_some_and(|p| p.redo()) {
            self.mark_dirty();
            self.cancel_all_previews();
            self.request_render();
        }
    }

    /// Mark the project as having unsaved changes and schedule an autosave.
    fn mark_dirty(&mut self) {
        self.is_dirty = true;
        self.autosave_pending = true;
    }

    /// Write the autosave file if a change is pending.  Called every frame from
    /// `poll_background`; is a no-op when nothing has changed.
    fn maybe_write_autosave(&mut self) {
        if !self.autosave_pending {
            return;
        }
        let Some(session_id) = self.autosave_session_id else {
            return;
        };
        let Some(source_path) = self.last_path.clone() else {
            return;
        };
        let Some(store) = &self.copies else { return };
        let Ok((copies, active)) = store.save_states() else {
            return;
        };
        crate::autosave::write(
            session_id,
            &source_path,
            self.project_path.as_deref(),
            &copies,
            active,
        );
        self.autosave_pending = false;
    }

    /// Begin restoring an autosave session.
    ///
    /// Stores the pipeline data from `entry` and opens the source image.
    /// When the image finishes loading the pipeline state will be applied
    /// automatically.  If the source file no longer exists, the error will
    /// appear in the status bar.
    pub fn restore_autosave(&mut self, entry: crate::autosave::AutosaveEntry) {
        let source_path = std::path::PathBuf::from(&entry.data.source_path);
        self.autosave_restore = Some((entry.data.copies, entry.data.active_copy));
        self.autosave_restore_session_id = Some(entry.data.started_at);
        self.open_file(source_path);
    }

    /// Start editing the pipeline op at `index`.  Copies its parameters into
    /// the corresponding tool panel and activates that tool's live preview so
    /// the user sees the current values while they adjust.  No-op when the op
    /// is not one we support editing (returns without changing state).
    pub fn begin_edit(&mut self, index: usize) {
        // Already editing — first, cancel current session.
        self.end_edit();
        let (op_clone, op_name) = {
            let Some(pipeline) = self.pipeline() else {
                return;
            };
            let Some(entry) = pipeline.ops().get(index) else {
                return;
            };
            (entry.operation.clone_box(), entry.operation.name())
        };
        let Some(tool) = load_op_into_tools(op_clone.as_ref(), &mut self.tools) else {
            self.status = format!("This op type cannot be edited: {}", op_name);
            return;
        };
        self.editing = Some(EditSession {
            op_index: index,
            tool,
        });
        // Temporarily disable the op under edit so previewed values are shown
        // in situ rather than stacked on top of its committed output.
        if let Some(p) = self.pipeline_mut() {
            p.set_enabled_no_snapshot(index, false);
        }
        // Turn on this tool's live preview so the user immediately sees the
        // loaded parameters without having to nudge a slider.
        self.activate_preview_for(tool);
        self.request_render();
    }

    /// End the current edit session, re-enabling the op if it was auto-disabled.
    pub fn end_edit(&mut self) {
        let Some(session) = self.editing.take() else {
            return;
        };
        // Re-enable the op (it was toggled off when the session began).
        if let Some(p) = self.pipeline_mut() {
            p.set_enabled_no_snapshot(session.op_index, true);
        }
        self.tools.cancel_all_previews();
        self.request_render();
    }

    /// Replace the op under edit with `new_op` and end the session.
    pub fn commit_edit(&mut self, new_op: Box<dyn Operation>) {
        let Some(session) = self.editing.take() else {
            // Should not happen; fall back to push.
            self.push_op(new_op);
            return;
        };
        self.tools.cancel_all_previews();
        if let Some(p) = self.pipeline_mut() {
            p.set_enabled_no_snapshot(session.op_index, true);
            p.replace_op(session.op_index, new_op);
        }
        self.mark_dirty();
        self.request_render();
    }

    fn activate_preview_for(&mut self, tool: EditingTool) {
        match tool {
            EditingTool::Levels => self.tools.levels_preview_active = true,
            EditingTool::BlackAndWhite => self.tools.bw_preview_active = true,
            EditingTool::BrightnessContrast => self.tools.bc_preview_active = true,
            EditingTool::Saturation => self.tools.sat_preview_active = true,
            EditingTool::Sepia => self.tools.sepia_preview_active = true,
            EditingTool::Sharpen => self.tools.sharpen_preview_active = true,
            EditingTool::ClarityTexture => self.tools.clarity_preview_active = true,
            EditingTool::SplitTone => self.tools.split_preview_active = true,
            EditingTool::Curves => self.tools.curve_preview_active = true,
            EditingTool::Vignette => self.tools.vignette_preview_active = true,
            EditingTool::Vibrance => self.tools.vibrance_preview_active = true,
            EditingTool::HueShift => self.tools.hue_preview_active = true,
            EditingTool::HighlightsShadows => self.tools.hl_preview_active = true,
            EditingTool::ShadowExposure => self.tools.shadow_exp_preview_active = true,
            EditingTool::WhiteBalance => self.tools.wb_preview_active = true,
            EditingTool::FauxHdr => self.tools.hdr_preview_active = true,
            EditingTool::Grain => self.tools.grain_preview_active = true,
            EditingTool::ColorBalance => self.tools.cb_preview_active = true,
            EditingTool::HslPanel => self.tools.hsl_preview_active = true,
            EditingTool::Blur => self.tools.blur_preview_active = true,
            EditingTool::Denoise => self.tools.denoise_preview_active = true,
            EditingTool::NoiseReduction => self.tools.nr_preview_active = true,
        }
    }

    fn push_op(&mut self, op: Box<dyn Operation>) {
        // When an edit session is active, the tool's Apply button replaces the
        // op under edit instead of pushing a new one.  The mask wrapper is
        // skipped in this path — editing preserves the structure of the
        // existing entry (including its own mask wrapper, if any, which we
        // leave untouched by writing the new inner op type unwrapped).
        if self.editing.is_some() {
            self.commit_edit(op);
            return;
        }
        // Wrap in MaskedOp when masking is active.
        let op: Box<dyn Operation> = match self.tools.current_mask_shape() {
            Some(mask) => Box::new(MaskedOp { inner: op, mask }),
            None => op,
        };
        self.cancel_all_previews();
        if let Some(store) = &mut self.copies {
            store.active_pipeline_mut().push_op(op);
            self.mark_dirty();
            self.request_render();
        }
    }

    /// One-click auto-enhance: stretch levels to the 0.5/99.5 percentile,
    /// boost saturation slightly, apply a mild sharpen.  Pushes three ops
    /// as a single atomic batch (one render fired at the end).
    pub fn push_classic_bw(&mut self) {
        if self.copies.is_none() {
            return;
        }
        self.cancel_all_previews();
        if let Some(store) = &mut self.copies {
            let p = store.active_pipeline_mut();
            p.push_op(Box::new(BlackAndWhiteOp::channel_mixer(0.45, 0.35, 0.13)));
            p.push_op(Box::new(BrightnessContrastOp::new(0.03, 0.08)));
            p.push_op(Box::new(VignetteOp::new(0.52, 0.28, 1.0)));
        }
        self.mark_dirty();
        self.request_render();
    }

    pub fn push_auto_enhance(&mut self) {
        if self.copies.is_none() || self.histogram.is_none() {
            return;
        }
        let (black, white) = {
            let hist = self.histogram.as_ref().unwrap();
            percentile_levels(&hist.luma, 0.005, 0.995)
        };
        self.cancel_all_previews();
        if let Some(store) = &mut self.copies {
            let p = store.active_pipeline_mut();
            p.push_op(Box::new(LevelsOp::new(black, white, 1.0)));
            p.push_op(Box::new(SaturationOp::new(1.1)));
            p.push_op(Box::new(SharpenOp::new(0.5)));
        }
        self.mark_dirty();
        self.request_render();
    }

    /// Silently dismiss every tool preview without committing any of them.
    ///
    /// Called automatically whenever the pipeline is mutated through any means
    /// other than a tool's own "Apply" button, so the committed state is always
    /// visible unobscured.  Slider/curve values are preserved so the user can
    /// resume adjusting after the other operation is complete.
    fn cancel_all_previews(&mut self) {
        self.tools.cancel_all_previews();
        self.preview_overlay = None;
        self.preview_overlay_rect = None;
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
        if self.copies.is_none() {
            return;
        }
        if self.loading {
            // Another render is in-flight; mark dirty so we re-render after it.
            self.needs_rerender = true;
            return;
        }

        // Render at reduced scale when a preview op is active so ops run on
        // a fraction of the pixels (~16× fewer at 25%).  Full-res renders are
        // queued automatically once the preview is displayed.
        let is_preview = self.tools.any_preview_active() && !force_full_res;
        let preview_scale = if is_preview {
            Some(PREVIEW_SCALE)
        } else {
            None
        };

        // Collect all pipeline-derived data in a scoped borrow so the borrow
        // is dropped before we call self methods (e.g. make_bw_op) below.
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
        // For full-resolution renders we vacate the cache slot so the render
        // thread receives the sole Arc reference (refcount = 1).  This lets
        // Arc::try_unwrap succeed in the render loop, avoiding a 136 MiB
        // deep_clone before the first operation runs.  Preview renders use
        // the read-only path because they downsample first and never write
        // back to the step cache.
        let start_image = if is_preview {
            self.pipeline().unwrap().best_cached_start().1
        } else {
            self.pipeline_mut().unwrap().take_start_for_render().1
        };

        // Preview op — applied on top of committed result but NOT cached.
        let preview_op: Option<Box<dyn Operation>> = self.tools.preview_op();

        // Track whether the upcoming render involves noise reduction so the UI
        // can show a Cancel button while the (potentially slow) NLM runs.
        let nr_in_flight = self.tools.nr_preview_active
            || preview_op
                .as_deref()
                .is_some_and(|op| op.name() == "noise_reduction")
            || committed_ops
                .iter()
                .flatten()
                .any(|op| op.name() == "noise_reduction");

        // Clear any cancel request left over from a previous render.
        core_cancel::reset();

        self.loading = true;
        self.nr_in_flight = nr_in_flight;
        self.status = "Rendering…".into();
        self.render_start = Some(std::time::Instant::now());

        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();

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

        std::thread::Builder::new()
            .name("rasterlab-render".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let msg = match render_in_thread(
                    start_image,
                    committed_ops,
                    preview_op,
                    preview_scale,
                    preview_viewport,
                    overlay_viewport,
                ) {
                    Ok((image, hist, intermediates, overlay_rect)) => BgMessage::RenderComplete {
                        image,
                        hist: Box::new(hist),
                        intermediates,
                        start_index: start_idx,
                        cache_gen,
                        is_preview,
                        overlay_rect,
                    },
                    Err(e) => {
                        // If a cancel was requested, the op returned
                        // RasterError::Cancelled — surface it as a clean
                        // BgMessage::Cancelled rather than a red error.
                        if core_cancel::is_requested() {
                            BgMessage::Cancelled
                        } else {
                            BgMessage::Error(e)
                        }
                    }
                };
                let _ = tx.send(msg);
                ctx.request_repaint();
            })
            .expect("failed to spawn render thread");
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    /// Borrow the active pipeline, if any image is loaded.
    pub fn pipeline(&self) -> Option<&EditPipeline> {
        self.copies.as_ref().map(|s| s.active_pipeline())
    }

    /// Borrow the source image metadata for the active pipeline, if any.
    pub fn image_metadata(&self) -> Option<&rasterlab_core::image::ImageMetadata> {
        self.pipeline().map(|p| &p.source().metadata)
    }

    /// Mutably borrow the active pipeline, if any image is loaded.
    fn pipeline_mut(&mut self) -> Option<&mut EditPipeline> {
        self.copies.as_mut().map(|s| s.active_pipeline_mut())
    }

    pub fn can_undo(&self) -> bool {
        self.pipeline().is_some_and(|p| p.can_undo())
    }
    pub fn can_redo(&self) -> bool {
        self.pipeline().is_some_and(|p| p.can_redo())
    }

    // -----------------------------------------------------------------------
    // Virtual copy management
    // -----------------------------------------------------------------------

    /// Add a new empty virtual copy and make it active.
    pub fn add_virtual_copy(&mut self) {
        if let Some(store) = &mut self.copies {
            let n = store.len() + 1;
            store.add_copy(format!("Copy {}", n));
        }
        self.cancel_all_previews();
        self.mark_dirty();
        self.request_render();
    }

    /// Duplicate the active copy (same ops) and make it active.
    pub fn duplicate_virtual_copy(&mut self) {
        if let Some(store) = &mut self.copies {
            let n = store.len() + 1;
            if let Err(e) = store.duplicate_active(format!("Copy {}", n)) {
                self.status = format!("Duplicate failed: {}", e);
                return;
            }
        }
        self.cancel_all_previews();
        self.mark_dirty();
        self.request_render();
    }

    /// Switch to the copy at `index` and re-render.
    pub fn switch_copy(&mut self, index: usize) {
        if let Some(store) = &mut self.copies {
            if index == store.active_index() {
                return;
            }
            store.set_active(index);
        }
        self.cancel_all_previews();
        self.request_render();
    }

    /// Remove the copy at `index` (refused silently when only one copy exists).
    pub fn remove_virtual_copy(&mut self, index: usize) {
        let removed = self.copies.as_mut().is_some_and(|s| s.remove(index));
        if removed {
            self.cancel_all_previews();
            self.mark_dirty();
            self.request_render();
        }
    }

    /// Rename the copy at `index`.
    pub fn rename_virtual_copy(&mut self, index: usize, name: String) {
        if let Some(store) = &mut self.copies {
            store.rename(index, name);
        }
        self.mark_dirty();
    }
}

// ---------------------------------------------------------------------------
// Free functions
// ---------------------------------------------------------------------------

/// Compute the largest axis-aligned `CropOp` that fits inside a `W×H` image
/// rotated by `angle_deg` degrees while preserving the original W:H aspect ratio.
///
/// After an arbitrary rotation the output has black/transparent corners.  This
/// gives the tightest crop that removes all of them while keeping the same shape.
fn straighten_crop_op(w: u32, h: u32, angle_deg: f32) -> CropOp {
    let theta = angle_deg.to_radians().abs();
    let cos_t = theta.cos();
    let sin_t = theta.sin();
    let wf = w as f32;
    let hf = h as f32;
    let r = wf / hf; // aspect ratio

    // Solve for the half-height `b` of the largest inscribed rectangle with
    // the same aspect ratio:  a = r·b,  subject to
    //   a·cos_t + b·sin_t ≤ W/2
    //   a·sin_t + b·cos_t ≤ H/2
    let b = f32::min(
        wf / (2.0 * (r * cos_t + sin_t)),
        hf / (2.0 * (r * sin_t + cos_t)),
    );
    let a = r * b;

    // Full-pixel inner dimensions (floor to stay inside the rotated image).
    let inner_w = (2.0 * a).floor() as u32;
    let inner_h = (2.0 * b).floor() as u32;

    // Post-rotation bounding box dimensions (matches rotate_arbitrary).
    let rot_w = (wf * cos_t + hf * sin_t).ceil() as u32;
    let rot_h = (wf * sin_t + hf * cos_t).ceil() as u32;

    // Centre the crop inside the rotated bounding box.
    let x = (rot_w.saturating_sub(inner_w)) / 2;
    let y = (rot_h.saturating_sub(inner_h)) / 2;

    CropOp::new(x, y, inner_w.max(1), inner_h.max(1))
}

// ---------------------------------------------------------------------------
// Free functions: run in the render thread
// ---------------------------------------------------------------------------

/// Linear scale factor used for the fast downsampled preview.
/// 0.25 = 1/4 width × 1/4 height = 1/16 the pixels → ~16× faster ops.
const PREVIEW_SCALE: f32 = 0.25;

type RenderResult = Result<(Arc<Image>, HistogramData, Vec<Arc<Image>>, Option<[u32; 4]>), String>;

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
    committed_ops: Vec<Option<Box<dyn Operation>>>,
    preview_op: Option<Box<dyn Operation>>,
    preview_scale: Option<f32>,
    preview_viewport: Option<[u32; 4]>,
    overlay_viewport: Option<[u32; 4]>,
) -> RenderResult {
    // ── Overlay path ─────────────────────────────────────────────────────
    // The pipeline is fully cached so committed_ops is empty.  Crop the
    // committed result to exactly the visible viewport, apply the preview
    // op at full resolution, and return it as a positioned overlay.  The
    // main `state.rendered` image is never replaced, so the canvas stays
    // stable and sharp.
    if let (Some(op), Some([vp_x, vp_y, vp_w, vp_h])) = (&preview_op, overlay_viewport) {
        let mut current = start_image;
        // Run committed ops (should be empty — all_cached — but be safe).
        for committed in committed_ops.iter().flatten() {
            let img = match Arc::try_unwrap(current) {
                Ok(img) => img,
                Err(a) => a.as_ref().deep_clone(),
            };
            let result = committed
                .apply(img)
                .map_err(|e| format!("Op '{}' failed: {}", committed.name(), e))?;
            debug_validate_image(&result, committed.name());
            current = Arc::new(result);
        }
        // Clamp viewport to image bounds.
        let x = vp_x.min(current.width.saturating_sub(1));
        let y = vp_y.min(current.height.saturating_sub(1));
        let w = vp_w.min(current.width.saturating_sub(x)).max(1);
        let h = vp_h.min(current.height.saturating_sub(y)).max(1);

        let crop = extract_region(current.as_ref(), x, y, w, h);
        let processed = op
            .apply(crop)
            .map_err(|e| format!("Op '{}' (overlay) failed: {}", op.name(), e))?;
        debug_validate_image(&processed, op.name());

        let hist = HistogramData::compute(&processed);
        return Ok((Arc::new(processed), hist, Vec::new(), Some([x, y, w, h])));
    }

    // ── Fallback: downsampled-blit path ───────────────────────────────────
    let is_preview = preview_scale.is_some();
    let mut current: Arc<Image> = match preview_scale {
        Some(scale) => Arc::new(downsample_nn(start_image.as_ref(), scale)),
        None => start_image,
    };
    let mut intermediates = if !is_preview {
        Vec::with_capacity(committed_ops.len())
    } else {
        Vec::new()
    };

    for maybe_op in committed_ops {
        if let Some(op) = maybe_op {
            // Scale geometric ops (e.g. crop) so their pixel coordinates
            // match the downsampled image dimensions.
            let op = match preview_scale {
                Some(s) => op.scaled_for_preview(s),
                None => op,
            };
            let img = match Arc::try_unwrap(current) {
                Ok(img) => img,
                Err(a) => a.as_ref().deep_clone(),
            };
            let result = op
                .apply(img)
                .map_err(|e| format!("Op '{}' failed: {}", op.name(), e))?;
            debug_validate_image(&result, op.name());
            current = Arc::new(result);
        }
        if !is_preview {
            intermediates.push(Arc::clone(&current));
        }
    }

    if let Some(op) = preview_op {
        if let Some([vp_x, vp_y, vp_w, vp_h]) = preview_viewport {
            let scale = preview_scale.unwrap_or(1.0);
            let sx = ((vp_x as f32 * scale) as u32).min(current.width.saturating_sub(1));
            let sy = ((vp_y as f32 * scale) as u32).min(current.height.saturating_sub(1));
            let sw = ((vp_w as f32 * scale).ceil() as u32)
                .min(current.width.saturating_sub(sx))
                .max(1);
            let sh = ((vp_h as f32 * scale).ceil() as u32)
                .min(current.height.saturating_sub(sy))
                .max(1);
            let crop = extract_region(current.as_ref(), sx, sy, sw, sh);
            let processed = op
                .apply(crop)
                .map_err(|e| format!("Op '{}' (preview) failed: {}", op.name(), e))?;
            debug_validate_image(&processed, op.name());
            let base = match Arc::try_unwrap(current) {
                Ok(img) => img,
                Err(a) => a.as_ref().deep_clone(),
            };
            // If the op changed image dimensions (e.g. 90°/270° rotation) the
            // crop-blit optimisation is invalid — fall back to full-image apply.
            if processed.width == sw && processed.height == sh {
                let mut base = base;
                blit_region(&mut base, &processed, sx, sy);
                current = Arc::new(base);
            } else {
                let result = op
                    .apply(base)
                    .map_err(|e| format!("Op '{}' (preview) failed: {}", op.name(), e))?;
                debug_validate_image(&result, op.name());
                current = Arc::new(result);
            }
        } else {
            let img = match Arc::try_unwrap(current) {
                Ok(img) => img,
                Err(a) => a.as_ref().deep_clone(),
            };
            let result = op
                .apply(img)
                .map_err(|e| format!("Op '{}' (preview) failed: {}", op.name(), e))?;
            debug_validate_image(&result, op.name());
            current = Arc::new(result);
        }
    }

    let hist = HistogramData::compute(current.as_ref());
    Ok((current, hist, intermediates, None))
}

/// Debug-only validation that an Image's buffer matches its declared dimensions.
#[inline]
fn debug_validate_image(image: &Image, op_name: &str) {
    debug_assert_eq!(
        image.data.len(),
        image.width as usize * image.height as usize * 4,
        "Operation '{}' returned an Image with mismatched buffer: \
         data.len()={} but {}x{}x4={}",
        op_name,
        image.data.len(),
        image.width,
        image.height,
        image.width as usize * image.height as usize * 4,
    );
}

/// Extract a rectangular region from `src` into a new Image without cloning
/// the full source buffer.  Only `w × h × 4` bytes are read and written,
/// compared with `src_w × src_h × 4` bytes for a deep_clone + CropOp.
fn extract_region(src: &Image, x: u32, y: u32, w: u32, h: u32) -> Image {
    use rayon::prelude::*;
    let mut out = Image::new(w, h);
    let src_stride = src.width as usize * 4;
    let x_off = x as usize * 4;
    let row_bytes = w as usize * 4;
    out.data
        .par_chunks_mut(row_bytes)
        .enumerate()
        .for_each(|(dst_y, dst_row)| {
            let src_start = (y as usize + dst_y) * src_stride + x_off;
            dst_row.copy_from_slice(&src.data[src_start..src_start + row_bytes]);
        });
    out
}

/// Copy `src` into `dst` at pixel offset `(x, y)`.  Caller must ensure the
/// region fits within `dst`.
fn blit_region(dst: &mut Image, src: &Image, x: u32, y: u32) {
    use rayon::prelude::*;
    let row_bytes = src.width as usize * 4;
    let dst_stride = dst.width as usize * 4;
    // Slice only the rows that receive data — avoids iterating the full image
    // height and filtering in rayon (which still schedules tasks for empty rows).
    let start = y as usize * dst_stride;
    let end = start + src.height as usize * dst_stride;
    let x_off = x as usize * 4;
    dst.data[start..end]
        .par_chunks_mut(dst_stride)
        .enumerate()
        .for_each(|(src_row, dst_row)| {
            let src_off = src_row * row_bytes;
            dst_row[x_off..x_off + row_bytes]
                .copy_from_slice(&src.data[src_off..src_off + row_bytes]);
        });
}
