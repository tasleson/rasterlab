use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, atomic::AtomicBool},
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
    import::{self, ImportSession},
    reconstruct::{self, RebuildProgress},
    search::SearchFilter,
    stoolap_db::StoolapDb,
    thumbnail::{generate_thumbnail, write_thumbnail},
};

// ── Public types ──────────────────────────────────────────────────────────────

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
        let paths = collect_image_paths(folder, &self.registry);
        let cancelled = Arc::new(AtomicBool::new(false));
        import::import_folder_grouped(
            &self.root,
            self.db.as_ref(),
            &self.registry,
            &paths,
            cancelled,
            Some(folder),
            &progress_cb,
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

    /// Move a photo into this library's Recently Deleted area.
    ///
    /// The `.rlab` is renamed within the library filesystem, which is fast and
    /// recoverable on local disks and network mounts alike. Its index metadata
    /// and thumbnail remain available so the photo can be restored exactly.
    pub fn delete_photo(&self, photo_id: PhotoId) -> Result<()> {
        let photos = self.db.all_photos(SortOrder::default())?;
        let Some(row) = photos.iter().find(|r| r.id == photo_id) else {
            bail!("photo {photo_id} not found");
        };
        if row.protected {
            let name = row.original_filename.as_deref().unwrap_or("photo");
            bail!("\"{name}\" is protected and cannot be deleted");
        }

        let _write_guard = self.lock_project_writes()?;
        let active = self.rlab_path(&row.hash);
        let deleted = self.recently_deleted_path(&row.hash);
        let moved = move_library_file(&active, &deleted)?;
        if let Err(error) = self.db.mark_photo_deleted(photo_id, unix_now()) {
            if moved {
                let _ = move_library_file(&deleted, &active);
            }
            return Err(error.context("record Recently Deleted state"));
        }
        Ok(())
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

    pub fn restore_photo(&self, photo_id: PhotoId) -> Result<()> {
        let deleted_rows = self.db.recently_deleted()?;
        let Some(row) = deleted_rows.iter().find(|r| r.photo.id == photo_id) else {
            bail!("recently deleted photo {photo_id} not found");
        };
        let _write_guard = self.lock_project_writes()?;
        let deleted = self.recently_deleted_path(&row.photo.hash);
        let active = self.rlab_path(&row.photo.hash);
        let moved = move_library_file(&deleted, &active)?;
        if let Err(error) = self.db.restore_photo(photo_id) {
            if moved {
                let _ = move_library_file(&active, &deleted);
            }
            return Err(error.context("restore photo index state"));
        }
        Ok(())
    }

    pub fn delete_recently_deleted_permanently(&self, photo_id: PhotoId) -> Result<()> {
        let deleted_rows = self.db.recently_deleted()?;
        let Some(row) = deleted_rows.iter().find(|r| r.photo.id == photo_id) else {
            bail!("recently deleted photo {photo_id} not found");
        };
        let _write_guard = self.lock_project_writes()?;
        self.permanently_remove(&row.photo, &self.recently_deleted_path(&row.photo.hash))?;
        self.db.delete_empty_sessions()?;
        Ok(())
    }

    pub fn empty_recently_deleted(&self) -> Result<usize> {
        let rows = self.db.recently_deleted()?;
        let _write_guard = self.lock_project_writes()?;
        for row in &rows {
            self.permanently_remove(&row.photo, &self.recently_deleted_path(&row.photo.hash))?;
        }
        self.db.delete_empty_sessions()?;
        Ok(rows.len())
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
    pub fn delete_collection(&self, id: CollectionId) -> Result<()> {
        let members: Vec<PhotoId> = self
            .db
            .collection_photos(id)?
            .into_iter()
            .map(|row| row.id)
            .collect();
        self.remove_from_collection(id, &members)?;
        self.db.delete_collection(id)
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
        let hashes: HashMap<PhotoId, String> = self
            .db
            .all_photos(SortOrder::default())?
            .into_iter()
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

    pub fn rebuild_index(&self, progress_cb: impl Fn(RebuildProgress)) -> Result<()> {
        reconstruct::rebuild(&self.root, self.db.as_ref(), &self.registry, &progress_cb)
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

    fn rewrite_lmta_in_file(&self, photo_id: PhotoId, lmta: &LibraryMeta) -> Result<()> {
        let photos = self.db.all_photos(SortOrder::default())?;
        let Some(row) = photos.iter().find(|r| r.id == photo_id) else {
            return Ok(());
        };
        let rlab_path = self.rlab_path(&row.hash);
        // The guard has to be held across the existence check as well as the
        // rewrite. Checking first lets a delete move the file out from under
        // us, turning a photo that is simply gone — which this skips — into a
        // read error reported to the user as a failed metadata write.
        let _write_guard = self.lock_project_writes()?;
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
        let rlab_path = self.rlab_path(hash);
        // The guard has to be held across the existence check as well as the
        // rewrite. Checking first lets a delete move the file out from under
        // us, turning a photo that is simply gone — which this skips — into a
        // read error reported to the user as a failed metadata write.
        let _write_guard = self.lock_project_writes()?;
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
