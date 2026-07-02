use std::{
    path::PathBuf as StdPathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use image as img_crate;

use crate::{
    prefs::Prefs,
    state::{LibraryState, VirtualCopyStore},
};

use egui::Context;
use rasterlab_core::{
    Image, cancel as core_cancel,
    formats::FormatRegistry,
    ops::{
        BlackAndWhiteOp, BrightnessContrastOp, HistogramData, LevelsOp, MaskedOp, NoiseReductionOp,
        NrMethod, ResizeOp, SaturationOp, SharpenOp, VignetteOp,
    },
    pipeline::EditPipeline,
    project::{RlabFile, RlabMeta},
    traits::operation::Operation,
};
use rasterlab_gpu::GpuContext;
use rasterlab_render::{PREVIEW_SCALE, RenderMeta, RenderRequest, RenderResult};

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

#[derive(Clone)]
struct ReusableNrPreview {
    copy_index: usize,
    cursor: usize,
    cache_gen: u64,
    signature: NrPreviewSignature,
    image: Arc<Image>,
}

#[derive(Clone, PartialEq)]
struct NrPreviewSignature {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessingBackend {
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
    /// Non-render error from a background thread (file load, export, etc.).
    Error(String),
    /// Progress update from a running import.
    ImportProgress(rasterlab_library::ImportProgress),
    /// Import finished; thumbnail cache should be invalidated.
    ImportComplete {
        session: rasterlab_library::ImportSession,
        errors: Vec<(StdPathBuf, String)>,
    },
    /// A thumbnail image was loaded from disk; ready to upload to egui.
    ThumbLoaded { hash: String, bytes: Vec<u8> },
    /// Progress update from a running integrity scrub.
    ScrubProgress(rasterlab_library::ScrubProgress),
    /// Scrub finished (completed or cancelled).
    ScrubComplete {
        outcome: rasterlab_library::ScrubOutcome,
    },
}

impl From<RenderResult> for BgMessage {
    fn from(r: RenderResult) -> Self {
        BgMessage::Render(r)
    }
}

/// Number of worker threads servicing thumbnail loads. Fixed and small so a
/// large library grid can't spawn thousands of threads at once.
const THUMB_LOADER_THREADS: usize = 4;

/// A queued thumbnail load: read `thumb_path` (falling back to the embedded
/// thumbnail in `rlab_path`) and post the bytes back as `BgMessage::ThumbLoaded`.
struct ThumbLoadRequest {
    hash: String,
    thumb_path: StdPathBuf,
    rlab_path: StdPathBuf,
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
    /// Sender into the bounded thumbnail-loader pool (lazily spawned on first
    /// request). A fixed worker count prevents a large grid from spawning one
    /// OS thread per visible photo.
    thumb_req_tx: Option<mpsc::Sender<ThumbLoadRequest>>,
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

impl AppState {
    pub fn new(ctx: Context, gpu: Option<Arc<GpuContext>>) -> Self {
        let (bg_tx, bg_rx) = mpsc::channel();
        let prefs = Prefs::load();
        let mut tools = ToolState::new();
        tools.encode_opts.jpeg_quality = prefs.jpeg_quality;
        tools.encode_opts.png_compression = prefs.png_compression;
        tools.encode_opts.preserve_metadata = prefs.preserve_metadata;
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
                    self.reset_tools_for_new_image(w, h);
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

                    self.prefs.push_recent(path, None);
                    self.prefs.save();
                    self.loading = false;
                    self.image_generation += 1;
                    self.request_render();
                }
                BgMessage::ProjectLoaded { path, rlab, image } => {
                    let w = image.width;
                    let h = image.height;
                    self.reset_tools_for_new_image(w, h);
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
                    // Determine the session ID: reuse the one from an autosave
                    // restore (for correct cleanup on save) or mint a fresh one.
                    self.autosave_session_id = Some(
                        self.autosave_restore_session_id
                            .take()
                            .unwrap_or_else(crate::autosave::unix_now),
                    );
                    self.autosave_pending = false;
                    let display_name = rlab.lmta.as_ref().and_then(|l| l.original_filename.clone());
                    let restored_autosave = self.autosave_restore.is_some();
                    let (copies, active_copy) = match self.autosave_restore.take() {
                        Some((saved_copies, saved_active)) => (saved_copies, saved_active),
                        None => (rlab.copies, rlab.active_copy_index),
                    };
                    match VirtualCopyStore::load_from_saved(Arc::new(image), copies, active_copy) {
                        Ok(store) => {
                            self.copies = Some(store);
                            if restored_autosave {
                                self.mark_dirty();
                            }
                        }
                        Err(e) => {
                            self.status = format!("Warning: could not restore edit stack: {}", e);
                        }
                    }
                    self.prefs.push_recent(path, display_name);
                    self.prefs.save();
                    self.loading = false;
                    self.image_generation += 1;
                    self.request_render();
                }
                BgMessage::Render(result) => match result {
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
                            if let Some((copy_index, cursor, key_cache_gen, signature)) =
                                reusable_nr_key
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
                        self.status = format!("Error: {}", e);
                        self.loading = false;
                        self.nr_in_flight = false;
                        self.render_start = None;
                        self.render_backend = None;
                        self.pending_nr_preview_key = None;
                    }
                    RenderResult::Cancelled => {
                        self.loading = false;
                        self.nr_in_flight = false;
                        self.pending_nr_preview_key = None;
                        self.render_start = None;
                        self.render_backend = None;
                        self.status = "Cancelled".into();
                        if self.needs_rerender {
                            self.needs_rerender = false;
                            self.request_render_inner(false);
                        }
                    }
                },
                BgMessage::Error(e) => {
                    self.status = format!("Error: {}", e);
                    self.loading = false;
                }
                BgMessage::ImportProgress(p) => {
                    // Mirror the running error list into `last_import_errors` so the
                    // "⚠ N import error(s)" button and its detail window work mid-import,
                    // not only once the whole run completes.
                    self.library.last_import_errors = p.errors.clone();
                    self.library.import_progress = Some(p);
                }
                BgMessage::ImportComplete { session, errors } => {
                    self.library.import_progress = None;
                    self.library.thumbs.clear();
                    self.library.refresh();
                    if errors.is_empty() {
                        self.status = format!(
                            "Import complete: {} photos in \"{}\"",
                            session.photo_count, session.name
                        );
                    } else {
                        // Dump details to the terminal for quick diagnosis, and
                        // keep them in state so the UI can show them on demand.
                        for (path, msg) in &errors {
                            eprintln!("import error: {}: {msg}", path.display());
                        }
                        self.status = format!(
                            "Import: {} photos, {} error(s)",
                            session.photo_count,
                            errors.len()
                        );
                    }
                    self.library.last_import_errors = errors;
                }
                BgMessage::ThumbLoaded { hash, bytes } => {
                    // Upload JPEG bytes as a texture, downscaled to the size the
                    // grid actually draws (cell size in device pixels) so a
                    // 512 px on-disk thumbnail doesn't sit in GPU memory at 4×
                    // the resolution it's shown at. Never upscales.
                    if let Ok(dyn_img) = img_crate::load_from_memory(&bytes) {
                        let target = crate::state::library_state::thumb_target_side(
                            self.library.thumb_scale,
                            self.ctx.pixels_per_point(),
                        );
                        let dyn_img = if dyn_img.width().max(dyn_img.height()) > target {
                            dyn_img.resize(
                                target,
                                target,
                                img_crate::imageops::FilterType::Triangle,
                            )
                        } else {
                            dyn_img
                        };
                        let rgba = dyn_img.to_rgba8();
                        let size = [rgba.width() as usize, rgba.height() as usize];
                        let color_image =
                            egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
                        let handle =
                            self.ctx
                                .load_texture(&hash, color_image, egui::TextureOptions::LINEAR);
                        self.library.thumbs.insert(hash, handle);
                    }
                    self.ctx.request_repaint();
                }
                BgMessage::ScrubProgress(p) => {
                    // Mirror the running error list so the "⚠ N scrub error(s)"
                    // button and its detail window work mid-scrub, not only once
                    // the whole run completes.
                    self.library.last_scrub_errors = p.errors.clone();
                    self.library.scrub_progress = Some(p);
                }
                BgMessage::ScrubComplete { outcome } => {
                    self.scrub_cancel = None;
                    self.library.scrub_progress = None;
                    let verb = if outcome.cancelled {
                        "Scrub stopped"
                    } else {
                        "Scrub complete"
                    };
                    self.status = format!(
                        "{verb}: {} checked, {} repaired, {} upgraded, {} error(s)",
                        outcome.checked,
                        outcome.repaired,
                        outcome.upgraded,
                        outcome.errors.len()
                    );
                    self.library.last_scrub_errors = outcome.errors;
                }
            }
        }
        self.update_processing_status();
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
        self.mode = AppMode::Editor;

        // Clear the canvas so the previous image doesn't flash while the new
        // one is still decoding/rendering in the background.
        self.rendered = None;
        self.preview_overlay = None;
        self.preview_overlay_rect = None;
        self.histogram = None;

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
                // Decoders are third-party code (e.g. rawler) and not guaranteed
                // panic-free on malformed input. A panic here would unwind the
                // thread without ever sending a message, leaving the UI stuck on
                // "Loading…" forever. Contain it so a bad file becomes an error.
                let path_label = path.display().to_string();
                let computed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if is_project {
                        match RlabFile::read(&path) {
                            Ok(rlab) => {
                                let registry = FormatRegistry::with_builtins();
                                let hint =
                                    rlab.meta.source_path.as_deref().map(std::path::Path::new);
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
                    }
                }));
                let msg = computed.unwrap_or_else(|_| {
                    BgMessage::Error(format!(
                        "Failed to load {path_label}: decoder panicked \
                         (corrupt file or unsupported camera variant)"
                    ))
                });
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
        // If we're overwriting an existing library `.rlab`, read its current
        // LMTA chunk so we can carry it forward. Otherwise the save drops the
        // library metadata (keywords, rating, source-file timestamps, …) and
        // features that depend on it — like "Export Selection → Original" —
        // lose ground truth.
        let existing_lmta = if path.exists() {
            RlabFile::read(&path).ok().and_then(|r| r.lmta)
        } else {
            None
        };
        let mut rlab = RlabFile::new(meta, original_bytes, copies_saved, active_idx, None);
        rlab.set_lmta(existing_lmta);
        // v4 adds Reed-Solomon parity so the file is repairable by an integrity
        // scrub; this also avoids downgrading a library photo that was imported
        // as v4 when its edits are saved back in place.
        match rlab.write_v4(&path) {
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

                // If this file was opened from the library, regenerate its thumbnail.
                if let Some((_, hash)) = &self.library_context
                    && let Some(lib) = self.library.library.clone()
                {
                    let hash = hash.clone();
                    let tx = self.bg_tx.clone();
                    let ctx = self.ctx.clone();
                    std::thread::Builder::new()
                        .name("rasterlab-thumb-regen".into())
                        .stack_size(32 * 1024 * 1024)
                        .spawn(move || {
                            if let Err(e) = lib.regenerate_thumbnail(&hash) {
                                eprintln!("thumb-regen failed: {:#}", e);
                                return;
                            }
                            let thumb_path = lib.thumb_path(&hash);
                            if let Ok(bytes) = std::fs::read(&thumb_path) {
                                let _ = tx.send(BgMessage::ThumbLoaded { hash, bytes });
                                ctx.request_repaint();
                            }
                        })
                        .ok();
                }
            }
            Err(e) => {
                self.status = format!("Save failed: {}", e);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Pipeline mutations (always followed by request_render)
    // -----------------------------------------------------------------------

    /// True while a render that includes a noise-reduction op is running.
    /// Used by the tools panel to decide whether to show the NR Cancel button.
    pub fn nr_in_flight(&self) -> bool {
        self.nr_in_flight && self.loading
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
    pub(crate) fn mark_dirty(&mut self) {
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
    /// Stores the pipeline data from `entry` and opens the project file when
    /// available, falling back to the original source image. When loading
    /// finishes, the autosaved pipeline state is applied automatically.
    pub fn restore_autosave(&mut self, entry: crate::autosave::AutosaveEntry) {
        let restore_path = entry
            .data
            .project_path
            .as_deref()
            .map(std::path::PathBuf::from)
            .filter(|p| p.exists())
            .unwrap_or_else(|| std::path::PathBuf::from(&entry.data.source_path));
        self.autosave_restore = Some((entry.data.copies, entry.data.active_copy));
        self.autosave_restore_session_id = Some(entry.data.started_at);
        self.open_file(restore_path);
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
        for t in &mut self.tools.tools {
            if t.editing_tool() == Some(tool) {
                t.activate_preview();
                break;
            }
        }
    }

    pub(crate) fn push_op(&mut self, op: Box<dyn Operation>) {
        // When an edit session is active, the tool's Apply button replaces the
        // op under edit instead of pushing a new one.  The mask wrapper is
        // skipped in this path — editing preserves the structure of the
        // existing entry (including its own mask wrapper, if any, which we
        // leave untouched by writing the new inner op type unwrapped).
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

    fn try_push_reusable_nr_preview(&mut self, op: &dyn Operation) -> bool {
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

    /// Analyse the current image and push whatever corrections it needs.
    ///
    /// Unlike [`Self::push_auto_enhance`], which applies fixed preset values,
    /// this measures the rendered image (colour cast, tone, chroma,
    /// sharpness) and derives per-image parameter values in closed form.
    /// Each correction lands as its own op in the edit stack so it can be
    /// inspected, tweaked, or undone individually.
    pub fn push_smart_enhance(&mut self) {
        let Some(rendered) = self.rendered.clone() else {
            return;
        };
        if self.copies.is_none() {
            return;
        }

        let plan = rasterlab_core::analysis::plan_enhancement(&rendered);
        if plan.is_empty() {
            self.status = "Smart Enhance: no corrections needed".into();
            return;
        }
        self.status = format!("Smart Enhance: {}", plan.summary());

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
    fn reset_tools_for_new_image(&mut self, w: u32, h: u32) {
        use crate::panels::tools::crop::CropTool;
        use crate::panels::tools::resize::ResizeTool;
        use crate::panels::tools::rotate::RotateTool;

        if let Some(crop) = self.tools.find_mut::<CropTool>() {
            crop.x = 0;
            crop.y = 0;
            crop.w = w;
            crop.h = h;
        }
        if let Some(resize) = self.tools.find_mut::<ResizeTool>() {
            resize.w = w;
            resize.h = h;
        }
        if let Some(rotate) = self.tools.find_mut::<RotateTool>() {
            rotate.deg = 0.0;
            rotate.preview_active = false;
        }
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
        let tool = self.tools.find_mut::<PanoramaTool>().unwrap();
        tool.paths.push(path.to_string_lossy().into_owned());
        let needs_render = tool.paths.len() >= 2;
        if needs_render {
            tool.preview_active = true;
        }
        if needs_render {
            self.request_render();
        }
    }

    pub fn focus_stack_add_image(&mut self, path: std::path::PathBuf) {
        use crate::panels::tools::focus_stack::FocusStackTool;
        let tool = self.tools.find_mut::<FocusStackTool>().unwrap();
        tool.paths.push(path.to_string_lossy().into_owned());
        let needs_render = tool.paths.len() >= 2;
        if needs_render {
            tool.preview_active = true;
        }
        if needs_render {
            self.request_render();
        }
    }

    pub fn hdr_merge_add_image(&mut self, path: std::path::PathBuf) {
        use crate::panels::tools::hdr_merge::HdrMergeTool;
        let tool = self.tools.find_mut::<HdrMergeTool>().unwrap();
        tool.paths.push(path.to_string_lossy().into_owned());
        let needs_render = tool.paths.len() >= 2;
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

    fn update_processing_status(&mut self) {
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
    fn request_render_inner(&mut self, force_full_res: bool) {
        if self.copies.is_none() {
            return;
        }
        if self.loading {
            // Another render is in-flight; mark dirty so we re-render after it.
            self.needs_rerender = true;
            return;
        }

        // Preview op — applied on top of committed result but NOT cached.
        let preview_op: Option<Box<dyn Operation>> = self.tools.preview_op();
        let reusable_nr_signature = preview_op
            .as_deref()
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
        rasterlab_render::spawn_render(request, meta, tx, repaint);
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

    // ── Library ────────────────────────────────────────────────────────────

    pub fn new_library(&mut self, path: std::path::PathBuf) {
        if let Err(e) = std::fs::create_dir_all(&path) {
            self.library.last_error = Some(format!("Failed to create directory: {e}"));
            return;
        }
        self.open_library(path);
    }

    pub fn open_library(&mut self, path: std::path::PathBuf) {
        let scale = self.prefs.library_thumb_scale;
        self.library.open_library(path.clone(), scale);
        self.prefs.push_recent_library(path.clone());
        self.prefs.last_library = Some(path);
        self.prefs.save();
        self.mode = AppMode::Library;
    }

    pub fn import_into_library(&mut self, paths: Vec<std::path::PathBuf>) {
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("rasterlab-import".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let progress_tx = tx.clone();
                let result = lib.import_files(&paths, move |p| {
                    let _ = progress_tx.send(BgMessage::ImportProgress(p));
                    ctx.request_repaint();
                });
                match result {
                    Ok(session) => {
                        let _ = tx.send(BgMessage::ImportComplete {
                            errors: Vec::new(),
                            session,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(BgMessage::Error(e.to_string()));
                    }
                }
            })
            .ok();
    }

    /// Recursively import `folder`, grouping photos into back-dated import
    /// sessions by capture date (see [`rasterlab_library::Library::import_folder`]).
    pub fn import_folder_into_library(&mut self, folder: std::path::PathBuf) {
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("rasterlab-import".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let progress_tx = tx.clone();
                let result = lib.import_folder(&folder, move |p| {
                    let _ = progress_tx.send(BgMessage::ImportProgress(p));
                    ctx.request_repaint();
                });
                match result {
                    Ok(sessions) => {
                        let total: usize = sessions.iter().map(|s| s.photo_count).sum();
                        let errors: Vec<_> =
                            sessions.iter().flat_map(|s| s.errors.clone()).collect();
                        // Synthesise a summary "session" so the existing
                        // ImportComplete status line can report the whole run.
                        let summary = rasterlab_library::ImportSession {
                            id: String::new(),
                            name: format!("{} group(s)", sessions.len()),
                            started_at: 0,
                            photo_count: total,
                            errors: Vec::new(),
                        };
                        let _ = tx.send(BgMessage::ImportComplete {
                            errors,
                            session: summary,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(BgMessage::Error(e.to_string()));
                    }
                }
            })
            .ok();
    }

    /// True while a background integrity scrub is running.
    pub fn scrub_running(&self) -> bool {
        self.scrub_cancel.is_some()
    }

    /// Spawn a background scrub over every `.rlab` file in the open library.
    /// No-op if a scrub is already running or no library is open.
    pub fn start_scrub(&mut self) {
        if self.scrub_cancel.is_some() {
            return;
        }
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        self.scrub_cancel = Some(cancel.clone());
        self.library.scrub_progress = Some(rasterlab_library::ScrubProgress::default());
        self.library.last_scrub_errors.clear();
        self.status = "Scrubbing library…".into();

        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("rasterlab-scrub".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let progress_tx = tx.clone();
                let progress_ctx = ctx.clone();
                let result = lib.scrub(cancel, move |p| {
                    let _ = progress_tx.send(BgMessage::ScrubProgress(p));
                    progress_ctx.request_repaint();
                });
                match result {
                    Ok(outcome) => {
                        let _ = tx.send(BgMessage::ScrubComplete { outcome });
                    }
                    Err(e) => {
                        let _ = tx.send(BgMessage::Error(format!("Scrub failed: {e}")));
                    }
                }
                ctx.request_repaint();
            })
            .ok();
    }

    /// Request that a running scrub stop after the current file.
    pub fn stop_scrub(&mut self) {
        if let Some(cancel) = &self.scrub_cancel {
            cancel.store(true, Ordering::Relaxed);
            self.status = "Stopping scrub…".into();
        }
    }

    pub fn rebuild_library_index(&mut self) {
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();
        self.status = "Rebuilding library index…".into();
        std::thread::Builder::new()
            .name("rasterlab-rebuild".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let result = lib.rebuild_index(|_p| {});
                match result {
                    Ok(()) => {
                        let _ = tx.send(BgMessage::ImportComplete {
                            session: rasterlab_library::ImportSession {
                                id: String::new(),
                                name: "Index rebuild".into(),
                                started_at: 0,
                                photo_count: 0,
                                errors: Vec::new(),
                            },
                            errors: Vec::new(),
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(BgMessage::Error(format!("Rebuild failed: {e}")));
                    }
                }
                ctx.request_repaint();
            })
            .ok();
    }

    /// Change the active virtual copy for a library photo and regenerate its
    /// thumbnail in the background.  The new thumbnail is sent back via
    /// `BgMessage::ThumbLoaded` so the grid updates without reopening the editor.
    pub fn set_active_copy(&mut self, hash: &str, copy_idx: usize) {
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        // Evict the stale thumbnail immediately so the grid shows a placeholder
        // while regen is running.
        self.library.thumbs.remove(hash);

        let hash = hash.to_owned();
        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();
        std::thread::Builder::new()
            .name("rasterlab-copy-select".into())
            .stack_size(32 * 1024 * 1024)
            .spawn(move || {
                let rlab_path = lib.rlab_path(&hash);
                let result = (|| -> anyhow::Result<Vec<u8>> {
                    let mut rlab = rasterlab_core::project::RlabFile::read(&rlab_path)?;
                    rlab.active_copy_index = copy_idx.min(rlab.copies.len().saturating_sub(1));
                    rlab.write_v4(&rlab_path)?;
                    lib.regenerate_thumbnail(&hash)?;
                    Ok(std::fs::read(lib.thumb_path(&hash))?)
                })();
                match result {
                    Ok(bytes) => {
                        let _ = tx.send(BgMessage::ThumbLoaded { hash, bytes });
                    }
                    Err(e) => {
                        let _ = tx.send(BgMessage::Error(format!("set active copy: {e}")));
                    }
                }
                ctx.request_repaint();
            })
            .ok();
    }

    /// Request that the thumbnail for `hash` be loaded from disk in the background.
    ///
    /// Loads are serviced by a fixed pool of worker threads (see
    /// [`Self::ensure_thumb_pool`]); the grid may request many thumbnails per
    /// frame, but the pool bounds how many run at once.
    pub fn request_thumb_load(&mut self, hash: String) {
        if self.library.thumbs.is_requested(&hash) {
            return;
        }
        let Some(lib) = &self.library.library else {
            return;
        };
        let req = ThumbLoadRequest {
            thumb_path: lib.thumb_path(&hash),
            rlab_path: lib.rlab_path(&hash),
            hash: hash.clone(),
        };
        self.library.thumbs.mark_requested(hash);
        self.ensure_thumb_pool();
        if let Some(tx) = &self.thumb_req_tx {
            let _ = tx.send(req);
        }
    }

    /// Lazily spawn the fixed-size thumbnail-loader pool. Workers pull requests
    /// off a shared queue, read the thumbnail bytes, and post them back as
    /// `BgMessage::ThumbLoaded`. Idempotent; the pool lives for the app's life.
    fn ensure_thumb_pool(&mut self) {
        if self.thumb_req_tx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel::<ThumbLoadRequest>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..THUMB_LOADER_THREADS {
            let rx = Arc::clone(&rx);
            let bg_tx = self.bg_tx.clone();
            let ctx = self.ctx.clone();
            std::thread::Builder::new()
                .name("rasterlab-thumb".into())
                .stack_size(1024 * 1024)
                .spawn(move || {
                    loop {
                        // Hold the lock only to dequeue; release before reading.
                        let req = {
                            let guard = rx.lock().unwrap();
                            guard.recv()
                        };
                        let Ok(req) = req else {
                            break; // sender dropped — app shutting down
                        };
                        // Primary source: separate JPEG in thumbs/.
                        // Fallback: thumbnail embedded in the PREV chunk of the .rlab.
                        let bytes = std::fs::read(&req.thumb_path).ok().or_else(|| {
                            rasterlab_core::project::RlabFile::read(&req.rlab_path)
                                .ok()
                                .and_then(|r| r.thumbnail)
                        });
                        if let Some(bytes) = bytes {
                            let _ = bg_tx.send(BgMessage::ThumbLoaded {
                                hash: req.hash,
                                bytes,
                            });
                            ctx.request_repaint();
                        }
                    }
                })
                .ok();
        }
        self.thumb_req_tx = Some(tx);
    }
}
