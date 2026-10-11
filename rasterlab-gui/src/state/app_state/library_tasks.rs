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
use rasterlab_library::{CollectionId, MembershipChange, PhotoId};

use super::{AppMode, AppState, BgMessage, workers};
use crate::state::library_state::{
    CollectionTask, DeleteKind, DeleteTask, ImportKind, ReopenAfterUndo, UndoableDelete, plural,
};

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

/// What a bulk delete operation acts on.
enum DeleteTarget {
    /// The grid selection, taken as the task starts.
    Selection,
    /// Exactly these photos, whatever is selected: an undo puts back what the
    /// move took, not what happens to be selected by the time it is asked for.
    Photos(Vec<PhotoId>),
    /// These collections, leaving their photos alone.
    Collections(Vec<CollectionId>),
    /// Everything in Recently Deleted.
    Everything,
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
        self.forget_cached_projects();
        let scale = self.prefs.library_thumb_scale;
        self.library.create_library(path.clone(), scale);
        self.remember_open_library(path);
    }

    pub fn open_library(&mut self, path: std::path::PathBuf) {
        self.flush_library_metadata_drafts();
        self.forget_cached_projects();
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
        self.start_delete_task(DeleteKind::ToRecentlyDeleted, DeleteTarget::Selection);
    }

    /// Move the selection back out of Recently Deleted.
    pub fn restore_selected(&mut self) {
        self.start_delete_task(DeleteKind::Restore, DeleteTarget::Selection);
    }

    /// Erase the selected Recently Deleted photos for good.
    pub fn permanently_delete_selected(&mut self) {
        self.start_delete_task(DeleteKind::Permanent, DeleteTarget::Selection);
    }

    /// Erase everything in Recently Deleted for good.
    pub fn empty_recently_deleted(&mut self) {
        self.start_delete_task(DeleteKind::Empty, DeleteTarget::Everything);
    }

    /// True when Undo would put back the last move to Recently Deleted.
    ///
    /// Not while another bulk operation is running: the restore is one
    /// itself, and would be refused.  Nor during an edit session, for the
    /// same reason pipeline undo is not: the session points into the open
    /// photo's stack, and an undo that reopens the deleted photo would leave
    /// it pointing into a document that is no longer there.
    pub fn can_undo_delete(&self) -> bool {
        self.library.undoable_delete.is_some()
            && self.library.library.is_some()
            && self.editing.is_none()
            && !self.delete_running()
    }

    /// Put back the photos the last move to Recently Deleted took, without
    /// the user having to go and find them in Recently Deleted.
    ///
    /// When the move was the editor's Delete of one photo and the editor is
    /// still on the photo it went on to afterwards, the photo the user deleted
    /// is the one they expect to see again, so it is noted for
    /// [`AppState::take_photo_to_reopen`].  A grid delete, or one of several
    /// photos, has no single photo to return to, and only restores.
    pub fn undo_delete(&mut self) {
        if !self.can_undo_delete() {
            return;
        }
        let Some(undo) = self.library.undoable_delete.take() else {
            return;
        };
        self.library.reopen_after_undo = match (undo.photos.as_slice(), &self.library_context) {
            ([photo], Some((_, shown))) if undo.from_editor && self.mode == AppMode::Editor => {
                self.library.library.clone().map(|library| ReopenAfterUndo {
                    photo: *photo,
                    library,
                    shown: shown.clone(),
                })
            }
            _ => None,
        };
        self.start_delete_task(DeleteKind::Restore, DeleteTarget::Photos(undo.photos));
    }

    /// Once the restore started by [`AppState::undo_delete`] is over, the
    /// photo to take the editor back to, as `(rlab_path, library_root, hash)`.
    ///
    /// Only if the editor is still where it was when Ctrl+Z was pressed, in
    /// the same library, and the photo is back in the results; a restore that
    /// failed, was stopped, or put the photo somewhere the current view does
    /// not show leaves the editor alone.  Either way the request is used up.
    pub fn take_photo_to_reopen(&mut self) -> Option<(StdPathBuf, StdPathBuf, String)> {
        if self.delete_running() {
            return None;
        }
        let reopen = self.library.reopen_after_undo.take()?;
        let lib = self
            .library
            .library
            .clone()
            .filter(|lib| Arc::ptr_eq(lib, &reopen.library))?;
        let (root, shown) = self.library_context.clone()?;
        if self.mode != AppMode::Editor || shown != reopen.shown {
            return None;
        }
        let photo = self.library.results.iter().find(|p| p.id == reopen.photo)?;
        Some((lib.rlab_path(&photo.hash), root, photo.hash.clone()))
    }

    /// Delete collections, leaving the photos that were in them alone.
    ///
    /// The ids come from the confirmation the user answered rather than from
    /// the marks as they stand now: the sidebar is still live behind the
    /// dialog, and the batch that goes must be the batch that was listed.
    pub fn delete_collections(&mut self, ids: Vec<CollectionId>) {
        self.start_delete_task(DeleteKind::Collections, DeleteTarget::Collections(ids));
    }

    /// True while the editor is showing a photo from the library, which is
    /// where its Delete key works.
    fn in_library_editor(&self) -> bool {
        self.mode == AppMode::Editor && self.library_context.is_some()
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
    fn start_delete_task(&mut self, kind: DeleteKind, target: DeleteTarget) {
        if self.delete_running() {
            return;
        }
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let (photos, collections) = match target {
            DeleteTarget::Selection => (std::mem::take(&mut self.library.selected), Vec::new()),
            DeleteTarget::Photos(photos) => (photos, Vec::new()),
            DeleteTarget::Collections(collections) => {
                self.library.leave_collections(&collections);
                (Vec::new(), collections)
            }
            DeleteTarget::Everything => (Vec::new(), Vec::new()),
        };
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

        // Whatever this run is, it is now the latest thing Ctrl+Z could be
        // asked to undo, and putting back an older move would undo the wrong
        // one — or, after a purge, try to restore photos that are gone.
        self.library.undoable_delete = None;

        let cancel = Arc::new(AtomicBool::new(false));
        self.delete_cancel = Some(cancel.clone());
        self.library.delete_task = Some(DeleteTask {
            kind,
            progress: rasterlab_library::BulkProgress {
                total,
                ..Default::default()
            },
            stopping: false,
            library: Arc::clone(&lib),
            from_editor: self.in_library_editor(),
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

    pub(super) fn on_delete_complete(&mut self, mut outcome: rasterlab_library::BulkOutcome) {
        // Asked before the task is released: it knows where the run was
        // started from, and whether the library it ran against is still open.
        let undo_from_editor = self.library.delete_task.as_ref().and_then(|task| {
            let same_library = self
                .library
                .library
                .as_ref()
                .is_some_and(|lib| Arc::ptr_eq(lib, &task.library));
            (task.kind == DeleteKind::ToRecentlyDeleted && same_library).then_some(task.from_editor)
        });
        let kind = self.finish_delete_task();
        // A run stopped partway is undoable too: what it did move is in
        // Recently Deleted, and putting that back is still what the user would
        // be asking for.
        if let Some(from_editor) = undo_from_editor
            && !outcome.moved.is_empty()
        {
            self.library.undoable_delete = Some(UndoableDelete {
                photos: std::mem::take(&mut outcome.moved),
                from_editor,
            });
        }
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
        // Nothing else on screen says the move can be taken back without a
        // trip to Recently Deleted.
        if self.library.undoable_delete.is_some() {
            self.status.push_str(" (Ctrl+Z to undo)");
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
    // Collection membership
    // -----------------------------------------------------------------------

    /// True while a collection-membership change is running.
    pub fn collection_running(&self) -> bool {
        self.collection_cancel.is_some()
    }

    /// Create a collection and start filing the photos it was asked for into
    /// it.
    ///
    /// The row is created here so the sidebar shows it at once; the photos
    /// follow on the worker, because each of them is a `.rlab` rewrite.  An
    /// `Err` is the name's — the dialog stays open showing it.
    pub fn create_collection(
        &mut self,
        name: &str,
        photos: Vec<PhotoId>,
        change: MembershipChange,
    ) -> Result<(), String> {
        // Checked before the row is created: a collection that came up empty
        // because the worker was busy is worse than being asked to wait, and
        // the dialog is still on screen to say so.
        if !photos.is_empty() && self.collection_running() {
            return Err("Another collection change is still running.".to_owned());
        }
        let id = self.library.create_collection(name)?;
        self.start_collection_task(id, photos, change);
        Ok(())
    }

    /// Add every selected photo to a collection; the ones already in it stay
    /// as they are.
    pub fn add_selected_to_collection(&mut self, id: CollectionId) {
        let photos = self.library.selected.clone();
        self.start_collection_task(id, photos, MembershipChange::Add);
    }

    /// Take every selected photo out of a collection.
    pub fn remove_selected_from_collection(&mut self, id: CollectionId) {
        let photos = self.library.selected.clone();
        self.start_collection_task(id, photos, MembershipChange::Remove);
    }

    /// File every selected photo into a collection and out of the ones it is
    /// in now, in the single pass over the `.rlab` files that a remove
    /// followed by an add would take twice.
    pub fn move_selected_to_collection(&mut self, id: CollectionId) {
        let photos = self.library.selected.clone();
        self.start_collection_task(id, photos, MembershipChange::Move);
    }

    /// Spawn the worker that rewrites each photo's `.rlab` and then brings the
    /// index in line with it.
    ///
    /// The photos are taken as the task starts rather than read from the
    /// selection as it goes: the grid stays live while this runs, and the
    /// batch that is filed must be the batch the user asked for.
    fn start_collection_task(
        &mut self,
        id: CollectionId,
        photos: Vec<PhotoId>,
        change: MembershipChange,
    ) {
        if self.collection_running() || photos.is_empty() {
            return;
        }
        let Some(lib) = self.library.library.clone() else {
            return;
        };
        let name = self
            .library
            .collection_name(id)
            .unwrap_or("collection")
            .to_owned();

        let cancel = Arc::new(AtomicBool::new(false));
        self.collection_cancel = Some(cancel.clone());
        self.library.collection_task = Some(CollectionTask {
            change,
            name: name.clone(),
            progress: rasterlab_library::BulkProgress {
                total: photos.len(),
                ..Default::default()
            },
            stopping: false,
        });
        let verb = match change {
            MembershipChange::Add => "Adding to",
            MembershipChange::Remove => "Removing from",
            MembershipChange::Move => "Moving to",
        };
        self.status = format!("{verb} “{name}”…");

        let progress_tx = self.bg_tx.clone();
        let progress_ctx = self.ctx.clone();
        workers::spawn(
            "rasterlab-library-collection",
            workers::IMAGE_WORKER_STACK,
            self.bg_tx.clone(),
            self.ctx.clone(),
            BgMessage::CollectionFailed,
            move || {
                let report = move |p: rasterlab_library::BulkProgress| {
                    let _ = progress_tx.send(BgMessage::CollectionProgress(p));
                    progress_ctx.request_repaint();
                };
                match lib.change_collection_membership(id, &photos, change, cancel, report) {
                    Ok(outcome) => BgMessage::CollectionComplete { outcome },
                    Err(e) => BgMessage::CollectionFailed(e.to_string()),
                }
            },
        );
    }

    /// Request that a running collection change stop after the current photo.
    pub fn stop_collection_change(&mut self) {
        if let Some(cancel) = &self.collection_cancel {
            cancel.store(true, Ordering::Relaxed);
            if let Some(task) = &mut self.library.collection_task {
                task.stopping = true;
            }
        }
    }

    pub(super) fn on_collection_progress(&mut self, progress: rasterlab_library::BulkProgress) {
        if let Some(task) = &mut self.library.collection_task {
            task.progress = progress;
        }
    }

    pub(super) fn on_collection_complete(&mut self, outcome: rasterlab_library::BulkOutcome) {
        let (verb, name) = self.finish_collection_task();
        self.library.refresh();
        self.status = if outcome.cancelled {
            format!(
                "{verb} “{name}” stopped after {}",
                plural(outcome.done, "photo")
            )
        } else {
            format!("{verb} “{name}”: {}", plural(outcome.done, "photo"))
        };
        // Photos whose file could not be rewritten are not in the collection,
        // and the status line is gone by the next action, so the one that
        // matters gets the banner rather than only a count.
        if let Some((photo, error)) = outcome.errors.first() {
            let more = outcome.errors.len() - 1;
            let mut text = format!("{verb} “{name}” failed for {photo}: {error}");
            if more > 0 {
                text.push_str(&format!(" (and {} more)", plural(more, "photo")));
            }
            self.status
                .push_str(&format!(", {} error(s)", outcome.errors.len()));
            self.library.last_error = Some(text);
        }
    }

    /// Terminal handler for a collection change that will never report again.
    ///
    /// Refreshes like a completed one: a run that died partway through still
    /// filed the photos it had already written, and the grid should show them
    /// where they now are.
    pub(super) fn on_collection_failed(&mut self, message: String) {
        let (verb, name) = self.finish_collection_task();
        self.library.refresh();
        let text = format!("{verb} “{name}” failed: {message}");
        self.status = text.clone();
        self.library.last_error = Some(text);
    }

    /// Release the in-flight state the change owns, reporting what it was for
    /// the status line that follows it.
    fn finish_collection_task(&mut self) -> (&'static str, String) {
        self.collection_cancel = None;
        // Membership lives in each photo's `.rlab`, and even a change that
        // failed part-way has rewritten some of them.
        self.forget_cached_projects();
        self.library
            .collection_task
            .take()
            .map(|task| (task.past_verb(), task.name))
            .unwrap_or(("Added to", "collection".to_owned()))
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

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        time::{Duration, Instant},
    };

    use rasterlab_core::{
        Image,
        ops::{SaturationOp, SepiaOp},
        pipeline::EditPipeline,
        project::{RlabFile, RlabMeta},
    };
    use rasterlab_library::BulkOutcome;

    use super::*;
    use crate::state::{
        edit_session::{EditSession, EditingTool},
        virtual_copies::VirtualCopyStore,
    };

    type Ids = &'static [PhotoId];
    /// Something done to the state, named for the failure message.
    type Action = (&'static str, fn(&mut AppState));

    fn state_with_library(dir: &Path) -> AppState {
        let mut state = AppState::new(egui::Context::default(), None);
        state.library.create_library(dir.to_path_buf(), 0.5);
        // Where opening a library leaves the app, and where the grid is.
        state.mode = AppMode::Library;
        assert!(
            state.library.library.is_some(),
            "{:?}",
            state.library.last_error
        );
        state
    }

    /// Stand in for a running bulk operation of `kind`, without a worker, so
    /// its completion can be handed over directly.
    fn pretend_running(state: &mut AppState, kind: DeleteKind) {
        state.delete_cancel = Some(Arc::new(AtomicBool::new(false)));
        state.library.delete_task = Some(DeleteTask {
            kind,
            progress: Default::default(),
            stopping: false,
            library: state.library.library.clone().unwrap(),
            from_editor: false,
        });
    }

    /// Fold worker reports in until the running bulk operation is over.
    fn wait_for_delete(state: &mut AppState) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while state.delete_running() {
            assert!(
                Instant::now() < deadline,
                "the delete worker never reported back"
            );
            std::thread::sleep(Duration::from_millis(5));
            state.poll_background();
        }
    }

    fn undoable(photos: &[PhotoId]) -> Option<UndoableDelete> {
        Some(UndoableDelete {
            photos: photos.to_vec(),
            from_editor: false,
        })
    }

    #[test]
    fn only_a_move_that_moved_something_can_be_undone() {
        let cases: [(DeleteKind, Ids, bool, Option<Ids>); 7] = [
            (DeleteKind::ToRecentlyDeleted, &[1, 2], false, Some(&[1, 2])),
            // Stopped partway: what did move can still go back.
            (DeleteKind::ToRecentlyDeleted, &[1], true, Some(&[1])),
            // Everything was protected or failed: nothing to put back.
            (DeleteKind::ToRecentlyDeleted, &[], false, None),
            (DeleteKind::Restore, &[1], false, None),
            (DeleteKind::Permanent, &[1], false, None),
            (DeleteKind::Empty, &[1], false, None),
            (DeleteKind::Collections, &[1], false, None),
        ];
        for (kind, moved, cancelled, expected) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let mut state = state_with_library(tmp.path());
            pretend_running(&mut state, kind);

            state.on_delete_complete(BulkOutcome {
                done: moved.len(),
                moved: moved.to_vec(),
                cancelled,
                ..Default::default()
            });

            let what = format!("{kind:?}, moved {moved:?}, cancelled {cancelled}");
            assert_eq!(
                state.library.undoable_delete,
                expected.and_then(undoable),
                "{what}"
            );
            assert_eq!(state.can_undo_delete(), expected.is_some(), "{what}");
            assert_eq!(
                state.status.contains("Ctrl+Z to undo"),
                expected.is_some(),
                "{what}: {}",
                state.status
            );
        }
    }

    /// Photo ids are only unique within a library, so a move that finishes
    /// after the user has switched away must not offer to restore its ids in
    /// the one they switched to.
    #[test]
    fn a_move_that_finishes_after_a_library_switch_cannot_be_undone() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let mut state = state_with_library(first.path());
        pretend_running(&mut state, DeleteKind::ToRecentlyDeleted);
        state
            .library
            .create_library(second.path().to_path_buf(), 0.5);

        state.on_delete_complete(BulkOutcome {
            done: 1,
            moved: vec![1],
            ..Default::default()
        });

        assert_eq!(state.library.undoable_delete, None);
        assert!(!state.status.contains("Ctrl+Z"), "{}", state.status);
    }

    #[test]
    fn switching_libraries_retires_the_undo() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let mut state = state_with_library(first.path());
        state.library.undoable_delete = undoable(&[1]);

        state
            .library
            .create_library(second.path().to_path_buf(), 0.5);

        assert_eq!(state.library.undoable_delete, None);
    }

    /// Any other bulk delete is now the latest action, and after a purge or
    /// an empty the photos an older undo names may be gone for good.
    #[test]
    fn starting_any_delete_retires_the_undo() {
        let starts: [Action; 5] = [
            ("move", AppState::move_selected_to_recently_deleted),
            ("restore", AppState::restore_selected),
            ("permanent delete", AppState::permanently_delete_selected),
            ("empty", AppState::empty_recently_deleted),
            ("collections", |state| state.delete_collections(vec![1])),
        ];
        for (what, start) in starts {
            let tmp = tempfile::tempdir().unwrap();
            let mut state = state_with_library(tmp.path());
            state.library.undoable_delete = undoable(&[1]);
            state.library.selected = vec![2];

            start(&mut state);

            assert!(state.delete_running(), "{what} did not start");
            assert_eq!(state.library.undoable_delete, None, "{what}");
            assert!(!state.can_undo_delete(), "{what}");
            wait_for_delete(&mut state);
        }
    }

    /// A library holding one imported photo, shown in the grid.
    fn library_with_photo(dir: &Path) -> (AppState, rasterlab_library::PhotoRow) {
        let mut state = state_with_library(dir);
        let image = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("test_images/meta_test.jpg");
        let lib = state.library.library.clone().unwrap();
        lib.import_files(&[image], |_| {}).unwrap();
        state.library.refresh();
        let photo = state.library.results[0].clone();
        (state, photo)
    }

    /// Put the editor on `hash`, as opening a library photo does.
    fn show_in_editor(state: &mut AppState, root: &Path, hash: &str) {
        state.mode = AppMode::Editor;
        state.library_context = Some((root.to_path_buf(), hash.to_owned()));
    }

    /// The whole round trip against a real library: a delete offers the undo,
    /// the undo puts the photo back, and the offer is used up.  Made from the
    /// grid it only restores; made from the editor it also takes the editor
    /// back to the photo.
    #[test]
    fn undoing_a_move_restores_exactly_what_moved() {
        for from_editor in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let (mut state, photo) = library_with_photo(tmp.path());
            if from_editor {
                show_in_editor(&mut state, tmp.path(), &photo.hash);
            }

            state.library.select_only(photo.id);
            state.move_selected_to_recently_deleted();
            wait_for_delete(&mut state);

            assert!(state.library.results.is_empty(), "{}", state.status);
            assert_eq!(
                state.library.undoable_delete,
                Some(UndoableDelete {
                    photos: vec![photo.id],
                    from_editor
                })
            );
            assert!(
                state.status.ends_with("(Ctrl+Z to undo)"),
                "{}",
                state.status
            );
            assert!(state.can_undo_delete());

            state.undo_delete();
            assert!(state.delete_running());
            assert!(!state.can_undo_delete(), "not while the restore runs");
            assert_eq!(
                state.take_photo_to_reopen(),
                None,
                "not before the restore lands"
            );
            wait_for_delete(&mut state);

            let back: Vec<PhotoId> = state.library.results.iter().map(|p| p.id).collect();
            assert_eq!(back, [photo.id], "{}", state.status);
            assert_eq!(state.status, "Restored 1 photo");
            assert_eq!(state.library.undoable_delete, None);
            assert!(!state.can_undo_delete());
            let lib = state.library.library.clone().unwrap();
            assert_eq!(
                state.take_photo_to_reopen(),
                from_editor.then(|| (
                    lib.rlab_path(&photo.hash),
                    tmp.path().to_path_buf(),
                    photo.hash.clone()
                ))
            );
            assert!(state.library.reopen_after_undo.is_none(), "used up");
        }
    }

    /// The editor goes back to the restored photo only when it was the
    /// editor's own Delete of a single photo and the editor is still on a
    /// library photo.
    #[test]
    fn undo_reopens_only_a_single_photo_deleted_from_the_editor() {
        let cases: [(bool, Ids, AppMode, bool, Option<PhotoId>); 5] = [
            (true, &[7], AppMode::Editor, true, Some(7)),
            (false, &[7], AppMode::Editor, true, None),
            (true, &[7, 8], AppMode::Editor, true, None),
            (true, &[7], AppMode::Library, true, None),
            (true, &[7], AppMode::Editor, false, None),
        ];
        for (from_editor, photos, mode, in_library, expected) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let mut state = state_with_library(tmp.path());
            state.library.undoable_delete = Some(UndoableDelete {
                photos: photos.to_vec(),
                from_editor,
            });
            state.mode = mode;
            state.library_context =
                in_library.then(|| (tmp.path().to_path_buf(), "next".to_owned()));

            state.undo_delete();

            let what = format!("from editor {from_editor}, {photos:?}, {mode:?}, {in_library}");
            let reopen = state.library.reopen_after_undo.as_ref();
            assert_eq!(reopen.map(|r| r.photo), expected, "{what}");
            assert!(
                reopen.is_none_or(|r| r.shown == "next"),
                "{what}: the photo on screen must be noted"
            );
            assert!(state.delete_running(), "{what}: the restore must still run");
            wait_for_delete(&mut state);
        }
    }

    /// The restore runs in the background, and the user may carry on in the
    /// meantime.  Having moved on, being dragged back would undo their
    /// navigation, not the delete.
    #[test]
    fn the_editor_goes_back_only_if_the_user_stayed_put() {
        let other_library = tempfile::tempdir().unwrap();
        let other_root = other_library.path().to_path_buf();
        type MoveOn = Box<dyn Fn(&mut AppState, &rasterlab_library::PhotoRow)>;
        let moves: [(&str, MoveOn); 4] = [
            ("stayed put", Box::new(|_, _| {})),
            (
                "arrowed on",
                Box::new(|state, _| {
                    let (root, _) = state.library_context.clone().unwrap();
                    state.library_context = Some((root, "another photo".to_owned()));
                }),
            ),
            (
                "left the editor",
                Box::new(|state, _| state.mode = AppMode::Library),
            ),
            // The other library holds a photo under the same id, so only the
            // library check stands between the reopen and the wrong photo.
            (
                "switched library",
                Box::new(move |state, photo| {
                    library_with_photo(&other_root);
                    state.library.open_library(other_root.clone(), 0.5);
                    assert!(
                        state.library.results.iter().any(|p| p.id == photo.id),
                        "the other library should reuse the id"
                    );
                    state.mode = AppMode::Editor;
                }),
            ),
        ];
        for (what, moved_on) in moves {
            let tmp = tempfile::tempdir().unwrap();
            let (mut state, photo) = library_with_photo(tmp.path());
            show_in_editor(&mut state, tmp.path(), &photo.hash);
            state.library.select_only(photo.id);
            state.move_selected_to_recently_deleted();
            wait_for_delete(&mut state);
            // Where the editor went on to after the delete.
            show_in_editor(&mut state, tmp.path(), "next");

            state.undo_delete();
            assert!(state.library.reopen_after_undo.is_some(), "{what}");
            moved_on(&mut state, &photo);
            wait_for_delete(&mut state);

            let reopened = state.take_photo_to_reopen().map(|(_, _, hash)| hash);
            let expected = (what == "stayed put").then(|| photo.hash.clone());
            assert_eq!(reopened, expected, "{what}");
            assert!(state.library.reopen_after_undo.is_none(), "{what}: used up");
        }
    }

    /// An edit session points into the open photo's stack, and an undo that
    /// reopened the deleted photo would leave it pointing into a document
    /// that is gone — the same reason pipeline undo waits for the session.
    #[test]
    fn an_edit_session_holds_the_undo_back() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = state_with_library(tmp.path());
        state.library.undoable_delete = undoable(&[1]);
        state.editing = Some(EditSession {
            op_index: 0,
            tool: EditingTool::Saturation,
            was_enabled: true,
        });

        assert!(!state.can_undo_delete());
        state.undo_delete();

        assert!(!state.delete_running());
        assert_eq!(
            state.library.undoable_delete,
            undoable(&[1]),
            "still there for when the session ends"
        );
        state.editing = None;
        assert!(state.can_undo_delete());
    }

    /// A store whose stack can be both undone and redone, with a second copy
    /// to switch to, so every action below has something to act on.
    fn state_with_edits() -> AppState {
        let mut pipeline = EditPipeline::new(Image::new(8, 8));
        pipeline.push_op(Box::new(SaturationOp::new(0.5)));
        pipeline.push_op(Box::new(SepiaOp::new(0.25)));
        assert!(pipeline.undo());
        let mut store = VirtualCopyStore::new("Copy 1".into(), pipeline);
        store.add_copy("Copy 2".into());
        store.set_active(0);
        let mut state = AppState::new(egui::Context::default(), None);
        state.copies = Some(store);
        state
    }

    /// "Most recent action wins": once the user edits the photo the editor
    /// moved on to, or switches to another copy of it, Ctrl+Z is about that.
    #[test]
    fn an_edit_retires_the_undo() {
        let edits: [Action; 6] = [
            ("push", |state| state.push_op(Box::new(SepiaOp::new(0.5)))),
            ("toggle", |state| state.toggle_op(0)),
            ("remove", |state| state.remove_op(0)),
            ("undo", AppState::undo),
            ("redo", AppState::redo),
            ("switch copy", |state| state.switch_copy(1)),
        ];
        for (what, edit) in edits {
            let mut state = state_with_edits();
            state.library.undoable_delete = undoable(&[1]);

            edit(&mut state);

            let switched = state.copies.as_ref().unwrap().active_index() == 1;
            assert!(state.is_dirty || switched, "{what} did nothing");
            assert_eq!(state.library.undoable_delete, None, "{what}");
        }
    }

    /// The editor opens the next photo straight after an editor delete, and
    /// that must not use up the undo.  Library photos open as projects; both
    /// load paths are covered, each with an open that restores an autosave,
    /// which leaves the document dirty without an edit.
    #[test]
    fn opening_a_photo_keeps_the_undo() {
        for project in [false, true] {
            for restore_autosave in [false, true] {
                let mut state = AppState::new(egui::Context::default(), None);
                // The autosave carries an edit the project on disk does not,
                // so restoring it is what makes the document dirty.
                let saved = |ops: usize| {
                    let mut pipeline = EditPipeline::new(Image::new(4, 4));
                    for _ in 0..ops {
                        pipeline.push_op(Box::new(SepiaOp::new(0.5)));
                    }
                    VirtualCopyStore::new("Copy 1".into(), pipeline)
                        .save_states()
                        .unwrap()
                };
                if restore_autosave {
                    state.autosave_restore = Some(saved(1));
                }
                state.library.undoable_delete = undoable(&[1]);

                if project {
                    let (copies, active) = saved(0);
                    let rlab = RlabFile::new(
                        RlabMeta::new("test", None::<String>, 4, 4),
                        Vec::new(),
                        copies,
                        active,
                        None,
                    );
                    state.on_project_loaded(
                        "next.rlab".into(),
                        Arc::new(rlab),
                        Arc::new(Image::new(4, 4)),
                    );
                } else {
                    state.on_image_loaded("next.png".into(), Image::new(4, 4), Vec::new());
                }

                let what = format!("project {project}, autosave restored {restore_autosave}");
                assert!(state.copies.is_some(), "{what}: {}", state.status);
                assert_eq!(state.is_dirty, restore_autosave, "{what}");
                assert_eq!(state.library.undoable_delete, undoable(&[1]), "{what}");
            }
        }
    }
}
