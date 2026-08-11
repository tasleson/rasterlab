//! Library background tasks: imports, integrity scrubs, index rebuilds,
//! thumbnail loading/regeneration, and the handlers that fold their progress
//! reports back into [`AppState`].

use std::{
    path::PathBuf as StdPathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};

use image as img_crate;
use rasterlab_core::panic_guard;

use super::{AppMode, AppState, BgMessage, workers};

/// Number of worker threads servicing thumbnail loads. Fixed and small so a
/// large library grid can't spawn thousands of threads at once.
const THUMB_LOADER_THREADS: usize = 4;

/// A queued thumbnail load: read `thumb_path` (falling back to the embedded
/// thumbnail in `rlab_path`) and post the bytes back as `BgMessage::ThumbLoaded`.
pub(super) struct ThumbLoadRequest {
    hash: String,
    thumb_path: StdPathBuf,
    rlab_path: StdPathBuf,
}

impl AppState {
    // -----------------------------------------------------------------------
    // Opening libraries
    // -----------------------------------------------------------------------

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

    // -----------------------------------------------------------------------
    // Import
    // -----------------------------------------------------------------------

    pub fn import_into_library(&mut self, paths: Vec<std::path::PathBuf>) {
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let progress_tx = self.bg_tx.clone();
        let progress_ctx = self.ctx.clone();
        workers::spawn(
            "rasterlab-import",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            BgMessage::ImportFailed,
            move || {
                let result = lib.import_files(&paths, move |p| {
                    let _ = progress_tx.send(BgMessage::ImportProgress(p));
                    progress_ctx.request_repaint();
                });
                match result {
                    Ok(session) => BgMessage::ImportComplete {
                        errors: Vec::new(),
                        session,
                    },
                    Err(e) => BgMessage::ImportFailed(e.to_string()),
                }
            },
        );
    }

    /// Recursively import `folder`, grouping photos into back-dated import
    /// sessions by capture date (see [`rasterlab_library::Library::import_folder`]).
    pub fn import_folder_into_library(&mut self, folder: std::path::PathBuf) {
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let progress_tx = self.bg_tx.clone();
        let progress_ctx = self.ctx.clone();
        workers::spawn(
            "rasterlab-import",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            BgMessage::ImportFailed,
            move || {
                let result = lib.import_folder(&folder, move |p| {
                    let _ = progress_tx.send(BgMessage::ImportProgress(p));
                    progress_ctx.request_repaint();
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
                        BgMessage::ImportComplete {
                            errors,
                            session: summary,
                        }
                    }
                    Err(e) => BgMessage::ImportFailed(e.to_string()),
                }
            },
        );
    }

    pub(super) fn on_import_progress(&mut self, progress: rasterlab_library::ImportProgress) {
        // Mirror the running error list into `last_import_errors` so the
        // "⚠ N import error(s)" button and its detail window work mid-import,
        // not only once the whole run completes.
        self.library.last_import_errors = progress.errors.clone();
        self.library.import_progress = Some(progress);
    }

    pub(super) fn on_import_complete(
        &mut self,
        session: rasterlab_library::ImportSession,
        errors: Vec<(StdPathBuf, String)>,
    ) {
        self.library.import_progress = None;
        self.library.thumbs.clear();
        self.library.refresh();
        if errors.is_empty() {
            self.status = format!(
                "Import complete: {} photos in \"{}\"",
                session.photo_count, session.name
            );
        } else {
            // Dump details to the terminal for quick diagnosis, and keep them in
            // state so the UI can show them on demand.
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

    /// Terminal handler for an import that will never report progress again.
    ///
    /// Tears the progress bar down and refreshes the grid: an import that died
    /// partway through still committed the photos it had already written, and
    /// they should be visible rather than waiting for the next library open.
    pub(super) fn on_import_failed(&mut self, message: String) {
        self.library.import_progress = None;
        self.library.thumbs.clear();
        self.library.refresh();
        self.status = format!("Import failed: {message}");
    }

    // -----------------------------------------------------------------------
    // Integrity scrub
    // -----------------------------------------------------------------------

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

        let progress_tx = self.bg_tx.clone();
        let progress_ctx = self.ctx.clone();
        workers::spawn(
            "rasterlab-scrub",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            BgMessage::ScrubFailed,
            move || {
                let result = lib.scrub(cancel, move |p| {
                    let _ = progress_tx.send(BgMessage::ScrubProgress(p));
                    progress_ctx.request_repaint();
                });
                match result {
                    Ok(outcome) => BgMessage::ScrubComplete { outcome },
                    Err(e) => BgMessage::ScrubFailed(e.to_string()),
                }
            },
        );
    }

    /// Request that a running scrub stop after the current file.
    pub fn stop_scrub(&mut self) {
        if let Some(cancel) = &self.scrub_cancel {
            cancel.store(true, Ordering::Relaxed);
            self.status = "Stopping scrub…".into();
        }
    }

    pub(super) fn on_scrub_progress(&mut self, progress: rasterlab_library::ScrubProgress) {
        // Mirror the running error list so the "⚠ N scrub error(s)" button and
        // its detail window work mid-scrub, not only once the whole run completes.
        self.library.last_scrub_errors = progress.errors.clone();
        self.library.scrub_progress = Some(progress);
    }

    pub(super) fn on_scrub_complete(&mut self, outcome: rasterlab_library::ScrubOutcome) {
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

    /// Terminal handler for a scrub that will never complete.
    ///
    /// Releasing `scrub_cancel` matters beyond the status line: it is what
    /// [`Self::scrub_running`] reports, so leaving it set would pin the File
    /// menu to "Stop scrub" and make every later [`Self::start_scrub`] a no-op.
    pub(super) fn on_scrub_failed(&mut self, message: String) {
        self.scrub_cancel = None;
        self.library.scrub_progress = None;
        self.status = format!("Scrub failed: {message}");
    }

    // -----------------------------------------------------------------------
    // Index rebuild
    // -----------------------------------------------------------------------

    pub fn rebuild_library_index(&mut self) {
        if self.library.rebuild_started.is_some() {
            return;
        }
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let progress_tx = self.bg_tx.clone();
        let progress_ctx = self.ctx.clone();
        self.library.rebuild_started = Some(std::time::Instant::now());
        self.status = "Rebuilding library index…".into();
        workers::spawn(
            "rasterlab-rebuild",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            // A rebuild that dies has no total to report; the fatal message is
            // what clears `rebuild_started` and unblocks a retry.
            |message| BgMessage::RebuildComplete {
                total: 0,
                errors: Vec::new(),
                fatal: Some(message),
            },
            move || {
                // Track the last progress report so the completion message can
                // carry the final total and per-file errors (rebuild_index only
                // exposes them through the callback).
                let last = std::cell::RefCell::new((0usize, Vec::new()));
                let result = lib.rebuild_index(|p| {
                    *last.borrow_mut() = (p.total, p.errors.clone());
                    let _ = progress_tx.send(BgMessage::RebuildProgress(p));
                    progress_ctx.request_repaint();
                });
                let (total, errors) = last.into_inner();
                BgMessage::RebuildComplete {
                    total,
                    errors,
                    fatal: result.err().map(|e| e.to_string()),
                }
            },
        );
    }

    pub(super) fn on_rebuild_progress(&mut self, progress: rasterlab_library::RebuildProgress) {
        self.library.rebuild_progress = Some(progress);
        if let Some(text) = self.library.rebuild_status_text() {
            self.status = text;
        }
    }

    pub(super) fn on_rebuild_complete(
        &mut self,
        total: usize,
        errors: Vec<(StdPathBuf, String)>,
        fatal: Option<String>,
    ) {
        self.library.rebuild_progress = None;
        self.library.rebuild_started = None;
        self.library.thumbs.clear();
        self.library.refresh();
        if let Some(e) = fatal {
            self.status = format!("Rebuild failed: {e}");
        } else if errors.is_empty() {
            self.status = format!("Index rebuild complete: {total} photos");
        } else {
            // Dump details to the terminal for quick diagnosis, the same way
            // import failures are reported.
            for (path, msg) in &errors {
                eprintln!("rebuild error: {}: {msg}", path.display());
            }
            self.status = format!(
                "Index rebuild: {} photos, {} error(s)",
                total.saturating_sub(errors.len()),
                errors.len()
            );
        }
    }

    // -----------------------------------------------------------------------
    // Thumbnails
    // -----------------------------------------------------------------------

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
        workers::spawn(
            "rasterlab-copy-select",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            |message| BgMessage::TaskFailed(format!("set active copy: {message}")),
            move || {
                let rlab_path = lib.rlab_path(&hash);
                let result = (|| -> anyhow::Result<Vec<u8>> {
                    let mut rlab = rasterlab_core::project::RlabFile::read(&rlab_path)?;
                    rlab.active_copy_index = copy_idx.min(rlab.copies.len().saturating_sub(1));
                    rlab.write_v5(&rlab_path)?;
                    lib.regenerate_thumbnail(&hash)?;
                    Ok(std::fs::read(lib.thumb_path(&hash))?)
                })();
                match result {
                    Ok(bytes) => BgMessage::ThumbLoaded { hash, bytes },
                    Err(e) => BgMessage::TaskFailed(format!("set active copy: {e}")),
                }
            },
        );
    }

    /// Rebuild the on-disk thumbnail for `hash` in the background and push the
    /// new bytes back to the grid. No-op when no library is open.
    pub(super) fn spawn_thumbnail_regen(&mut self, hash: String) {
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        workers::spawn(
            "rasterlab-thumb-regen",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            |message| BgMessage::TaskFailed(format!("thumbnail regeneration: {message}")),
            move || {
                if let Err(e) = lib.regenerate_thumbnail(&hash) {
                    return BgMessage::TaskFailed(format!("thumbnail regeneration failed: {e:#}"));
                }
                match std::fs::read(lib.thumb_path(&hash)) {
                    Ok(bytes) => BgMessage::ThumbLoaded { hash, bytes },
                    Err(e) => BgMessage::TaskFailed(format!("thumbnail read failed: {e}")),
                }
            },
        );
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
        // Mark only once the request is actually queued; a hash marked against
        // a pool that never started would never be retried.
        self.ensure_thumb_pool();
        if let Some(tx) = &self.thumb_req_tx
            && tx.send(req).is_ok()
        {
            self.library.thumbs.mark_requested(hash);
        }
    }

    /// Lazily spawn the fixed-size thumbnail-loader pool. Workers pull requests
    /// off a shared queue, read the thumbnail bytes, and post them back as
    /// `BgMessage::ThumbLoaded`. Idempotent; the pool lives for the app's life.
    ///
    /// The sender is only installed once at least one worker is running, so a
    /// grid that cannot get a pool reports it instead of quietly queueing
    /// requests into a channel nobody is reading.
    fn ensure_thumb_pool(&mut self) {
        if self.thumb_req_tx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel::<ThumbLoadRequest>();
        let rx = Arc::new(Mutex::new(rx));
        let mut running = 0usize;
        for _ in 0..THUMB_LOADER_THREADS {
            let rx = Arc::clone(&rx);
            let bg_tx = self.bg_tx.clone();
            let ctx = self.ctx.clone();
            let spawned = std::thread::Builder::new()
                .name("rasterlab-thumb".into())
                .stack_size(1024 * 1024)
                .spawn(move || {
                    loop {
                        // Hold the lock only to dequeue; release before reading.
                        // Recover from poisoning: a worker that panicked mid-load
                        // corrupted nothing here, and taking the queue down with
                        // it would silently stop every remaining worker.
                        let req = {
                            let guard = rx.lock().unwrap_or_else(|e| e.into_inner());
                            guard.recv()
                        };
                        let Ok(req) = req else {
                            break; // sender dropped — app shutting down
                        };
                        // One bad `.rlab` must cost its own thumbnail, not this
                        // worker: a panic that escaped here would shrink the pool
                        // for the rest of the session.
                        let bytes = panic_guard::guard(|| {
                            // Primary source: separate JPEG in thumbs/.
                            // Fallback: thumbnail embedded in the PREV chunk of the .rlab.
                            std::fs::read(&req.thumb_path).ok().or_else(|| {
                                rasterlab_core::project::RlabFile::read(&req.rlab_path)
                                    .ok()
                                    .and_then(|r| r.thumbnail)
                            })
                        });
                        match bytes {
                            Ok(Some(bytes)) => {
                                let _ = bg_tx.send(BgMessage::ThumbLoaded {
                                    hash: req.hash,
                                    bytes,
                                });
                                ctx.request_repaint();
                            }
                            Ok(None) => {}
                            Err(panic) => {
                                eprintln!(
                                    "thumbnail load panicked for {}: {panic}",
                                    req.rlab_path.display()
                                );
                            }
                        }
                    }
                })
                .is_ok();
            running += usize::from(spawned);
        }
        if running == 0 {
            self.status = "Could not start the thumbnail loaders".into();
            return;
        }
        self.thumb_req_tx = Some(tx);
    }

    pub(super) fn on_thumb_loaded(&mut self, hash: String, bytes: Vec<u8>) {
        // Upload JPEG bytes as a texture, downscaled to the size the grid
        // actually draws (cell size in device pixels) so a 512 px on-disk
        // thumbnail doesn't sit in GPU memory at 4× the resolution it's shown
        // at. Never upscales.
        if let Ok(dyn_img) = img_crate::load_from_memory(&bytes) {
            let target = crate::state::library_state::thumb_target_side(
                self.library.thumb_scale,
                self.ctx.pixels_per_point(),
            );
            let dyn_img = if dyn_img.width().max(dyn_img.height()) > target {
                dyn_img.resize(target, target, img_crate::imageops::FilterType::Triangle)
            } else {
                dyn_img
            };
            let rgba = dyn_img.to_rgba8();
            let size = [rgba.width() as usize, rgba.height() as usize];
            let color_image = egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw());
            let handle = self
                .ctx
                .load_texture(&hash, color_image, egui::TextureOptions::LINEAR);
            self.library.thumbs.insert(hash, handle);
        }
        self.ctx.request_repaint();
    }
}
