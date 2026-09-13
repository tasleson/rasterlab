//! Library background tasks: imports, bulk delete operations (Recently
//! Deleted and collections), integrity scrubs, index rebuilds, thumbnail loading/regeneration, and the
//! handlers that fold their progress reports back into [`AppState`].

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
use rasterlab_library::CollectionId;

use super::{AppMode, AppState, BgMessage, workers};
use crate::state::library_state::{DeleteKind, DeleteTask, ImportKind, plural};

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
        if let Err(e) = rasterlab_core::verified_write::create_dir_all_synced(&path) {
            self.library.last_error = Some(format!("Failed to create directory: {e}"));
            return;
        }
        self.flush_library_metadata_drafts();
        let scale = self.prefs.library_thumb_scale;
        self.library.create_library(path.clone(), scale);
        self.remember_open_library(path);
    }

    pub fn open_library(&mut self, path: std::path::PathBuf) {
        self.flush_library_metadata_drafts();
        let scale = self.prefs.library_thumb_scale;
        self.library.open_library(path.clone(), scale);
        self.remember_open_library(path);
    }

    /// Record the library the user just opened, and show it.
    ///
    /// Runs whether or not the open succeeded: a library on a disconnected
    /// drive is still the one the user means to come back to, and the Library
    /// mode is where the error banner explaining the failure lives.
    fn remember_open_library(&mut self, path: std::path::PathBuf) {
        self.prefs.push_recent_library(path.clone());
        self.prefs.last_library = Some(path);
        self.prefs.save();
        self.mode = AppMode::Library;
    }

    /// Keep the selection cache aligned and enqueue at most one seek-only
    /// metadata read for each newly selected photo.
    pub(crate) fn sync_library_detail(&mut self, selection: Option<(i64, &str)>) {
        let changed = match (self.library.selected_detail.as_ref(), selection) {
            (None, None) => false,
            (Some(detail), Some((id, hash))) => detail.id != id || detail.hash != hash,
            _ => true,
        };
        if changed {
            self.commit_library_detail_metadata(true);
        }
        let Some(request) = self.library.begin_selected_detail(selection) else {
            return;
        };
        let failure_id = request.id;
        let failure_hash = request.hash.clone();
        let failure_revision = request.request_revision;
        let id = request.id;
        let hash = request.hash;
        let path = request.path;
        let request_revision = request.request_revision;
        workers::spawn(
            "rasterlab-detail-load",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            move |error| BgMessage::LibraryDetailLoaded {
                id: failure_id,
                hash: failure_hash.clone(),
                request_revision: failure_revision,
                result: Box::new(Err(error)),
            },
            move || BgMessage::LibraryDetailLoaded {
                id,
                hash,
                request_revision,
                result: Box::new(
                    rasterlab_core::project::read_library_summary(&path)
                        .map_err(|error| error.to_string()),
                ),
            },
        );
    }

    /// Write every unsaved metadata draft now, on this thread.
    ///
    /// The debounced path hands drafts to a background worker, which is right
    /// while the app keeps running and useless when it is about to stop: the
    /// worker outlives nothing. Callers are the points past which no further
    /// commit can happen — the app exiting, or the library closing under the
    /// drafts. Blocking here is the point; a share that has gone away costs a
    /// pause on exit rather than the user's last edit.
    pub(crate) fn flush_library_metadata_drafts(&mut self) {
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let pending = self.library.drain_metadata_drafts();
        if pending.is_empty() {
            return;
        }
        if let Err(error) = lib.update_metadata_batch(&pending) {
            self.library.last_error = Some(format!("Metadata update failed: {error}"));
        }
    }

    pub(crate) fn commit_library_detail_metadata(&mut self, force: bool) {
        let Some(request) = self.library.prepare_selected_detail_commit(force) else {
            return;
        };
        self.spawn_library_metadata_commit(request);
    }

    pub(super) fn commit_library_metadata_for(&mut self, id: i64, force: bool) {
        let Some(request) = self.library.prepare_detail_commit(id, force) else {
            return;
        };
        self.spawn_library_metadata_commit(request);
    }

    fn spawn_library_metadata_commit(
        &mut self,
        request: crate::state::library_state::MetadataCommitRequest,
    ) {
        let failure_id = request.id;
        let failure_revision = request.revision;
        let id = request.id;
        let revision = request.revision;
        let lmta = request.lmta;
        let library = request.library;
        workers::spawn(
            "rasterlab-metadata-save",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            move |error| BgMessage::LibraryMetadataSaved {
                id: failure_id,
                revision: failure_revision,
                result: Err(error),
            },
            move || BgMessage::LibraryMetadataSaved {
                id,
                revision,
                result: library
                    .update_metadata(id, lmta)
                    .map_err(|error| error.to_string()),
            },
        );
    }

    // -----------------------------------------------------------------------
    // Import
    // -----------------------------------------------------------------------

    pub fn import_into_library(&mut self, paths: Vec<std::path::PathBuf>) {
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let job = self.start_import_job(plural(paths.len(), "file"), ImportKind::Files);
        let progress_tx = self.bg_tx.clone();
        let progress_ctx = self.ctx.clone();
        workers::spawn(
            "rasterlab-import",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            move |message| BgMessage::ImportFailed { job, message },
            move || {
                let result = lib.import_files(&paths, move |progress| {
                    let _ = progress_tx.send(BgMessage::ImportProgress { job, progress });
                    progress_ctx.request_repaint();
                });
                match result {
                    Ok(session) => {
                        let errors = session.errors.clone();
                        BgMessage::ImportComplete {
                            job,
                            errors,
                            session,
                        }
                    }
                    Err(e) => BgMessage::ImportFailed {
                        job,
                        message: e.to_string(),
                    },
                }
            },
        );
    }

    /// Register a new import with the library state and hand back the id that
    /// tells its messages apart from those of the imports already running.
    fn start_import_job(&mut self, label: String, kind: ImportKind) -> u64 {
        let id = self.next_import_id;
        self.next_import_id += 1;
        self.library.start_import_job(id, label, kind);
        id
    }

    /// Ask what collection a folder import should file its photos into, rather
    /// than starting the import straight away.
    ///
    /// The folder picker — native or built-in — has nowhere to put the
    /// question, so it is asked in a dialog of its own once the folder is
    /// known.  That also lets the dialog seed the collection name with the
    /// folder's, which is what the user almost always wants.
    pub fn prompt_folder_import(&mut self, folder: std::path::PathBuf) {
        if self.library.library.is_none() {
            return;
        }
        self.library.folder_import_prompt =
            Some(crate::state::library_state::FolderImportPrompt::new(
                folder,
                self.prefs.import_collection,
            ));
    }

    /// Recursively import `folder`, grouping photos into back-dated import
    /// sessions by capture date (see [`rasterlab_library::Library::import_folder`]),
    /// filing what it imports as `collection` says.
    pub fn import_folder_into_library(
        &mut self,
        folder: std::path::PathBuf,
        collection: rasterlab_library::ImportCollection,
    ) {
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let label = folder.file_name().map_or_else(
            || folder.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let job = self.start_import_job(label, ImportKind::Folder);
        let progress_tx = self.bg_tx.clone();
        let progress_ctx = self.ctx.clone();
        workers::spawn(
            "rasterlab-import",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            move |message| BgMessage::ImportFailed { job, message },
            move || {
                let result =
                    lib.import_folder_into_collection(&folder, collection, move |progress| {
                        let _ = progress_tx.send(BgMessage::ImportProgress { job, progress });
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
                            job,
                            errors,
                            session: summary,
                        }
                    }
                    Err(e) => BgMessage::ImportFailed {
                        job,
                        message: e.to_string(),
                    },
                }
            },
        );
    }

    pub(super) fn on_import_progress(
        &mut self,
        job: u64,
        progress: rasterlab_library::ImportProgress,
    ) {
        // A report can outlive the job it belongs to, since its completion
        // message travels the same channel; applying it would put a finished
        // import back on the status line.
        if !self.library.apply_import_progress(job, progress) {
            return;
        }
        // Follow the import along in the sidebar and the grid rather than
        // leaving both frozen until it ends. Throttled, and a no-op unless
        // photos have actually landed since the last one.
        self.library.refresh_during_import();
    }

    pub(super) fn on_import_complete(
        &mut self,
        job: u64,
        session: rasterlab_library::ImportSession,
        errors: Vec<(StdPathBuf, String)>,
    ) {
        // Dump details to the terminal for quick diagnosis, and keep them in
        // state (merged across the batch) so the UI can show them on demand.
        for (path, message) in &errors {
            eprintln!("import error: {}: {message}", path.display());
        }
        let Some(batch) = self
            .library
            .finish_import_job(job, errors, session.photo_count)
        else {
            return;
        };
        if batch.remaining > 0 {
            // Other imports are still running: their progress line stays, and
            // the final tally waits for the last of them.
            self.library.refresh();
            return;
        }
        self.library.thumbs.clear();
        // Always reveal a successful individual-file import. Otherwise a
        // photo added while viewing a collection or an older session is in the
        // database but appears to have vanished because the old scope remains
        // active. Only when it was the batch's only import, though: with
        // several, whichever finished last is an arbitrary place to land.
        if batch.was_alone() && session.photo_count > 0 && !session.id.is_empty() {
            self.library.view =
                crate::state::library_state::LibraryView::Session(session.id.clone());
        }
        self.library.refresh();
        let failures = self.library.last_import_errors.len();
        self.status = if failures > 0 {
            format!("Import: {} photos, {failures} error(s)", batch.photos)
        } else if batch.jobs > 1 {
            format!(
                "Import complete: {} photos from {} imports",
                batch.photos, batch.jobs
            )
        } else {
            format!(
                "Import complete: {} photos in \"{}\"",
                batch.photos, session.name
            )
        };
    }

    /// Terminal handler for an import that will never report progress again.
    ///
    /// Takes down only the job that died: any other import sharing the batch
    /// is still running and still owns its share of the status line. Refreshes
    /// like a completed one, because an import that died partway through still
    /// committed the photos it had already written, and they should be visible
    /// rather than waiting for the next library open.
    pub(super) fn on_import_failed(&mut self, job: u64, message: String) {
        let Some(batch) = self.library.fail_import_job(job) else {
            return;
        };
        if batch.remaining == 0 {
            self.library.thumbs.clear();
        }
        self.library.refresh();
        // Name the survivors: with a batch running, "Import failed" on its own
        // reads as though everything stopped, when the rest carries on.
        self.status = match batch.remaining {
            0 => format!("Import failed: {message}"),
            remaining => format!(
                "Import failed: {message} ({} still running)",
                plural(remaining, "import")
            ),
        };
    }

    // -----------------------------------------------------------------------
    // Recently Deleted
    // -----------------------------------------------------------------------

    /// True while a bulk Recently Deleted operation is running.
    pub fn delete_running(&self) -> bool {
        self.delete_cancel.is_some()
    }

    /// Move the selection into the library-owned Recently Deleted area.
    pub fn move_selected_to_recently_deleted(&mut self) {
        self.start_delete_task(DeleteKind::ToRecentlyDeleted, Vec::new());
    }

    /// Move the selection back out of Recently Deleted.
    pub fn restore_selected(&mut self) {
        self.start_delete_task(DeleteKind::Restore, Vec::new());
    }

    /// Erase the selected Recently Deleted photos for good.
    pub fn permanently_delete_selected(&mut self) {
        self.start_delete_task(DeleteKind::Permanent, Vec::new());
    }

    /// Erase everything in Recently Deleted for good.
    pub fn empty_recently_deleted(&mut self) {
        self.start_delete_task(DeleteKind::Empty, Vec::new());
    }

    /// Delete collections, leaving the photos that were in them alone.
    ///
    /// The ids come from the confirmation the user answered rather than from
    /// the marks as they stand now: the sidebar is still live behind the
    /// dialog, and the batch that goes must be the batch that was listed.
    pub fn delete_collections(&mut self, ids: Vec<CollectionId>) {
        self.start_delete_task(DeleteKind::Collections, ids);
    }

    /// Request that a running bulk operation stop after the current photo.
    pub fn stop_delete(&mut self) {
        if let Some(cancel) = &self.delete_cancel {
            cancel.store(true, Ordering::Relaxed);
            if let Some(task) = &mut self.library.delete_task {
                task.stopping = true;
            }
        }
    }

    /// Spawn the worker for one bulk delete operation.
    ///
    /// What the operation acts on is taken as the task starts rather than when
    /// it finishes: those photos or collections are on their way out, and
    /// leaving them marked invites a second run at them while the first is
    /// still going.
    fn start_delete_task(&mut self, kind: DeleteKind, collections: Vec<CollectionId>) {
        if self.delete_running() {
            return;
        }
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let mut photos = Vec::new();
        match kind {
            DeleteKind::Empty => {}
            DeleteKind::Collections => self.library.leave_collections(&collections),
            _ => photos = std::mem::take(&mut self.library.selected),
        }
        // Empty takes no list of its own, so seed its bar from the sidebar
        // count until the worker's first report replaces it with the real
        // total.
        let total = match kind {
            DeleteKind::Empty => self.library.recently_deleted_count,
            DeleteKind::Collections => collections.len(),
            _ => photos.len(),
        };
        if kind != DeleteKind::Empty && total == 0 {
            return;
        }

        let cancel = Arc::new(AtomicBool::new(false));
        self.delete_cancel = Some(cancel.clone());
        self.library.delete_task = Some(DeleteTask {
            kind,
            progress: rasterlab_library::BulkProgress {
                total,
                ..Default::default()
            },
            stopping: false,
        });
        self.library.last_delete_errors.clear();
        self.status = format!("{}…", kind.progress_verb());

        let progress_tx = self.bg_tx.clone();
        let progress_ctx = self.ctx.clone();
        workers::spawn(
            "rasterlab-library-delete",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            BgMessage::DeleteFailed,
            move || {
                let report = move |p: rasterlab_library::BulkProgress| {
                    let _ = progress_tx.send(BgMessage::DeleteProgress(p));
                    progress_ctx.request_repaint();
                };
                let result = match kind {
                    DeleteKind::ToRecentlyDeleted => {
                        lib.move_to_recently_deleted(&photos, cancel, report)
                    }
                    DeleteKind::Restore => lib.restore_photos(&photos, cancel, report),
                    DeleteKind::Permanent => {
                        lib.purge_recently_deleted(Some(&photos), cancel, report)
                    }
                    DeleteKind::Empty => lib.purge_recently_deleted(None, cancel, report),
                    DeleteKind::Collections => lib.delete_collections(&collections, cancel, report),
                };
                match result {
                    Ok(outcome) => BgMessage::DeleteComplete { outcome },
                    Err(e) => BgMessage::DeleteFailed(e.to_string()),
                }
            },
        );
    }

    pub(super) fn on_delete_progress(&mut self, progress: rasterlab_library::BulkProgress) {
        // Mirror the running error list so the "⚠ N delete error(s)" button and
        // its detail window work mid-run, not only once the whole run completes.
        self.library.last_delete_errors = progress.errors.clone();
        if let Some(task) = &mut self.library.delete_task {
            task.progress = progress;
        }
    }

    pub(super) fn on_delete_complete(&mut self, outcome: rasterlab_library::BulkOutcome) {
        let kind = self.finish_delete_task();
        // Permanently erased photos will never be shown again, so their
        // thumbnails are dead weight in the texture cache.
        for hash in &outcome.purged {
            self.library.thumbs.remove(hash);
        }
        self.library.refresh();

        self.status = if outcome.cancelled {
            format!(
                "{} stopped after {}",
                kind.progress_verb(),
                kind.item_count(outcome.done)
            )
        } else {
            format!("{} {}", kind.past_verb(), kind.item_count(outcome.done))
        };
        if !outcome.errors.is_empty() {
            self.status
                .push_str(&format!(", {} error(s)", outcome.errors.len()));
        }
        // Protected photos are a partly-unfollowed instruction, not a failure,
        // so they get the banner rather than being lost in the status line.
        if !outcome.protected.is_empty() {
            self.library.last_error = Some(protected_message(&outcome.protected));
        }
        self.library.last_delete_errors = outcome.errors;
    }

    /// Terminal handler for a bulk operation that will never report again.
    ///
    /// Refreshes like a completed one: a run that died partway through still
    /// moved the photos it had already got to, and they should not sit on
    /// screen as though they were still where they were.
    pub(super) fn on_delete_failed(&mut self, message: String) {
        let kind = self.finish_delete_task();
        self.library.refresh();
        let text = format!("{} failed: {message}", kind.progress_verb());
        self.status = text.clone();
        self.library.last_error = Some(text);
    }

    /// Release the in-flight state a bulk operation owns and report which one
    /// it was, so the caller can name it in its status line.
    fn finish_delete_task(&mut self) -> DeleteKind {
        self.delete_cancel = None;
        self.library
            .delete_task
            .take()
            .map(|task| task.kind)
            .unwrap_or(DeleteKind::ToRecentlyDeleted)
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

    /// True while a background index rebuild is running.
    pub fn rebuild_running(&self) -> bool {
        self.rebuild_cancel.is_some()
    }

    /// Spawn a background rebuild of the open library's index. No-op if one is
    /// already running or no library is open.
    pub fn rebuild_library_index(&mut self) {
        if self.rebuild_running() {
            return;
        }
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let progress_tx = self.bg_tx.clone();
        let progress_ctx = self.ctx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.rebuild_cancel = Some(cancel.clone());
        self.library.rebuild_started = Some(std::time::Instant::now());
        self.library.rebuild_stopping = false;
        self.status = "Rebuilding library index…".into();
        workers::spawn(
            "rasterlab-rebuild",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            // A rebuild that dies has no tally to report; the fatal message is
            // what clears `rebuild_started` and unblocks a retry.
            |message| BgMessage::RebuildComplete {
                outcome: rasterlab_library::RebuildOutcome::default(),
                fatal: Some(message),
            },
            move || {
                let result = lib.rebuild_index(cancel, |p| {
                    let _ = progress_tx.send(BgMessage::RebuildProgress(p));
                    progress_ctx.request_repaint();
                });
                match result {
                    Ok(outcome) => BgMessage::RebuildComplete {
                        outcome,
                        fatal: None,
                    },
                    Err(e) => BgMessage::RebuildComplete {
                        outcome: rasterlab_library::RebuildOutcome::default(),
                        fatal: Some(e.to_string()),
                    },
                }
            },
        );
    }

    /// Request that a running rebuild stop after the current file.
    ///
    /// The walk it interrupts has still refreshed every row it reached, so the
    /// index is left usable and running the rebuild again finishes the job.
    pub fn stop_rebuild(&mut self) {
        if let Some(cancel) = &self.rebuild_cancel {
            cancel.store(true, Ordering::Relaxed);
            self.library.rebuild_stopping = true;
            self.status = "Stopping index rebuild…".into();
        }
    }

    pub(super) fn on_rebuild_progress(&mut self, progress: rasterlab_library::RebuildProgress) {
        self.library.rebuild_progress = Some(progress);
        if let Some(text) = self.library.rebuild_status_text() {
            self.status = text;
        }
    }

    /// Terminal handler for a rebuild that has finished, been stopped, or died.
    ///
    /// Releasing `rebuild_cancel` is what [`Self::rebuild_running`] reports, so
    /// leaving it set would pin the File menu to "Stop Index Rebuild".
    pub(super) fn on_rebuild_complete(
        &mut self,
        outcome: rasterlab_library::RebuildOutcome,
        fatal: Option<String>,
    ) {
        let rasterlab_library::RebuildOutcome {
            total,
            done,
            errors,
            cancelled,
        } = outcome;
        self.rebuild_cancel = None;
        self.library.rebuild_progress = None;
        self.library.rebuild_started = None;
        self.library.rebuild_stopping = false;
        self.library.thumbs.clear();
        self.library.refresh();
        if let Some(e) = fatal {
            self.status = format!("Rebuild failed: {e}");
        } else if cancelled {
            // The files it did not reach are not errors, so report how far it
            // got rather than a photo count that looks like the whole library.
            self.status = format!("Index rebuild stopped: {done} of {total} photos indexed");
            if !errors.is_empty() {
                self.status
                    .push_str(&format!(", {} error(s)", errors.len()));
            }
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
        if !self.library.begin_cached_active_copy_save(hash) {
            return;
        }
        // Evict the stale thumbnail immediately so the grid shows a placeholder
        // while regen is running.
        self.library.thumbs.remove(hash);

        let hash = hash.to_owned();
        let failure_hash = hash.clone();
        workers::spawn(
            "rasterlab-copy-select",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            move |message| BgMessage::ActiveCopySaved {
                hash: failure_hash.clone(),
                copy_idx,
                result: Err(message),
            },
            move || BgMessage::ActiveCopySaved {
                hash: hash.clone(),
                copy_idx,
                result: lib
                    .set_active_copy_and_regenerate_thumbnail(&hash, copy_idx)
                    .map_err(|error| error.to_string()),
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

/// Say which photos a delete left alone, naming them while the list is short
/// enough to be worth reading.
fn protected_message(names: &[String]) -> String {
    /// Above this many, the names are noise and the count is the message.
    const NAMED: usize = 3;
    if names.len() <= NAMED {
        format!("Protected, so not deleted: {}.", names.join(", "))
    } else {
        format!("{} protected photos were not deleted.", names.len())
    }
}
