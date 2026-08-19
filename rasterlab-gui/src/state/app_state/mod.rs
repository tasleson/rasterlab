//! Central application state.
//!
//! [`AppState`] owns everything the GUI reads each frame. Its behaviour is
//! split across sibling modules by responsibility:
//!
//! - [`persistence`] — opening/saving images and `.rlab` projects, dirty
//!   tracking, and the autosave session.
//! - [`render`] — deciding what to render, spawning the background render, and
//!   folding results back in.
//! - [`library_tasks`] — imports, scrubs, index rebuilds, and thumbnails.
//!
//! What stays here is the state itself, the background message pump that routes
//! to those modules, and the edit-stack mutations the tool panels drive.

mod library_tasks;
mod persistence;
mod render;
mod workers;

use std::{
    path::PathBuf as StdPathBuf,
    sync::{Arc, atomic::AtomicBool, mpsc},
};

use crate::{
    prefs::Prefs,
    state::{LibraryState, VirtualCopyStore},
};

use egui::Context;
use rasterlab_core::{
    Image,
    analysis::{ImageStats, PlanMode},
    formats::FormatRegistry,
    ops::{
        BlackAndWhiteOp, BrightnessContrastOp, CropOp, HistogramData, LevelsOp, MaskedOp,
        SaturationOp, SharpenOp, SprocketFilmOp, VignetteOp,
    },
    pipeline::EditPipeline,
    project::RlabFile,
    traits::operation::Operation,
};
use rasterlab_gpu::GpuContext;
use rasterlab_render::RenderResult;

use self::render::{NrPreviewSignature, ProcessingBackend, ReusableNrPreview};
use super::virtual_copies::EditStateSnapshot;
use super::{EditSession, EditingTool, ToolState, load_op_into_tools};

// ---------------------------------------------------------------------------
// App mode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppMode {
    #[default]
    Editor,
    Library,
}

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
    /// Result from the background render thread (via `rasterlab-render` crate).
    Render(RenderResult),
    /// A background thread failed at loading the open document. Terminal for
    /// the editor's `loading` flag.
    Error(String),
    /// An auxiliary background task failed — a thumbnail that could not be
    /// rebuilt, say. Reported without touching `loading`, which belongs to
    /// whatever render or load is actually in flight.
    TaskFailed(String),
    /// Progress update from a running import.
    ImportProgress(rasterlab_library::ImportProgress),
    /// Import finished; thumbnail cache should be invalidated.
    ImportComplete {
        session: rasterlab_library::ImportSession,
        errors: Vec<(StdPathBuf, String)>,
    },
    /// The import worker gave up, panicked, or never started. Terminal, so the
    /// progress bar it owns has to be torn down.
    ImportFailed(String),
    /// A thumbnail image was loaded from disk; ready to upload to egui.
    ThumbLoaded { hash: String, bytes: Vec<u8> },
    /// Progress update from a running integrity scrub.
    ScrubProgress(rasterlab_library::ScrubProgress),
    /// Progress update from a running index rebuild.
    RebuildProgress(rasterlab_library::RebuildProgress),
    /// Index rebuild finished. `fatal` is set if the rebuild aborted early;
    /// `errors` are per-file failures from a run that otherwise completed.
    RebuildComplete {
        total: usize,
        errors: Vec<(StdPathBuf, String)>,
        fatal: Option<String>,
    },
    /// Scrub finished (completed or cancelled).
    ScrubComplete {
        outcome: rasterlab_library::ScrubOutcome,
    },
    /// The scrub worker gave up, panicked, or never started. Terminal, so the
    /// cancellation handle that drives the Start/Stop toggle has to be released.
    ScrubFailed(String),
}

impl From<RenderResult> for BgMessage {
    fn from(r: RenderResult) -> Self {
        BgMessage::Render(r)
    }
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
    /// `true` when effective edits differ from the last open, save, or export.
    pub is_dirty: bool,
    /// Effective edit state at the last open/save boundary. Dirty state is
    /// recomputed against this so undoing or removing every edit becomes clean.
    clean_edit_state: Option<EditStateSnapshot>,
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
    /// Sender into the bounded thumbnail-loader pool (lazily spawned on first
    /// request). A fixed worker count prevents a large grid from spawning one
    /// OS thread per visible photo.
    thumb_req_tx: Option<mpsc::Sender<library_tasks::ThumbLoadRequest>>,
    // egui context — needed to wake up the UI after background work completes
    ctx: Context,
    gpu: Option<Arc<GpuContext>>,

    /// All per-tool input fields, preview flags, and export settings.
    pub tools: ToolState,

    /// Set when a slider changes while a render is in-flight; triggers a
    /// follow-up render as soon as the current one completes.
    needs_rerender: bool,
    /// Wall-clock time at which the most recent render thread was spawned.
    render_start: Option<std::time::Instant>,
    render_backend: Option<ProcessingBackend>,
    /// True when the in-flight render includes a noise-reduction op (either
    /// as the active preview or as a committed pipeline step).  Drives the
    /// visibility of the NR Cancel button so the user can abort a slow NLM run.
    nr_in_flight: bool,
    reusable_nr_preview: Option<ReusableNrPreview>,
    pending_nr_preview_key: Option<(usize, usize, u64, NrPreviewSignature)>,

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

    // ── App mode & library ────────────────────────────────────────────────────
    pub mode: AppMode,
    pub library: LibraryState,
    /// Set when the Editor opens a file that was imported into a library.
    /// `(library_root, hash)` — on save triggers thumb regen + DB sync.
    pub library_context: Option<(StdPathBuf, String)>,

    /// Cancellation flag for a running integrity scrub. `Some` while a scrub is
    /// in flight (drives the File-menu Start/Stop toggle); cleared on completion.
    scrub_cancel: Option<Arc<AtomicBool>>,
}

/// Largest centred 2:1 rectangle that fits inside the image.
fn centered_sprocket_crop(image_width: u32, image_height: u32) -> [u32; 4] {
    if image_width < 2 || image_height == 0 {
        return [0, 0, image_width.max(1), image_height.max(1)];
    }
    let crop_height = image_height.min(image_width / 2).max(1);
    let crop_width = crop_height.saturating_mul(2).min(image_width);
    [
        (image_width - crop_width) / 2,
        (image_height - crop_height) / 2,
        crop_width,
        crop_height,
    ]
}

/// Clamp a user-positioned crop to the current image while preserving an
/// exact 2:1 pixel ratio. Normal images are at least two pixels wide; the tiny
/// fallback simply preserves the only available pixel.
fn clamp_sprocket_crop(crop: [u32; 4], image_width: u32, image_height: u32) -> [u32; 4] {
    if image_width < 2 || image_height == 0 {
        return [0, 0, image_width.max(1), image_height.max(1)];
    }
    let [x, y, requested_width, requested_height] = crop;
    let max_height = image_height.min(image_width / 2).max(1);
    let width_height = (requested_width / 2).max(1);
    let crop_height = requested_height.max(1).min(width_height).min(max_height);
    let crop_width = crop_height * 2;
    let crop_x = x.min(image_width - crop_width);
    let crop_y = y.min(image_height - crop_height);
    [crop_x, crop_y, crop_width, crop_height]
}

impl AppState {
    pub fn new(ctx: Context, gpu: Option<Arc<GpuContext>>) -> Self {
        let (bg_tx, bg_rx) = mpsc::channel();
        let prefs = Prefs::load();
        let mut tools = ToolState::new();
        tools.encode_opts.jpeg_quality = prefs.jpeg_quality;
        tools.encode_opts.png_compression = prefs.png_compression;
        tools.encode_opts.preserve_metadata = prefs.preserve_metadata;
        tools.export_border = prefs.export_border.clone();
        let initial_thumb_scale = prefs.library_thumb_scale;
        Self {
            prefs,
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
            clean_edit_state: None,
            project_created_at: None,
            image_generation: 0,
            split_view: false,
            split_mode: SplitMode::VsOriginal,
            split_focus: None,
            tools_force_open: None,
            bg_tx,
            bg_rx,
            thumb_req_tx: None,
            ctx,
            gpu,
            tools,
            needs_rerender: false,
            render_start: None,
            render_backend: None,
            nr_in_flight: false,
            reusable_nr_preview: None,
            pending_nr_preview_key: None,
            autosave_session_id: None,
            autosave_pending: false,
            autosave_restore: None,
            autosave_restore_session_id: None,
            editing: None,
            mode: AppMode::Editor,
            library: LibraryState {
                thumb_scale: initial_thumb_scale,
                ..Default::default()
            },
            library_context: None,
            scrub_cancel: None,
        }
    }

    // -----------------------------------------------------------------------
    // Background message pump — call once per frame from update()
    // -----------------------------------------------------------------------

    /// Drain every message posted by a background thread and route it to the
    /// module that owns that responsibility.
    pub fn poll_background(&mut self) {
        while let Ok(msg) = self.bg_rx.try_recv() {
            match msg {
                BgMessage::ImageLoaded {
                    path,
                    image,
                    original_bytes,
                } => self.on_image_loaded(path, image, original_bytes),
                BgMessage::ProjectLoaded { path, rlab, image } => {
                    self.on_project_loaded(path, rlab, image)
                }
                BgMessage::Render(result) => self.on_render_result(result),
                BgMessage::Error(e) => {
                    self.status = format!("Error: {}", e);
                    self.loading = false;
                }
                BgMessage::TaskFailed(e) => self.status = format!("Error: {}", e),
                BgMessage::ImportProgress(p) => self.on_import_progress(p),
                BgMessage::ImportComplete { session, errors } => {
                    self.on_import_complete(session, errors)
                }
                BgMessage::ImportFailed(e) => self.on_import_failed(e),
                BgMessage::ThumbLoaded { hash, bytes } => self.on_thumb_loaded(hash, bytes),
                BgMessage::ScrubProgress(p) => self.on_scrub_progress(p),
                BgMessage::RebuildProgress(p) => self.on_rebuild_progress(p),
                BgMessage::RebuildComplete {
                    total,
                    errors,
                    fatal,
                } => self.on_rebuild_complete(total, errors, fatal),
                BgMessage::ScrubComplete { outcome } => self.on_scrub_complete(outcome),
                BgMessage::ScrubFailed(e) => self.on_scrub_failed(e),
            }
        }
        self.update_processing_status();
        self.maybe_write_autosave();
    }

    // -----------------------------------------------------------------------
    // Pipeline mutations (always followed by request_render)
    // -----------------------------------------------------------------------

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

    /// Start editing the pipeline op at `index`.  Copies its parameters into
    /// the corresponding tool panel and activates that tool's live preview so
    /// the user sees the current values while they adjust.  No-op when the op
    /// is not one we support editing (returns without changing state).
    pub fn begin_edit(&mut self, index: usize) {
        // Already editing — first, cancel current session.
        self.end_edit();
        let (op_clone, op_name, was_enabled) = {
            let Some(pipeline) = self.pipeline() else {
                return;
            };
            let Some(entry) = pipeline.ops().get(index) else {
                return;
            };
            (
                entry.operation.clone_box(),
                entry.operation.name(),
                entry.enabled,
            )
        };
        let Some(tool) = load_op_into_tools(op_clone.as_ref(), &mut self.tools) else {
            self.status = format!("This op type cannot be edited: {}", op_name);
            return;
        };
        self.editing = Some(EditSession {
            op_index: index,
            tool,
            was_enabled,
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
        // Restore the op's original enabled state (it was temporarily hidden
        // while the editor's preview took its place).
        if let Some(p) = self.pipeline_mut() {
            p.set_enabled_no_snapshot(session.op_index, session.was_enabled);
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
            let mask = p
                .ops()
                .get(session.op_index)
                .and_then(|entry| entry.operation.as_any())
                .and_then(|any| any.downcast_ref::<MaskedOp>())
                .map(|masked| masked.mask.clone());
            let new_op: Box<dyn Operation> = if let Some(mask) = mask {
                Box::new(MaskedOp {
                    inner: new_op,
                    mask,
                })
            } else {
                new_op
            };
            p.set_enabled_no_snapshot(session.op_index, session.was_enabled);
            p.replace_op(session.op_index, new_op);
        }
        self.mark_dirty();
        self.request_render();
    }

    fn activate_preview_for(&mut self, tool: EditingTool) {
        if tool == EditingTool::SprocketFilm {
            self.tools.sprocket_edit_preview_active = true;
            return;
        }
        for t in &mut self.tools.tools {
            if t.editing_tool() == Some(tool) {
                t.activate_preview();
                break;
            }
        }
    }

    pub(crate) fn push_op(&mut self, op: Box<dyn Operation>) {
        // When an edit session is active, the tool's Apply button replaces the
        // op under edit instead of pushing a new one. `commit_edit` preserves
        // an existing mask wrapper rather than applying the currently selected
        // global mask as though this were a new operation.
        if self.editing.is_some() {
            self.commit_edit(op);
            return;
        }
        if self.try_push_reusable_nr_preview(op.as_ref()) {
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

    /// Monochrome look: channel-mixer black and white, a gentle tone lift, and
    /// a vignette.  Pushed as a single atomic batch (one render at the end).
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

    /// Start an interactive, fixed 2:1 crop for the sprocket panorama look.
    pub fn begin_sprocket_crop(&mut self) {
        if self.copies.is_none() || self.editing.is_some() {
            return;
        }
        let Some(rendered) = self.rendered.as_ref() else {
            return;
        };

        let scale = self.rendered_scale.max(0.01);
        let width = (rendered.width as f32 / scale).round().max(1.0) as u32;
        let height = (rendered.height as f32 / scale).round().max(1.0) as u32;
        let [x, y, w, h] = centered_sprocket_crop(width, height);

        self.cancel_all_previews();
        self.tools.mask_sel = 0;
        if let Some(heal) = self
            .tools
            .find_mut::<crate::panels::tools::heal::HealTool>()
        {
            heal.active = false;
        }
        if let Some(straighten) = self
            .tools
            .find_mut::<crate::panels::tools::straighten::StraightenTool>()
        {
            straighten.active = false;
        }
        if let Some(crop) = self
            .tools
            .find_mut::<crate::panels::tools::crop::CropTool>()
        {
            crop.x = x;
            crop.y = y;
            crop.w = w;
            crop.h = h;
        }
        self.tools.sprocket_crop_active = true;
        self.status = "Position the 2:1 crop, then apply the sprocket look".into();
    }

    pub fn cancel_sprocket_crop(&mut self) {
        self.tools.sprocket_crop_active = false;
        self.status = "Cancelled 35mm Sprocket Panorama crop".into();
    }

    /// Recreate a full-width 35 mm negative using the positioned 2:1 crop and
    /// the selected stock (or a randomized stock when no selection was made).
    pub fn push_sprocket_panorama(&mut self) {
        if self.copies.is_none() {
            return;
        }
        let Some(rendered) = self.rendered.as_ref() else {
            return;
        };

        // `rendered` may currently be a quarter-scale tool preview. Convert
        // back to the dimensions seen by the committed pipeline before making
        // an absolute-pixel crop operation.
        let scale = self.rendered_scale.max(0.01);
        let width = (rendered.width as f32 / scale).round().max(1.0) as u32;
        let height = (rendered.height as f32 / scale).round().max(1.0) as u32;
        let requested_crop = self
            .tools
            .find::<crate::panels::tools::crop::CropTool>()
            .map_or(centered_sprocket_crop(width, height), |crop| {
                [crop.x, crop.y, crop.w, crop.h]
            });
        let [crop_x, crop_y, crop_w, crop_h] = clamp_sprocket_crop(requested_crop, width, height);
        let film_op = self.tools.sprocket_film_stock.map_or_else(
            SprocketFilmOp::randomized,
            SprocketFilmOp::with_random_markings,
        );
        let film_description = film_op.describe();

        self.cancel_all_previews();
        if let Some(store) = &mut self.copies {
            let pipeline = store.active_pipeline_mut();
            if [crop_x, crop_y, crop_w, crop_h] != [0, 0, width, height] {
                pipeline.push_op(Box::new(CropOp::new(crop_x, crop_y, crop_w, crop_h)));
            }
            pipeline.push_op(Box::new(film_op));
        }
        self.mark_dirty();
        self.status = format!("Applied {film_description}");
        self.request_render();
    }

    /// Analyse the current image and push whatever corrections it needs.
    ///
    /// Unlike [`Self::push_auto_enhance`], which applies fixed preset values,
    /// this measures the rendered image (colour cast, tone, chroma,
    /// sharpness) and derives per-image parameter values in closed form.
    /// Each correction lands as its own op in the edit stack so it can be
    /// inspected, tweaked, or undone individually.
    ///
    /// Uses every measurement available, including the regional ones, so an
    /// unevenly-lit frame may also get local tone.
    pub fn push_adaptive_enhance(&mut self) {
        self.push_analysis_plan("Adaptive Enhance", PlanMode::Adaptive);
    }

    /// Push global-only corrections, tuned for faded prints and scans.
    ///
    /// The same analysis as [`Self::push_adaptive_enhance`], but declining the
    /// regional judgements: no border exclusion, no local tone.  This is the
    /// behaviour the planner's constants were originally calibrated against.
    pub fn push_old_photo_restore(&mut self) {
        self.push_analysis_plan("Old Photo Restore", PlanMode::Restore);
    }

    fn push_analysis_plan(&mut self, label: &str, mode: PlanMode) {
        let Some(rendered) = self.rendered.clone() else {
            return;
        };
        if self.copies.is_none() {
            return;
        }

        let stats = ImageStats::compute(&rendered);
        let plan = rasterlab_core::analysis::plan_from_stats(&rendered, &stats, mode);
        if plan.is_empty() {
            self.status = format!("{label}: no corrections needed");
            return;
        }
        self.status = format!("{label}: {}", plan.summary());

        self.cancel_all_previews();
        if let Some(store) = &mut self.copies {
            let p = store.active_pipeline_mut();
            for op in plan.into_ops() {
                p.push_op(op);
            }
        }
        self.mark_dirty();
        self.request_render();
    }

    /// One-click auto-enhance: stretch levels to the 0.5/99.5 percentile,
    /// boost saturation slightly, apply a mild sharpen.
    pub fn push_auto_enhance(&mut self) {
        if self.copies.is_none() || self.histogram.is_none() {
            return;
        }
        let (black, white) = {
            let hist = self.histogram.as_ref().unwrap();
            rasterlab_render::percentile_levels(&hist.luma, 0.005, 0.995)
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
    pub(crate) fn cancel_all_previews(&mut self) {
        self.tools.cancel_all_previews();
        self.preview_overlay = None;
        self.preview_overlay_rect = None;
        self.pending_nr_preview_key = None;
        self.reusable_nr_preview = None;
    }

    /// Reset tool-specific state when a new image is loaded.
    ///
    /// This also drops every preview: the overlay and cached noise-reduction
    /// preview describe the image being replaced, and a still-active tool
    /// preview would be applied to the new image on its very first render.
    fn reset_tools_for_new_image(&mut self, w: u32, h: u32) {
        self.cancel_all_previews();
        self.tools.reset_for_new_image(w, h);
    }

    // -----------------------------------------------------------------------
    // Tool-specific helpers (delegate to per-tool structs)
    // -----------------------------------------------------------------------

    pub fn load_lut(&mut self, path: std::path::PathBuf) {
        use crate::panels::tools::lut::LutTool;
        use rasterlab_core::ops::LutOp;

        let strength = self.tools.find::<LutTool>().unwrap().strength;
        match std::fs::read_to_string(&path) {
            Ok(src) => match LutOp::from_cube_str(&src, strength) {
                Ok(mut op) => {
                    op.strength = strength;
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    self.status = format!("Loaded LUT: {name}");
                    let tool = self.tools.find_mut::<LutTool>().unwrap();
                    tool.name = name;
                    tool.lut_op = Some(op);
                    tool.preview_active = false;
                }
                Err(e) => {
                    self.status = format!("LUT parse error: {e}");
                }
            },
            Err(e) => {
                self.status = format!("Cannot read LUT file: {e}");
            }
        }
    }

    pub fn panorama_add_image(&mut self, path: std::path::PathBuf) {
        use crate::panels::tools::panorama::PanoramaTool;
        use crate::panels::tools::shared::StackFrame;
        let tool = self.tools.find_mut::<PanoramaTool>().unwrap();
        tool.frames.push(StackFrame::new(path.to_string_lossy()));
        let needs_render = tool.frames.len() >= 2;
        if needs_render {
            tool.preview_active = true;
        }
        if needs_render {
            self.request_render();
        }
    }

    pub fn focus_stack_add_image(&mut self, path: std::path::PathBuf) {
        use crate::panels::tools::focus_stack::FocusStackTool;
        use crate::panels::tools::shared::StackFrame;
        let tool = self.tools.find_mut::<FocusStackTool>().unwrap();
        tool.frames.push(StackFrame::new(path.to_string_lossy()));
        let needs_render = tool.frames.len() >= 2;
        if needs_render {
            tool.preview_active = true;
        }
        if needs_render {
            self.request_render();
        }
    }

    /// Load `paths` into the Focus Stack tool as its frame list, replacing
    /// whatever was there, and reveal the tool in the panel.
    ///
    /// Used by the library grid's Focus Stack action. No preview is started:
    /// the op reloads and fuses every frame at full resolution, which for a
    /// bulk selection of RAW frames is minutes of work the user has not asked
    /// for yet. They press Stack when ready.
    pub fn load_focus_stack_frames(&mut self, paths: Vec<std::path::PathBuf>) {
        use crate::panels::tools::focus_stack::FocusStackTool;
        use crate::panels::tools::shared::StackFrame;
        use crate::panels::tools::tool_trait::Tool;

        self.cancel_all_previews();
        let tool = self.tools.find_mut::<FocusStackTool>().unwrap();
        tool.frames = paths
            .into_iter()
            .map(|p| StackFrame::new(p.to_string_lossy()))
            .collect();
        let id = tool.id();
        self.tools.reveal_tool = Some(id);
    }

    pub fn hdr_merge_add_image(&mut self, path: std::path::PathBuf) {
        use crate::panels::tools::hdr_merge::HdrMergeTool;
        use crate::panels::tools::shared::StackFrame;
        let tool = self.tools.find_mut::<HdrMergeTool>().unwrap();
        tool.frames.push(StackFrame::new(path.to_string_lossy()));
        let needs_render = tool.frames.len() >= 2;
        if needs_render {
            tool.preview_active = true;
        }
        if needs_render {
            self.request_render();
        }
    }

    pub fn heal_place_spot(&mut self, dest_x: i32, dest_y: i32) {
        use crate::panels::tools::heal::HealTool;
        use rasterlab_core::ops::{HealOp, HealSpot};
        let radius = self.tools.find::<HealTool>().unwrap().radius;
        let src = if let Some(rendered) = &self.rendered {
            HealOp::auto_detect_source(rendered, dest_x, dest_y, radius)
        } else {
            (dest_x + radius as i32 * 2, dest_y)
        };
        self.tools
            .find_mut::<HealTool>()
            .unwrap()
            .spots
            .push(HealSpot {
                dest_x,
                dest_y,
                src_x: src.0,
                src_y: src.1,
                radius,
            });
    }

    pub fn update_straighten_preview(&mut self) {
        use crate::panels::tools::straighten::StraightenTool;
        self.tools
            .find_mut::<StraightenTool>()
            .unwrap()
            .preview_active = true;
        self.request_render();
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

#[cfg(test)]
mod sprocket_crop_tests {
    use rasterlab_core::ops::{LinearMask, MaskShape, MaskedOp, SaturationOp, SepiaOp};

    use super::{
        AppState, EditPipeline, EditSession, EditingTool, Image, VirtualCopyStore,
        centered_sprocket_crop, clamp_sprocket_crop,
    };

    #[test]
    fn centers_two_to_one_crop_in_portrait_source() {
        assert_eq!(centered_sprocket_crop(4000, 3000), [0, 500, 4000, 2000]);
    }

    #[test]
    fn centers_two_to_one_crop_in_wide_source() {
        assert_eq!(centered_sprocket_crop(5000, 2000), [500, 0, 4000, 2000]);
    }

    #[test]
    fn positioned_crop_is_clamped_without_losing_ratio() {
        assert_eq!(
            clamp_sprocket_crop([3900, 2900, 2000, 1000], 4000, 3000),
            [2000, 2000, 2000, 1000]
        );
    }

    fn state_with_op(op: Box<dyn rasterlab_core::traits::operation::Operation>) -> AppState {
        let mut pipeline = EditPipeline::new(Image::new(8, 8));
        pipeline.push_op(op);
        let mut state = AppState::new(egui::Context::default(), None);
        state.copies = Some(VirtualCopyStore::new("Copy 1".into(), pipeline));
        state
    }

    #[test]
    fn committing_an_edit_preserves_a_mask_wrapper() {
        let mask = MaskShape::Linear(LinearMask {
            cx: 0.4,
            cy: 0.6,
            angle_deg: 30.0,
            feather: 0.2,
            invert: true,
        });
        let mut state = state_with_op(Box::new(MaskedOp {
            inner: Box::new(SepiaOp::new(0.25)),
            mask: mask.clone(),
        }));
        state.editing = Some(EditSession {
            op_index: 0,
            tool: EditingTool::Sepia,
            was_enabled: true,
        });

        state.commit_edit(Box::new(SepiaOp::new(0.75)));

        let masked = state.pipeline().unwrap().ops()[0]
            .operation
            .as_any()
            .and_then(|any| any.downcast_ref::<MaskedOp>())
            .expect("mask wrapper was discarded");
        assert_eq!(
            serde_json::to_value(&masked.mask).unwrap(),
            serde_json::to_value(&mask).unwrap()
        );
        let sepia = masked
            .inner
            .as_any()
            .and_then(|any| any.downcast_ref::<SepiaOp>())
            .unwrap();
        assert!((sepia.strength - 0.75).abs() < 1e-6);
    }

    #[test]
    fn cancelling_an_edit_restores_a_disabled_operation() {
        let mut state = state_with_op(Box::new(SaturationOp::new(0.5)));
        state.pipeline_mut().unwrap().toggle_op(0);
        state.editing = Some(EditSession {
            op_index: 0,
            tool: EditingTool::Saturation,
            was_enabled: false,
        });

        state.end_edit();

        assert!(!state.pipeline().unwrap().ops()[0].enabled);
    }
}
