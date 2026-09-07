use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rasterlab_core::{
    formats::FormatRegistry,
    library_meta::{CollectionRef, LibraryMeta},
    pipeline::EditPipeline,
    project::{RlabFile, read_library_summary},
    verified_write::{create_dir_all_synced, rename_synced},
};
use uuid::Uuid;

use crate::{
    db_trait::{
        CollectionId, CollectionRow, ImportSessionRow, LibraryDb, PhotoId, PhotoRow,
        RecentlyDeletedRow, SortOrder,
    },
    import::{self, ImportCollection, ImportSession},
    reconstruct::{self, RebuildOutcome, RebuildProgress},
    search::SearchFilter,
    stoolap_db::StoolapDb,
    thumbnail::{generate_thumbnail, write_thumbnail},
};

// ── Public types ──────────────────────────────────────────────────────────────

/// Opening the library failed because another process already has it open.
///
/// The index takes an exclusive `flock` on `library.db` and holds it for as
/// long as the library is open, so one library means one process.  A long CLI
/// run is the case that matters: `rasterlab library rebuild` over a large
/// network library can take hours, and for all of them the GUI cannot open
/// that library.  Callers get this as its own type so they can say "busy, try
/// later" rather than reporting a library that is perfectly healthy as broken.
#[derive(Debug, thiserror::Error)]
#[error("the library is already open in another RasterLab process")]
pub struct LibraryBusy;

#[derive(Debug, Clone, Default)]
pub struct ImportProgress {
    pub total: usize,
    /// Files processed in the current phase. During scanning this is files whose
    /// capture date has been read; during importing this is files attempted.
    pub done: usize,
    /// New photos actually imported during the import phase.
    pub imported: usize,
    pub current_file: PathBuf,
    pub skipped_duplicates: usize,
    pub errors: Vec<(PathBuf, String)>,
    /// True during the pre-import capture-date scan (phase 1 of a grouped
    /// folder import). Lets the UI show "Scanning…" instead of a frozen
    /// "Importing…" while capture dates are read.
    pub scanning: bool,
}

/// Progress of a running bulk operation on the Recently Deleted area.
#[derive(Debug, Clone, Default)]
pub struct DeleteProgress {
    pub total: usize,
    /// Photos attempted so far.
    pub done: usize,
    /// Names of photos left where they are because they are protected.
    pub protected: Vec<String>,
    /// Per-photo `(name, message)` failures so far.
    pub errors: Vec<(String, String)>,
}

/// Final tally of a bulk operation on the Recently Deleted area.
///
/// Every photo is attempted, so a run that hits trouble reports what it did
/// manage alongside what it could not: one missing file in a selection of five
/// hundred should not decide the fate of the other four hundred and ninety
/// nine.
#[derive(Debug, Clone, Default)]
pub struct DeleteOutcome {
    /// Photos moved, restored, or erased.
    pub done: usize,
    /// Names of photos left alone because they are protected.
    pub protected: Vec<String>,
    /// Per-photo `(name, message)` failures.
    ///
    /// Named rather than pathed: a `.rlab` is named by content hash, so
    /// `files/3a/3adf….rlab` tells a photographer nothing about which of
    /// their photographs did not move.
    pub errors: Vec<(String, String)>,
    /// Content hashes whose files and thumbnails are now gone for good, so a
    /// caller holding cached thumbnails knows which of them to drop.
    pub purged: Vec<String>,
    /// True when the run stopped early because `cancel` was raised.
    pub cancelled: bool,
}

/// What became of one item inside a bulk operation.
///
/// A failure is a value rather than an `Err` because none of these is fatal to
/// the run: the loop records it and carries on to the next item.
enum Step {
    /// The item was moved, restored, or erased as asked.
    Done,
    /// It is protected, so it was left where it is.
    Protected(String),
    /// It could not be done. `item` names it for the user.
    Failed { item: String, error: String },
}

impl Step {
    /// A photo the index no longer lists. `expected` completes "photo 7 is
    /// not …".
    fn missing(id: PhotoId, expected: &str) -> Self {
        Self::Failed {
            item: format!("photo {id}"),
            error: format!("is not {expected}"),
        }
    }

    fn failed(row: &PhotoRow, error: &anyhow::Error) -> Self {
        Self::Failed {
            item: photo_label(row),
            error: error.to_string(),
        }
    }
}

// ── Library ───────────────────────────────────────────────────────────────────

pub struct Library {
    root: PathBuf,
    db: Box<dyn LibraryDb>,
    registry: FormatRegistry,
    /// Serializes read-modify-write operations on authoritative project files.
    /// Atomic rename prevents torn files, but without this guard two background
    /// tasks could still overwrite one another's metadata or edit stack.
    project_write_lock: Mutex<()>,
}

impl Library {
    /// Open (or create) a library at `path` using the default stoolap backend.
    pub fn open_or_create(path: &Path) -> Result<Self> {
        let db = StoolapDb::open(path)?;
        Self::with_db(path, Box::new(db))
    }

    /// Open (or create) a library at `path` with an injected DB backend
    /// (useful for testing with a fake/mock DB).
    pub fn with_db(path: &Path, db: Box<dyn LibraryDb>) -> Result<Self> {
        create_dir_all_synced(&path.join("files"))?;
        create_dir_all_synced(&path.join("thumbs"))?;
        create_dir_all_synced(&path.join("recently_deleted/files"))?;
        db.init()?;
        let library = Self {
            root: path.to_path_buf(),
            db,
            registry: FormatRegistry::with_builtins(),
            project_write_lock: Mutex::new(()),
        };
        library.reconcile_recently_deleted()?;
        Ok(library)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    // ── Paths ─────────────────────────────────────────────────────────────

    pub fn rlab_path(&self, hash: &str) -> PathBuf {
        import::rlab_path(&self.root, hash)
    }

    pub fn thumb_path(&self, hash: &str) -> PathBuf {
        import::thumb_path(&self.root, hash)
    }

    pub fn recently_deleted_path(&self, hash: &str) -> PathBuf {
        import::rlab_path(&self.root.join("recently_deleted"), hash)
    }

    /// Where a photo's `.rlab` is right now, whichever side of Recently
    /// Deleted it is on.
    ///
    /// [`Library::rlab_path`] names where an *active* photo's file belongs; a
    /// deleted one has been moved out from under it, so reading a photo at
    /// that path turns "this photo is in Recently Deleted" into an I/O error
    /// on a file that is sitting safely somewhere else.  Anything that reads a
    /// photo the user can still see — its metadata, its collections — wants
    /// this instead.
    ///
    /// A photo whose file is on neither side resolves to the active path, so
    /// the error a caller reports names where the file belongs rather than
    /// where it was last put.
    pub fn photo_rlab_path(&self, hash: &str) -> PathBuf {
        let active = self.rlab_path(hash);
        if active.exists() {
            return active;
        }
        let deleted = self.recently_deleted_path(hash);
        if deleted.exists() { deleted } else { active }
    }

    fn lock_project_writes(&self) -> Result<MutexGuard<'_, ()>> {
        self.project_write_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("project write lock is poisoned"))
    }

    // ── Import ────────────────────────────────────────────────────────────

    /// Import a list of individual files.  `progress_cb` is called after each
    /// file so the caller can update a progress bar.
    pub fn import_files(
        &self,
        paths: &[PathBuf],
        progress_cb: impl Fn(ImportProgress) + Send + 'static,
    ) -> Result<ImportSession> {
        let cancelled = Arc::new(AtomicBool::new(false));
        import::import_files(
            &self.root,
            self.db.as_ref(),
            &self.registry,
            paths,
            cancelled,
            &progress_cb,
        )
    }

    /// Recursively import all supported images found under `folder`, grouping
    /// them into one back-dated import session per run of same-or-consecutive
    /// capture days.  A day of more than
    /// [`HEAVY_DAY_PHOTOS`](crate::import::HEAVY_DAY_PHOTOS) photos gets a
    /// session to itself.  Returns one [`ImportSession`] per group.
    pub fn import_folder(
        &self,
        folder: &Path,
        progress_cb: impl Fn(ImportProgress) + Send + 'static,
    ) -> Result<Vec<ImportSession>> {
        self.import_folder_into_collection(folder, ImportCollection::None, progress_cb)
    }

    /// [`Library::import_folder`], additionally filing every photo it imports
    /// into a collection as `collection` describes.
    ///
    /// Collections are created on demand and an existing one of the same name
    /// is reused, so importing a folder a second time adds whatever is new to
    /// the collection the first import made rather than starting another one.
    /// Only new photos are filed: a duplicate is skipped whole, memberships
    /// included.
    pub fn import_folder_into_collection(
        &self,
        folder: &Path,
        collection: ImportCollection,
        progress_cb: impl Fn(ImportProgress) + Send + 'static,
    ) -> Result<Vec<ImportSession>> {
        let paths = collect_image_paths(folder, &self.registry);
        self.import_expanded(
            &paths,
            Some(folder),
            collection,
            Arc::new(AtomicBool::new(false)),
            &progress_cb,
        )
    }

    /// [`Library::import_folder_into_collection`] over a mixed list of files
    /// and folders, stopping when `cancel` is set.
    ///
    /// Each folder is walked for supported images and the whole run — loose
    /// files included — is grouped into back-dated sessions together, so a
    /// command line naming a shoot folder and the two stragglers next to it
    /// lands them in the sessions their capture dates ask for rather than in
    /// one pile per argument.  A path named twice, or named alongside a folder
    /// that contains it, is imported once.
    ///
    /// Setting `cancel` stops the run after the file it is on. Nothing has to
    /// be undone: imports are keyed by content hash, so re-running the same
    /// command picks up where this one left off.
    pub fn import_paths(
        &self,
        paths: &[PathBuf],
        collection: ImportCollection,
        cancel: Arc<AtomicBool>,
        progress_cb: impl Fn(ImportProgress),
    ) -> Result<Vec<ImportSession>> {
        let mut files = Vec::new();
        let mut seen = HashSet::new();
        let mut folders = Vec::new();
        let mut loose = 0usize;
        for path in paths {
            if path.is_dir() {
                folders.push(path.as_path());
                files.extend(
                    collect_image_paths(path, &self.registry)
                        .into_iter()
                        .filter(|file| seen.insert(file.clone())),
                );
            } else {
                // Not extension-filtered: a file the user named explicitly is a
                // file they meant, and an unreadable one is worth an error
                // naming it rather than a silent omission from the tally.
                loose += 1;
                if seen.insert(path.clone()) {
                    files.push(path.clone());
                }
            }
        }
        // The session's recorded source is only meaningful when the whole run
        // came from one place.
        let source = match (loose, folders.as_slice()) {
            (0, [only]) => Some(*only),
            _ => None,
        };
        self.import_expanded(&files, source, collection, cancel, &progress_cb)
    }

    fn import_expanded(
        &self,
        files: &[PathBuf],
        source_dir: Option<&Path>,
        collection: ImportCollection,
        cancel: Arc<AtomicBool>,
        progress_cb: &dyn Fn(ImportProgress),
    ) -> Result<Vec<ImportSession>> {
        import::import_folder_grouped(
            &self.root,
            self.db.as_ref(),
            &self.registry,
            files,
            cancel,
            source_dir,
            collection,
            progress_cb,
        )
    }

    // ── Integrity scrub ───────────────────────────────────────────────────

    /// Walk every `.rlab` file, verify its integrity, and repair correctable
    /// corruption in place (backing the damaged original up under
    /// `recovered/`). Clean pre-ECC files are upgraded to v4. `cancel` is
    /// polled between files so the caller can stop the scrub early.
    pub fn scrub(
        &self,
        cancel: Arc<AtomicBool>,
        progress_cb: impl Fn(crate::ScrubProgress),
    ) -> Result<crate::ScrubOutcome> {
        crate::scrub::scrub_with_project_lock(
            &self.root,
            cancel,
            &progress_cb,
            &self.project_write_lock,
        )
    }

    // ── Photos ────────────────────────────────────────────────────────────

    pub fn all_photos(&self, sort: SortOrder) -> Result<Vec<PhotoRow>> {
        self.db.all_photos(sort)
    }

    pub fn search(&self, filter: &SearchFilter, sort: SortOrder) -> Result<Vec<PhotoRow>> {
        self.db.search(filter, sort)
    }

    /// Move photos into this library's Recently Deleted area.
    ///
    /// Each `.rlab` is renamed within the library filesystem, which is fast and
    /// recoverable on local disks and network mounts alike. Index metadata and
    /// thumbnails stay put, so a photo can be restored exactly.
    ///
    /// A protected photo is reported and left alone rather than failing the
    /// run.  `cancel` is polled before each photo; stopping partway leaves what
    /// has already moved in Recently Deleted, which is a state the library is
    /// happy in — each photo moves on its own and the user can move the rest
    /// later or restore these.
    pub fn move_to_recently_deleted(
        &self,
        photos: &[PhotoId],
        cancel: Arc<AtomicBool>,
        progress_cb: impl Fn(DeleteProgress),
    ) -> Result<DeleteOutcome> {
        let index = photo_index(self.db.all_photos(SortOrder::default())?);
        Ok(bulk_op(photos, &cancel, progress_cb, |id| {
            let Some(row) = index.get(&id) else {
                return Step::missing(id, "in the library index");
            };
            if row.protected {
                return Step::Protected(photo_label(row));
            }
            match self.move_one_to_recently_deleted(row) {
                Ok(()) => Step::Done,
                Err(error) => Step::failed(row, &error),
            }
        }))
    }

    /// Move a photo into this library's Recently Deleted area.
    pub fn delete_photo(&self, photo_id: PhotoId) -> Result<()> {
        let outcome = self.move_to_recently_deleted(&[photo_id], idle_cancel(), |_| {})?;
        single_photo_result(&outcome)
    }

    /// Permanently remove an active photo's `.rlab`, thumbnail, and DB row.
    ///
    /// This is intended for maintenance and headless test environments.
    pub fn delete_photo_permanently(&self, photo_id: PhotoId) -> Result<()> {
        let photos = self.db.all_photos(SortOrder::default())?;
        let Some(row) = photos.iter().find(|r| r.id == photo_id) else {
            bail!("photo {photo_id} not found");
        };
        if row.protected {
            let name = row.original_filename.as_deref().unwrap_or("photo");
            bail!("\"{name}\" is protected and cannot be deleted");
        }
        let _write_guard = self.lock_project_writes()?;
        self.permanently_remove(row, &self.rlab_path(&row.hash))?;
        self.db.delete_empty_sessions()?;
        Ok(())
    }

    pub fn recently_deleted(&self) -> Result<Vec<RecentlyDeletedRow>> {
        self.db.recently_deleted()
    }

    /// Move photos back out of Recently Deleted, on the same terms as
    /// [`Library::move_to_recently_deleted`].
    pub fn restore_photos(
        &self,
        photos: &[PhotoId],
        cancel: Arc<AtomicBool>,
        progress_cb: impl Fn(DeleteProgress),
    ) -> Result<DeleteOutcome> {
        let index = photo_index(self.deleted_rows()?);
        Ok(bulk_op(photos, &cancel, progress_cb, |id| {
            let Some(row) = index.get(&id) else {
                return Step::missing(id, "in Recently Deleted");
            };
            match self.restore_one(row) {
                Ok(()) => Step::Done,
                Err(error) => Step::failed(row, &error),
            }
        }))
    }

    pub fn restore_photo(&self, photo_id: PhotoId) -> Result<()> {
        let outcome = self.restore_photos(&[photo_id], idle_cancel(), |_| {})?;
        single_photo_result(&outcome)
    }

    /// Erase photos from Recently Deleted for good: `photos` names them, or
    /// `None` empties the whole area.
    ///
    /// Sessions left without photos are dropped once at the end rather than
    /// after each removal, which on a network-mounted index is the difference
    /// between one round trip and one per photo.
    pub fn purge_recently_deleted(
        &self,
        photos: Option<&[PhotoId]>,
        cancel: Arc<AtomicBool>,
        progress_cb: impl Fn(DeleteProgress),
    ) -> Result<DeleteOutcome> {
        let rows = self.deleted_rows()?;
        let everything: Vec<PhotoId> = rows.iter().map(|row| row.id).collect();
        let index = photo_index(rows);
        let ids = photos.unwrap_or(&everything);

        let mut purged: Vec<String> = Vec::new();
        let mut outcome = bulk_op(ids, &cancel, progress_cb, |id| {
            let Some(row) = index.get(&id) else {
                return Step::missing(id, "in Recently Deleted");
            };
            match self.purge_one(row) {
                Ok(()) => {
                    purged.push(row.hash.clone());
                    Step::Done
                }
                Err(error) => Step::failed(row, &error),
            }
        });
        outcome.purged = purged;
        // Bookkeeping on top of work already committed: the photos are gone
        // either way, so a failure here is recorded rather than allowed to
        // report a successful run as a failed one — and to take `purged`,
        // which is how the caller knows to drop their thumbnails, down with it.
        if let Err(error) = self.db.delete_empty_sessions() {
            outcome
                .errors
                .push(("the library index".to_owned(), error.to_string()));
        }
        Ok(outcome)
    }

    pub fn delete_recently_deleted_permanently(&self, photo_id: PhotoId) -> Result<()> {
        let outcome = self.purge_recently_deleted(Some(&[photo_id]), idle_cancel(), |_| {})?;
        single_photo_result(&outcome)
    }

    pub fn empty_recently_deleted(&self) -> Result<usize> {
        let outcome = self.purge_recently_deleted(None, idle_cancel(), |_| {})?;
        if let Some((photo, error)) = outcome.errors.first() {
            bail!("{photo}: {error}");
        }
        Ok(outcome.done)
    }

    /// Every Recently Deleted photo, without the timestamp the bulk operations
    /// have no use for.
    fn deleted_rows(&self) -> Result<Vec<PhotoRow>> {
        Ok(self
            .db
            .recently_deleted()?
            .into_iter()
            .map(|row| row.photo)
            .collect())
    }

    /// Storage first, index second, with the file put back if the index will
    /// not follow — a photo whose row still calls it active must still be
    /// where an active photo lives.
    fn move_one_to_recently_deleted(&self, row: &PhotoRow) -> Result<()> {
        let _write_guard = self.lock_project_writes()?;
        let active = self.rlab_path(&row.hash);
        let deleted = self.recently_deleted_path(&row.hash);
        let moved = move_library_file(&active, &deleted)?;
        if let Err(error) = self.db.mark_photo_deleted(row.id, unix_now()) {
            if moved {
                let _ = move_library_file(&deleted, &active);
            }
            return Err(error.context("record Recently Deleted state"));
        }
        Ok(())
    }

    /// Under the same guard as its siblings, though with nothing to roll back:
    /// the file is gone before the row is, so a failed index write leaves a
    /// stale row rather than a resurrected photo.
    fn purge_one(&self, row: &PhotoRow) -> Result<()> {
        let _write_guard = self.lock_project_writes()?;
        self.permanently_remove(row, &self.recently_deleted_path(&row.hash))
    }

    /// The mirror of [`Library::move_one_to_recently_deleted`].
    fn restore_one(&self, row: &PhotoRow) -> Result<()> {
        let _write_guard = self.lock_project_writes()?;
        let deleted = self.recently_deleted_path(&row.hash);
        let active = self.rlab_path(&row.hash);
        let moved = move_library_file(&deleted, &active)?;
        if let Err(error) = self.db.restore_photo(row.id) {
            if moved {
                let _ = move_library_file(&active, &deleted);
            }
            return Err(error.context("restore photo index state"));
        }
        Ok(())
    }

    /// Complete a file-first move that was interrupted before its database
    /// flag committed. Already-indexed deleted files retain their timestamps.
    fn reconcile_recently_deleted(&self) -> Result<()> {
        let already_deleted: HashSet<String> = self
            .db
            .recently_deleted()?
            .into_iter()
            .map(|row| row.photo.hash)
            .collect();
        let deleted_files = self.root.join("recently_deleted/files");
        for entry in walkdir::WalkDir::new(deleted_files)
            .into_iter()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry.file_type().is_file()
                    && entry.path().extension().is_some_and(|ext| ext == "rlab")
            })
        {
            let Some(hash) = entry.path().file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            if already_deleted.contains(hash) {
                continue;
            }
            if let Some(row) = self.db.photo_by_hash(hash)? {
                self.db.mark_photo_deleted(row.id, unix_now())?;
            }
        }
        Ok(())
    }

    /// Storage first, index second: a failed index mutation may leave a stale
    /// row, but can never resurrect a file the user permanently removed.
    fn permanently_remove(&self, row: &PhotoRow, rlab: &Path) -> Result<()> {
        if rlab.exists() {
            std::fs::remove_file(rlab).with_context(|| format!("remove {}", rlab.display()))?;
        }
        let thumb = self.thumb_path(&row.hash);
        if thumb.exists() {
            std::fs::remove_file(&thumb).ok();
        }
        self.db.delete_photo(row.id)?;
        Ok(())
    }

    /// Write new metadata to the photo's `.rlab` first, then to the index.
    ///
    /// The `.rlab` is the record; the database is a cache of it that
    /// [`Library::rebuild_index`] can regenerate.  Writing the file first means
    /// a failure or a crash costs at worst a stale index row — the edit itself
    /// is already durable, and a rebuild recovers it.  The other order loses
    /// the edit outright the next time the index is rebuilt.
    ///
    /// `lmta.collections` is ignored: membership is owned by
    /// [`Library::add_to_collection`] and its mirror, and the file's own list
    /// is kept.
    pub fn update_metadata(&self, photo_id: PhotoId, lmta: LibraryMeta) -> Result<()> {
        self.rewrite_lmta_in_file(photo_id, &lmta)?;
        self.db.update_lmta(photo_id, &lmta)
    }

    /// Same ordering as [`Library::update_metadata`], with the index caught up
    /// in one transaction once the files are written.  A photo whose file could
    /// not be rewritten is left out of that transaction and reported, so a
    /// rating applied to forty photos never ends up recorded for a file that
    /// does not carry it.
    pub fn update_metadata_batch(&self, updates: &[(PhotoId, LibraryMeta)]) -> Result<()> {
        let mut written: Vec<(PhotoId, LibraryMeta)> = Vec::with_capacity(updates.len());
        let mut failed: Vec<(PhotoId, anyhow::Error)> = Vec::new();
        for (id, lmta) in updates {
            match self.rewrite_lmta_in_file(*id, lmta) {
                Ok(()) => written.push((*id, lmta.clone())),
                Err(e) => failed.push((*id, e)),
            }
        }

        self.db.update_lmta_batch(&written)?;
        report_partial("update metadata", failed)
    }

    /// Mark a photo protected (or not). A protected photo cannot be deleted via
    /// [`Library::delete_photo`]. The flag is recorded both in the DB and in
    /// the file's `LMTA` chunk so it survives a rebuild.
    pub fn set_protected(&self, photo_id: PhotoId, protected: bool) -> Result<()> {
        let photos = self.db.all_photos(SortOrder::default())?;
        let Some(row) = photos.iter().find(|r| r.id == photo_id) else {
            bail!("photo {photo_id} not found");
        };
        let rlab_path = self.rlab_path(&row.hash);
        let _write_guard = self.lock_project_writes()?;
        if rlab_path.exists() {
            let mut rlab = RlabFile::read(&rlab_path)?;
            if let Some(ref mut lmta) = rlab.lmta {
                lmta.protected = protected;
            }
            rlab.meta = rlab.meta.touch();
            rlab.write_v5(&rlab_path)
                .context("rewrite lmta for protect")?;
        }
        self.db.set_protected(photo_id, protected)
    }

    // ── Sessions ──────────────────────────────────────────────────────────

    pub fn all_sessions(&self) -> Result<Vec<ImportSessionRow>> {
        self.db.all_sessions()
    }

    pub fn photos_in_session(&self, session_id: &str) -> Result<Vec<PhotoRow>> {
        self.db.photos_by_session(session_id)
    }

    /// Rename a session (DB only — no `.rlab` files touched).
    pub fn rename_session(&self, session_id: &str, name: &str) -> Result<()> {
        self.db.rename_session(session_id, name)
    }

    // ── Collections ───────────────────────────────────────────────────────

    pub fn create_collection(&self, name: &str) -> Result<CollectionRow> {
        let now = unix_now();
        // Minted here rather than by the index: the uuid goes into every
        // member file, and must survive the index being thrown away and
        // rebuilt, which reassigns row ids.
        let uuid = Uuid::new_v4().to_string();
        let id = self.db.create_collection(&uuid, name, now)?;
        Ok(CollectionRow {
            id,
            uuid,
            name: name.to_owned(),
            created_at: now,
        })
    }

    /// Rename a collection.  One index row, whatever its size.
    ///
    /// Member files record the collection's uuid and carry its name only as a
    /// hint for rebuilding an index that has been lost, so they are left as
    /// they are: rewriting a large collection's files would be minutes of
    /// verified writes, and an interruption would leave the rename half done.
    /// Those hints go stale until something else rewrites the file, which is
    /// what [`Library::rebuild_index`]'s newest-file-wins rule accounts for.
    pub fn rename_collection(&self, id: CollectionId, new_name: &str) -> Result<()> {
        self.db.rename_collection(id, new_name)
    }

    /// Delete a collection, taking it out of every member `.rlab` first.
    ///
    /// The files are what a rebuild believes, so a collection dropped from the
    /// index alone would come back the next time one ran.  This is the one
    /// collection operation whose cost is unavoidably per member: unlike a
    /// rename, there is nothing left in the index afterwards for the files to
    /// refer to.
    ///
    /// Members waiting in Recently Deleted are written too, for the same
    /// reason: their files record the collection just as firmly as an active
    /// photo's, and the photo is one restore away from carrying that record
    /// back into the library.
    pub fn delete_collection(&self, id: CollectionId) -> Result<()> {
        let members = self.db.collection_member_ids(id)?;
        self.remove_from_collection(id, &members)?;
        self.db.delete_collection(id)
    }

    /// Delete several collections, reporting progress and stopping when asked.
    ///
    /// Each one costs a rewrite of every member `.rlab`, so a handful of large
    /// collections is minutes of work on a network mount.  Every id is
    /// attempted even after one fails: the collections are independent, and a
    /// single unwritable photo should not strand the rest of the batch half
    /// done.  What failed is named in the outcome; what succeeded is gone.
    ///
    /// `cancel` is polled between collections rather than within one, since a
    /// collection is only half deleted until its last member has been
    /// rewritten.
    pub fn delete_collections(
        &self,
        ids: &[CollectionId],
        cancel: Arc<AtomicBool>,
        progress_cb: impl Fn(DeleteProgress),
    ) -> Result<DeleteOutcome> {
        // Named up front: once a collection is deleted the index can no longer
        // say what it was called, and an id is no use in an error message.
        let names: HashMap<CollectionId, String> = self
            .all_collections()?
            .into_iter()
            .map(|row| (row.id, row.name))
            .collect();
        Ok(bulk_op(ids, &cancel, progress_cb, |id| {
            let name = names
                .get(&id)
                .cloned()
                .unwrap_or_else(|| format!("collection {id}"));
            match self.delete_collection(id) {
                Ok(()) => Step::Done,
                Err(error) => Step::Failed {
                    item: name,
                    error: error.to_string(),
                },
            }
        }))
    }

    pub fn all_collections(&self) -> Result<Vec<CollectionRow>> {
        self.db.all_collections()
    }

    /// Every `(collection, photo)` pair in the library, in one query.
    ///
    /// The grid needs to know which collections a whole selection is already
    /// in; asking per collection would be a joined query each time the sidebar
    /// refreshes.
    pub fn collection_memberships(&self) -> Result<Vec<(CollectionId, PhotoId)>> {
        self.db.collection_memberships()
    }

    /// Add photos to a collection, recording membership in each `.rlab` before
    /// the index.  Only the photos whose file was actually rewritten are added
    /// to the index, so the two never disagree about a photo; the rest are
    /// reported as an error and simply stay out of the collection.
    ///
    /// Photos already in the collection are left alone, so adding a selection
    /// that partly overlaps it adds only what is missing.
    pub fn add_to_collection(
        &self,
        collection_id: CollectionId,
        photo_ids: &[PhotoId],
    ) -> Result<()> {
        self.set_collection_membership(collection_id, photo_ids, true)
    }

    /// Mirror of [`Library::add_to_collection`]: the `.rlab` files lose the
    /// collection first, and only those photos leave it in the index.
    pub fn remove_from_collection(
        &self,
        collection_id: CollectionId,
        photo_ids: &[PhotoId],
    ) -> Result<()> {
        self.set_collection_membership(collection_id, photo_ids, false)
    }

    /// Body of both directions: rewrite every `.rlab` first, then apply the
    /// same change to the index for the photos whose file was written.
    ///
    /// Photo ids the index doesn't know are skipped rather than added to the
    /// collection, which would leave a membership row pointing at nothing.
    fn set_collection_membership(
        &self,
        collection_id: CollectionId,
        photo_ids: &[PhotoId],
        member: bool,
    ) -> Result<()> {
        let collections = self.db.all_collections()?;
        let collection = collections
            .iter()
            .find(|row| row.id == collection_id)
            .with_context(|| format!("collection {collection_id} not found"))?;
        // One index read for the whole batch. Resolving each photo separately
        // walked every photo in the library per photo changed, which a grid
        // selection of a few hundred turns into a long stall.
        //
        // Recently Deleted is read as well: a photo waiting there still has a
        // file recording its collections, and dropping a collection has to
        // reach it or a restore would bring the collection back with it.
        let hashes: HashMap<PhotoId, String> = self
            .db
            .all_photos(SortOrder::default())?
            .into_iter()
            .chain(self.db.recently_deleted()?.into_iter().map(|row| row.photo))
            .map(|row| (row.id, row.hash))
            .collect();

        let mut written = Vec::with_capacity(photo_ids.len());
        let mut failed: Vec<(PhotoId, anyhow::Error)> = Vec::new();
        for &pid in photo_ids {
            let Some(hash) = hashes.get(&pid) else {
                continue;
            };
            match self.set_collection_in_file(hash, collection, &collections, member) {
                Ok(()) => written.push(pid),
                Err(e) => failed.push((pid, e)),
            }
        }

        if member {
            self.db.add_to_collection(collection_id, &written)?;
        } else {
            self.db.remove_from_collection(collection_id, &written)?;
        }
        let what = if member {
            "add to collection"
        } else {
            "remove from collection"
        };
        report_partial(what, failed)
    }

    pub fn collection_photos(&self, id: CollectionId) -> Result<Vec<PhotoRow>> {
        self.db.collection_photos(id)
    }

    // ── Stacks ────────────────────────────────────────────────────────────

    pub fn stack_photos(&self, stack_id: &str) -> Result<Vec<PhotoRow>> {
        self.db.photos_in_stack(stack_id)
    }

    // ── Maintenance ───────────────────────────────────────────────────────

    /// Bring the index back in line with the `.rlab` files on disk. `cancel` is
    /// polled between files so a rebuild over a large library can be stopped;
    /// [`reconstruct::rebuild`] documents what a stopped run leaves behind.
    pub fn rebuild_index(
        &self,
        cancel: Arc<AtomicBool>,
        progress_cb: impl Fn(RebuildProgress),
    ) -> Result<RebuildOutcome> {
        reconstruct::rebuild(
            &self.root,
            self.db.as_ref(),
            &self.registry,
            cancel,
            &progress_cb,
        )
    }

    /// Re-render the pipeline for `hash` at 512px and write the new thumbnail.
    pub fn regenerate_thumbnail(&self, hash: &str) -> Result<()> {
        let _write_guard = self.lock_project_writes()?;
        let rlab_path = self.rlab_path(hash);
        let rlab = RlabFile::read(&rlab_path)?;
        let hint = rlab.meta.source_path.as_deref().map(Path::new);
        let source = self
            .registry
            .decode_bytes(&rlab.original_bytes, hint)
            .context("decode original for thumbnail")?;

        // Apply the active virtual copy's edit stack so the thumbnail reflects
        // saved edits.
        let active = rlab
            .active_copy_index
            .min(rlab.copies.len().saturating_sub(1));
        let pipeline_state = rlab
            .copies
            .get(active)
            .map(|c| c.pipeline_state.clone())
            .context("rlab has no virtual copies")?;
        let source_arc = Arc::new(source);
        let mut pipeline = EditPipeline::new_virtual_copy(Arc::clone(&source_arc));
        pipeline
            .load_state(pipeline_state)
            .map_err(|e| anyhow::anyhow!("load pipeline state: {e}"))?;
        let rendered = pipeline
            .render()
            .map_err(|e| anyhow::anyhow!("render pipeline: {e}"))?;
        let thumb = generate_thumbnail(&rendered, 512)?;

        // Also update PREV chunk in the .rlab
        let mut updated = rlab;
        updated.thumbnail = Some(thumb.clone());
        updated.write_v5(&rlab_path)?;

        self.update_thumbnail_cache(hash, &thumb, updated.has_edits())?;

        Ok(())
    }

    /// Select a virtual copy and rebuild its thumbnail with one container read
    /// and one container rewrite. The previous GUI path wrote the selection,
    /// then called [`Library::regenerate_thumbnail`], which repeated both.
    pub fn set_active_copy_and_regenerate_thumbnail(
        &self,
        hash: &str,
        copy_idx: usize,
    ) -> Result<Vec<u8>> {
        let _write_guard = self.lock_project_writes()?;
        let rlab_path = self.rlab_path(hash);
        let mut rlab = RlabFile::read(&rlab_path)?;
        rlab.active_copy_index = copy_idx.min(rlab.copies.len().saturating_sub(1));

        let hint = rlab.meta.source_path.as_deref().map(Path::new);
        let source = self
            .registry
            .decode_bytes(&rlab.original_bytes, hint)
            .context("decode original for thumbnail")?;
        let pipeline_state = rlab
            .copies
            .get(rlab.active_copy_index)
            .map(|copy| copy.pipeline_state.clone())
            .context("rlab has no virtual copies")?;
        // Any copy having edits is what makes the photo an edited one, so
        // selecting the untouched Copy 1 of a photo whose second copy is
        // edited must not take it out of the edited-only filter.
        let edited = rlab.has_edits();
        let source = Arc::new(source);
        let mut pipeline = EditPipeline::new_virtual_copy(Arc::clone(&source));
        pipeline
            .load_state(pipeline_state)
            .map_err(|e| anyhow::anyhow!("load pipeline state: {e}"))?;
        let rendered = pipeline
            .render()
            .map_err(|e| anyhow::anyhow!("render pipeline: {e}"))?;
        let thumb = generate_thumbnail(&rendered, 512)?;

        rlab.thumbnail = Some(thumb.clone());
        rlab.write_v5(&rlab_path)?;
        self.update_thumbnail_cache(hash, &thumb, edited)?;
        Ok(thumb)
    }

    /// Save an editor-produced project without allowing an older cached LMTA
    /// value to overwrite metadata changed in the library view. The selective
    /// reader transfers only small metadata chunks, and the lock keeps that
    /// refresh and the following authoritative rewrite indivisible relative to
    /// other in-process project mutations.
    pub fn save_edited_project(
        &self,
        hash: &str,
        mut project: RlabFile,
    ) -> Result<Option<LibraryMeta>> {
        let _write_guard = self.lock_project_writes()?;
        let rlab_path = self.rlab_path(hash);
        let current_lmta = read_library_summary(&rlab_path)?.lmta;
        project.set_lmta(current_lmta.clone());
        project.write_v5(&rlab_path)?;
        Ok(current_lmta)
    }

    /// Publish already-rendered thumbnail bytes to the rebuildable side cache
    /// and update the index without touching the authoritative `.rlab`.
    pub fn update_thumbnail_cache(
        &self,
        hash: &str,
        thumbnail: &[u8],
        has_edits: bool,
    ) -> Result<()> {
        write_thumbnail(&self.thumb_path(hash), thumbnail)?;

        // Mark the photo as edited in the DB.
        if let Ok(Some(row)) = self.db.photo_by_hash(hash) {
            let _ = self.db.set_has_edits(row.id, has_edits);
        }
        Ok(())
    }

    // ── Internal LMTA rewrite helpers ─────────────────────────────────────

    /// A photo row by id, from either side of Recently Deleted.
    ///
    /// A deleted photo is still one the user can select and edit, so the
    /// active-photo query alone would drop those edits on the floor.
    fn photo_row(&self, photo_id: PhotoId) -> Result<Option<PhotoRow>> {
        if let Some(row) = self
            .db
            .all_photos(SortOrder::default())?
            .into_iter()
            .find(|row| row.id == photo_id)
        {
            return Ok(Some(row));
        }
        Ok(self
            .db
            .recently_deleted()?
            .into_iter()
            .map(|row| row.photo)
            .find(|row| row.id == photo_id))
    }

    fn rewrite_lmta_in_file(&self, photo_id: PhotoId, lmta: &LibraryMeta) -> Result<()> {
        let Some(row) = self.photo_row(photo_id)? else {
            return Ok(());
        };
        // The guard has to be held across resolving the path and the existence
        // check as well as the rewrite. Checking first lets a delete move the
        // file out from under us, turning a photo that is simply gone — which
        // this skips — into a read error reported to the user as a failed
        // metadata write.
        let _write_guard = self.lock_project_writes()?;
        let rlab_path = self.photo_rlab_path(&row.hash);
        if !rlab_path.exists() {
            return Ok(());
        }
        let mut rlab = RlabFile::read(&rlab_path)?;
        let mut lmta = lmta.clone();
        // Collection membership is not the caller's to write: it belongs to
        // add/remove_from_collection, which puts it in the file and the index
        // together. A metadata editor holds the LMTA it read when its photo
        // was selected, which may pre-date a collection change, so the file's
        // own membership wins over whatever the caller last saw.
        if let Some(current) = rlab.lmta.as_ref() {
            lmta.collection_refs = current.collection_refs.clone();
            lmta.legacy_collections = current.legacy_collections.clone();
        }
        rlab.set_lmta(Some(lmta));
        rlab.meta = rlab.meta.touch();
        rlab.write_v5(&rlab_path).context("rewrite lmta")
    }

    /// Record — or drop — one collection in a photo's `.rlab`.
    ///
    /// A file that already says what it should is left untouched, unless it
    /// still carries pre-uuid membership that this write can migrate while it
    /// has the file open: adding a selection that overlaps the collection
    /// would otherwise rewrite those photos for no change.
    fn set_collection_in_file(
        &self,
        hash: &str,
        collection: &CollectionRow,
        all: &[CollectionRow],
        member: bool,
    ) -> Result<()> {
        // The guard has to be held across resolving the path and the existence
        // check as well as the rewrite. Checking first lets a delete move the
        // file out from under us, turning a photo that is simply gone — which
        // this skips — into a read error reported to the user as a failed
        // metadata write.
        let _write_guard = self.lock_project_writes()?;
        let rlab_path = self.photo_rlab_path(hash);
        if !rlab_path.exists() {
            return Ok(());
        }
        let mut rlab = RlabFile::read(&rlab_path)?;
        let Some(ref mut lmta) = rlab.lmta else {
            // Nothing in the file records membership, so there is nothing to
            // write; the index still gets the change.
            return Ok(());
        };
        let migrated = migrate_legacy_collections(lmta, all);
        let listed = lmta
            .collection_refs
            .iter()
            .any(|held| held.id == collection.uuid);
        if listed == member && !migrated {
            return Ok(());
        }
        if member {
            lmta.collection_refs.push(CollectionRef {
                id: collection.uuid.clone(),
                name: collection.name.clone(),
            });
        } else {
            lmta.collection_refs
                .retain(|held| held.id != collection.uuid);
        }
        rlab.meta = rlab.meta.touch();
        rlab.write_v5(&rlab_path)?;
        Ok(())
    }
}

/// Turn any pre-uuid membership in `lmta` into proper refs, reporting whether
/// anything changed.
///
/// Done opportunistically, whenever a file is open for a membership change
/// anyway, so a library converts as it is used instead of needing a pass of its
/// own.  A name the index has never heard of is left in the legacy list rather
/// than dropped: it means the index is the incomplete one, and a rebuild can
/// still recover the membership from it.
fn migrate_legacy_collections(lmta: &mut LibraryMeta, all: &[CollectionRow]) -> bool {
    if lmta.legacy_collections.is_empty() {
        return false;
    }
    let mut migrated = false;
    lmta.legacy_collections.retain(|name| {
        let Some(known) = all.iter().find(|row| &row.name == name) else {
            return true;
        };
        migrated = true;
        if !lmta
            .collection_refs
            .iter()
            .any(|held| held.id == known.uuid)
        {
            lmta.collection_refs.push(CollectionRef {
                id: known.uuid.clone(),
                name: known.name.clone(),
            });
        }
        false
    });
    migrated
}

// ── Bulk operation helpers ───────────────────────────────────────────────────

/// A cancellation flag that is never raised, for the single-photo entry points
/// that have nothing to cancel.
fn idle_cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// What to call a photo in a message to the user.
///
/// A `.rlab` is named by content hash, so the only name a photographer
/// recognises is the one the file was imported under.
fn photo_label(row: &PhotoRow) -> String {
    row.original_filename
        .clone()
        .unwrap_or_else(|| row.hash.clone())
}

/// Index rows by id so a bulk operation reads the photo table once rather than
/// once per photo — the difference between a selection of five hundred costing
/// one query and costing five hundred full scans of the library.
fn photo_index(rows: Vec<PhotoRow>) -> HashMap<PhotoId, PhotoRow> {
    rows.into_iter().map(|row| (row.id, row)).collect()
}

/// Run `step` over `items`, keeping the tally and letting the caller out.
///
/// The bulk operations differ only in what they do to one item; the
/// bookkeeping around it is the same for all of them, and so is the reason it
/// exists. Each item is a file rename, unlink, or rewrite, which on a network
/// mount can take long enough that a batch of a few hundred is minutes of
/// work — hence a progress report before every item and a cancellation check
/// that does not wait for the current one to finish.
fn bulk_op<T: Copy>(
    items: &[T],
    cancel: &AtomicBool,
    progress_cb: impl Fn(DeleteProgress),
    mut step: impl FnMut(T) -> Step,
) -> DeleteOutcome {
    let mut progress = DeleteProgress {
        total: items.len(),
        ..Default::default()
    };
    let mut done = 0usize;
    let mut cancelled = false;

    for &id in items {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }
        progress_cb(progress.clone());
        match step(id) {
            Step::Done => done += 1,
            Step::Protected(name) => progress.protected.push(name),
            Step::Failed { item, error } => {
                eprintln!("library: {item}: {error}");
                progress.errors.push((item, error));
            }
        }
        progress.done += 1;
    }
    progress_cb(progress.clone());

    DeleteOutcome {
        done,
        protected: progress.protected,
        errors: progress.errors,
        purged: Vec::new(),
        cancelled,
    }
}

/// Reduce a one-photo bulk run to the plain success-or-failure the
/// single-photo entry points promise.
fn single_photo_result(outcome: &DeleteOutcome) -> Result<()> {
    if let Some(name) = outcome.protected.first() {
        bail!("\"{name}\" is protected and cannot be deleted");
    }
    if let Some((photo, error)) = outcome.errors.first() {
        bail!("{photo}: {error}");
    }
    Ok(())
}

// ── File-level helpers ────────────────────────────────────────────────────────

/// Rename `source` to `destination` within the library. Returns `true` when a
/// rename occurred and `false` when it had already happened, making the file
/// step safe to retry after an interrupted operation.
fn move_library_file(source: &Path, destination: &Path) -> Result<bool> {
    if source.exists() {
        if destination.exists() {
            bail!(
                "cannot move {}: destination {} already exists",
                source.display(),
                destination.display()
            );
        }
        if let Some(parent) = destination.parent() {
            create_dir_all_synced(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        // Synced: the index row is updated on the strength of this rename
        // having happened, so the name has to outlive a power cut too.
        rename_synced(source, destination)
            .with_context(|| format!("move {} to {}", source.display(), destination.display()))?;
        return Ok(true);
    }
    if destination.exists() {
        return Ok(false);
    }
    bail!("photo file is missing: {}", source.display())
}

/// Turn per-photo file failures into one error, raised only after the index has
/// been brought in line with the files that *were* written.  Reporting before
/// that would leave the index describing files that never changed.
fn report_partial(what: &str, mut failed: Vec<(PhotoId, anyhow::Error)>) -> Result<()> {
    if failed.is_empty() {
        return Ok(());
    }
    let count = failed.len();
    let (photo_id, first) = failed.swap_remove(0);
    Err(first.context(format!(
        "{what}: {count} photo(s) could not be written, starting with photo {photo_id}"
    )))
}

fn collect_image_paths(folder: &Path, registry: &FormatRegistry) -> Vec<PathBuf> {
    let exts: std::collections::HashSet<String> =
        registry.supported_extensions().into_iter().collect();

    walkdir::WalkDir::new(folder)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| exts.contains(&x.to_lowercase()))
                .unwrap_or(false)
        })
        .map(|e| e.into_path())
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
