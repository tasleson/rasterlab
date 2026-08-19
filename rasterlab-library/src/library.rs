use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rasterlab_core::{
    formats::FormatRegistry, library_meta::LibraryMeta, pipeline::EditPipeline, project::RlabFile,
};

use crate::{
    db_trait::{
        CollectionId, CollectionRow, ImportSessionRow, LibraryDb, PhotoId, PhotoRow, SortOrder,
    },
    fs_lock,
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
}

enum DeleteStorageMode {
    SystemTrash,
    Permanent,
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
        std::fs::create_dir_all(path.join("files"))?;
        std::fs::create_dir_all(path.join("thumbs"))?;
        db.init()?;
        Ok(Self {
            root: path.to_path_buf(),
            db,
            registry: FormatRegistry::with_builtins(),
        })
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
    /// capture days.  Returns one [`ImportSession`] per group.
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
        crate::scrub::scrub(&self.root, cancel, &progress_cb)
    }

    // ── Photos ────────────────────────────────────────────────────────────

    pub fn all_photos(&self, sort: SortOrder) -> Result<Vec<PhotoRow>> {
        self.db.all_photos(sort)
    }

    pub fn search(&self, filter: &SearchFilter, sort: SortOrder) -> Result<Vec<PhotoRow>> {
        self.db.search(filter, sort)
    }

    /// Move the `.rlab` to OS trash and remove the thumbnail + DB row.
    pub fn delete_photo(&self, photo_id: PhotoId) -> Result<()> {
        self.delete_photo_with_mode(photo_id, DeleteStorageMode::SystemTrash)
    }

    /// Permanently remove the `.rlab`, thumbnail, and DB row.
    ///
    /// This is intended for maintenance and headless test environments where
    /// the platform trash service may be unavailable or blocking.
    pub fn delete_photo_permanently(&self, photo_id: PhotoId) -> Result<()> {
        self.delete_photo_with_mode(photo_id, DeleteStorageMode::Permanent)
    }

    /// Storage first, index second, and every step tolerant of already having
    /// happened: an interrupted delete leaves a row pointing at a file that is
    /// gone, which reads as a photo that will not open — recoverable, and
    /// cleaned up by the next [`Library::rebuild_index`].  The other order
    /// would drop the row first and leave the file to be re-indexed by that
    /// same rebuild, resurrecting a photo the user deleted.
    fn delete_photo_with_mode(&self, photo_id: PhotoId, mode: DeleteStorageMode) -> Result<()> {
        // Find the hash so we can remove thumbnail
        let photos = self.db.all_photos(SortOrder::default())?;
        if let Some(row) = photos.iter().find(|r| r.id == photo_id) {
            if row.protected {
                let name = row.original_filename.as_deref().unwrap_or("photo");
                bail!("\"{name}\" is protected and cannot be deleted");
            }
            let rlab = self.rlab_path(&row.hash);
            let thumb = self.thumb_path(&row.hash);
            if rlab.exists() {
                match mode {
                    DeleteStorageMode::SystemTrash => {
                        trash::delete(&rlab)
                            .with_context(|| format!("trash {}", rlab.display()))?;
                    }
                    DeleteStorageMode::Permanent => {
                        std::fs::remove_file(&rlab)
                            .with_context(|| format!("remove {}", rlab.display()))?;
                    }
                }
            }
            if thumb.exists() {
                std::fs::remove_file(&thumb).ok();
            }
        }
        self.db.delete_photo(photo_id)
    }

    /// Write new metadata to the photo's `.rlab` first, then to the index.
    ///
    /// The `.rlab` is the record; the database is a cache of it that
    /// [`Library::rebuild_index`] can regenerate.  Writing the file first means
    /// a failure or a crash costs at worst a stale index row — the edit itself
    /// is already durable, and a rebuild recovers it.  The other order loses
    /// the edit outright the next time the index is rebuilt.
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
    /// [`Library::delete_photo`], and its `.rlab` is locked on the filesystem
    /// (best-effort, per-OS) so it cannot go missing. The flag is recorded both
    /// in the DB and in the file's `LMTA` chunk so it survives a rebuild.
    pub fn set_protected(&self, photo_id: PhotoId, protected: bool) -> Result<()> {
        let photos = self.db.all_photos(SortOrder::default())?;
        let Some(row) = photos.iter().find(|r| r.id == photo_id) else {
            bail!("photo {photo_id} not found");
        };
        let rlab_path = self.rlab_path(&row.hash);
        if rlab_path.exists() {
            // Keep the old lock in force if reading or rewriting fails. The
            // guard also covers panics, so every exit restores the prior state.
            fs_lock::with_unlocked(&rlab_path, || {
                let mut rlab = RlabFile::read(&rlab_path)?;
                if let Some(ref mut lmta) = rlab.lmta {
                    lmta.protected = protected;
                }
                rlab.meta = rlab.meta.touch();
                rlab.write_v5(&rlab_path)
                    .context("rewrite lmta for protect")
            })?;
            // Apply (or clear) the on-disk lock to match the new state.
            let _ = fs_lock::set_locked(&rlab_path, protected);
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
        let id = self.db.create_collection(name, now)?;
        Ok(CollectionRow {
            id,
            name: name.to_owned(),
            created_at: now,
        })
    }

    /// Rename a collection in every member `.rlab` first, then in the index.
    ///
    /// Membership lives in each file's `LMTA` chunk, so the files decide what
    /// the collection is called after a rebuild; renaming them first means an
    /// interruption leaves the new name already durable.  Files that could not
    /// be rewritten are reported once the index is updated.
    pub fn rename_collection(&self, id: CollectionId, new_name: &str) -> Result<()> {
        let old_name = self.collection_name(id)?;
        let photos = self.db.collection_photos(id)?;

        let mut failed: Vec<(PhotoId, anyhow::Error)> = Vec::new();
        for row in &photos {
            let rlab_path = self.rlab_path(&row.hash);
            if let Err(e) = rewrite_collection_name_in_file(&rlab_path, &old_name, new_name) {
                failed.push((row.id, e));
            }
        }

        self.db.rename_collection(id, new_name)?;
        report_partial("rename collection", failed)
    }

    pub fn delete_collection(&self, id: CollectionId) -> Result<()> {
        self.db.delete_collection(id)
    }

    pub fn all_collections(&self) -> Result<Vec<CollectionRow>> {
        self.db.all_collections()
    }

    /// Add photos to a collection, recording membership in each `.rlab` before
    /// the index.  Only the photos whose file was actually rewritten are added
    /// to the index, so the two never disagree about a photo; the rest are
    /// reported as an error and simply stay out of the collection.
    pub fn add_to_collection(
        &self,
        collection_id: CollectionId,
        photo_ids: &[PhotoId],
    ) -> Result<()> {
        let name = self.collection_name(collection_id)?;
        let mut written = Vec::with_capacity(photo_ids.len());
        let mut failed: Vec<(PhotoId, anyhow::Error)> = Vec::new();
        for &pid in photo_ids {
            match self.add_collection_to_file(pid, &name) {
                Ok(()) => written.push(pid),
                Err(e) => failed.push((pid, e)),
            }
        }

        self.db.add_to_collection(collection_id, &written)?;
        report_partial("add to collection", failed)
    }

    /// Mirror of [`Library::add_to_collection`]: the `.rlab` files lose the
    /// collection first, and only those photos leave it in the index.
    pub fn remove_from_collection(
        &self,
        collection_id: CollectionId,
        photo_ids: &[PhotoId],
    ) -> Result<()> {
        let name = self.collection_name(collection_id)?;
        let mut written = Vec::with_capacity(photo_ids.len());
        let mut failed: Vec<(PhotoId, anyhow::Error)> = Vec::new();
        for &pid in photo_ids {
            match self.remove_collection_from_file(pid, &name) {
                Ok(()) => written.push(pid),
                Err(e) => failed.push((pid, e)),
            }
        }

        self.db.remove_from_collection(collection_id, &written)?;
        report_partial("remove from collection", failed)
    }

    fn collection_name(&self, id: CollectionId) -> Result<String> {
        self.db
            .all_collections()?
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| c.name)
            .with_context(|| format!("collection {id} not found"))
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
        write_thumbnail(&self.thumb_path(hash), &thumb)?;

        // Also update PREV chunk in the .rlab
        let mut updated = rlab;
        updated.thumbnail = Some(thumb);
        fs_lock::with_unlocked(&rlab_path, || updated.write_v5(&rlab_path))?;

        // Mark the photo as edited in the DB.
        if let Ok(Some(row)) = self.db.photo_by_hash(hash) {
            let _ = self.db.set_has_edits(row.id, true);
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
        if !rlab_path.exists() {
            return Ok(());
        }
        let mut rlab = RlabFile::read(&rlab_path)?;
        rlab.set_lmta(Some(lmta.clone()));
        rlab.meta = rlab.meta.touch();
        fs_lock::with_unlocked(&rlab_path, || rlab.write_v5(&rlab_path)).context("rewrite lmta")
    }

    fn add_collection_to_file(&self, photo_id: PhotoId, collection_name: &str) -> Result<()> {
        let photos = self.db.all_photos(SortOrder::default())?;
        let Some(row) = photos.iter().find(|r| r.id == photo_id) else {
            return Ok(());
        };
        let rlab_path = self.rlab_path(&row.hash);
        if !rlab_path.exists() {
            return Ok(());
        }
        let mut rlab = RlabFile::read(&rlab_path)?;
        if let Some(ref mut lmta) = rlab.lmta
            && !lmta.collections.contains(&collection_name.to_owned())
        {
            lmta.collections.push(collection_name.to_owned());
        }
        rlab.meta = rlab.meta.touch();
        fs_lock::with_unlocked(&rlab_path, || rlab.write_v5(&rlab_path))?;
        Ok(())
    }

    fn remove_collection_from_file(&self, photo_id: PhotoId, collection_name: &str) -> Result<()> {
        let photos = self.db.all_photos(SortOrder::default())?;
        let Some(row) = photos.iter().find(|r| r.id == photo_id) else {
            return Ok(());
        };
        let rlab_path = self.rlab_path(&row.hash);
        if !rlab_path.exists() {
            return Ok(());
        }
        let mut rlab = RlabFile::read(&rlab_path)?;
        if let Some(ref mut lmta) = rlab.lmta {
            lmta.collections.retain(|c| c != collection_name);
        }
        rlab.meta = rlab.meta.touch();
        fs_lock::with_unlocked(&rlab_path, || rlab.write_v5(&rlab_path))?;
        Ok(())
    }
}

// ── File-level helpers ────────────────────────────────────────────────────────

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

fn rewrite_collection_name_in_file(rlab_path: &Path, old_name: &str, new_name: &str) -> Result<()> {
    let mut rlab = RlabFile::read(rlab_path)?;
    if let Some(ref mut lmta) = rlab.lmta {
        for name in &mut lmta.collections {
            if name == old_name {
                *name = new_name.to_owned();
            }
        }
    }
    rlab.meta = rlab.meta.touch();
    Ok(fs_lock::with_unlocked(rlab_path, || {
        rlab.write_v5(rlab_path)
    })?)
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
