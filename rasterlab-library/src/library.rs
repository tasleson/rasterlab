use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rasterlab_core::{
    formats::FormatRegistry, library_meta::LibraryMeta, pipeline::EditPipeline, project::RlabFile,
};

use crate::{
    db_trait::{
        CollectionId, CollectionRow, ImportSessionRow, LibraryDb, PhotoId, PhotoRow, SortOrder,
    },
    import::{self, ImportSession},
    reconstruct::{self, RebuildProgress},
    search::SearchFilter,
    stoolap_db::StoolapDb,
    thumbnail::generate_thumbnail,
};

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct ImportProgress {
    pub total: usize,
    pub done: usize,
    pub current_file: PathBuf,
    pub skipped_duplicates: usize,
    pub errors: Vec<(PathBuf, String)>,
}

// ── Library ───────────────────────────────────────────────────────────────────

pub struct Library {
    root: PathBuf,
    db: Box<dyn LibraryDb>,
    registry: FormatRegistry,
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

    /// Recursively import all supported images found under `folder`.
    pub fn import_folder(
        &self,
        folder: &Path,
        progress_cb: impl Fn(ImportProgress) + Send + 'static,
    ) -> Result<ImportSession> {
        let paths = collect_image_paths(folder, &self.registry);
        self.import_files(&paths, progress_cb)
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
        // Find the hash so we can remove thumbnail
        let photos = self.db.all_photos(SortOrder::default())?;
        if let Some(row) = photos.iter().find(|r| r.id == photo_id) {
            let rlab = self.rlab_path(&row.hash);
            let thumb = self.thumb_path(&row.hash);
            if rlab.exists() {
                trash::delete(&rlab).with_context(|| format!("trash {}", rlab.display()))?;
            }
            if thumb.exists() {
                std::fs::remove_file(&thumb).ok();
            }
        }
        self.db.delete_photo(photo_id)
    }

    pub fn update_metadata(&self, photo_id: PhotoId, lmta: LibraryMeta) -> Result<()> {
        // Update DB
        self.db.update_lmta(photo_id, &lmta)?;
        // Rewrite LMTA chunk in the .rlab file
        self.rewrite_lmta_in_file(photo_id, &lmta)
    }

    pub fn update_metadata_batch(&self, updates: &[(PhotoId, LibraryMeta)]) -> Result<()> {
        for (id, lmta) in updates {
            self.update_metadata(*id, lmta.clone())?;
        }
        Ok(())
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

    /// Rename a collection.  DB is updated immediately; all affected `.rlab`
    /// files are rewritten in the background (best-effort).
    pub fn rename_collection(&self, id: CollectionId, new_name: &str) -> Result<()> {
        // Fetch old name before rename
        let collections = self.db.all_collections()?;
        let old_name = collections
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone());

        self.db.rename_collection(id, new_name)?;

        // Rewrite LMTA in all affected .rlab files (best-effort background)
        if let Some(old) = old_name {
            let photos = self.db.collection_photos(id)?;
            for row in &photos {
                let rlab_path = self.rlab_path(&row.hash);
                rewrite_collection_name_in_file(&rlab_path, &old, new_name).ok();
            }
        }
        Ok(())
    }

    pub fn delete_collection(&self, id: CollectionId) -> Result<()> {
        self.db.delete_collection(id)
    }

    pub fn all_collections(&self) -> Result<Vec<CollectionRow>> {
        self.db.all_collections()
    }

    pub fn add_to_collection(
        &self,
        collection_id: CollectionId,
        photo_ids: &[PhotoId],
    ) -> Result<()> {
        self.db.add_to_collection(collection_id, photo_ids)?;
        // Update collection names in .rlab LMTA
        if let Ok(coll) = self.db.all_collections()
            && let Some(c) = coll.iter().find(|c| c.id == collection_id)
        {
            for pid in photo_ids {
                self.add_collection_to_file(*pid, &c.name).ok();
            }
        }
        Ok(())
    }

    pub fn remove_from_collection(
        &self,
        collection_id: CollectionId,
        photo_ids: &[PhotoId],
    ) -> Result<()> {
        self.db.remove_from_collection(collection_id, photo_ids)?;
        if let Ok(coll) = self.db.all_collections()
            && let Some(c) = coll.iter().find(|c| c.id == collection_id)
        {
            for pid in photo_ids {
                self.remove_collection_from_file(*pid, &c.name).ok();
            }
        }
        Ok(())
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
        let source = self
            .registry
            .decode_bytes(&rlab.original_bytes, None)
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
        let tpath = self.thumb_path(hash);
        if let Some(parent) = tpath.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&tpath, &thumb)?;

        // Also update PREV chunk in the .rlab
        let mut updated = rlab;
        updated.thumbnail = Some(thumb);
        updated.write(&rlab_path)?;

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
        rlab.write(&rlab_path).context("rewrite lmta")
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
        rlab.write(&rlab_path)?;
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
        rlab.write(&rlab_path)?;
        Ok(())
    }
}

// ── File-level helpers ────────────────────────────────────────────────────────

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
    Ok(rlab.write(rlab_path)?)
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
