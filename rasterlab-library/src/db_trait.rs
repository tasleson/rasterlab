use rasterlab_core::library_meta::LibraryMeta;

use crate::search::SearchFilter;

pub type PhotoId = i64;
pub type CollectionId = i64;

// ── Row types returned by the DB ──────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct PhotoRow {
    pub id: PhotoId,
    /// Blake3 hex of the original file bytes.
    pub hash: String,
    /// Relative path inside `files/`: e.g. `"ab/cd/abc123….rlab"`.
    pub lib_path: String,
    pub width: u32,
    pub height: u32,
    pub import_date: u64,
    pub import_session: String,
    pub capture_date: Option<String>,
    pub original_filename: Option<String>,
    /// UUID shared by all files in a RAW+JPEG stack.
    pub stack_id: Option<String>,
    pub stack_is_primary: bool,
    /// True if the photo has at least one committed edit (non-empty op stack).
    pub has_edits: bool,
}

#[derive(Debug, Clone)]
pub struct ImportSessionRow {
    pub id: String,
    pub name: String,
    pub started_at: u64,
    pub source_dir: Option<String>,
    pub photo_count: i64,
}

#[derive(Debug, Clone)]
pub struct CollectionRow {
    pub id: CollectionId,
    pub name: String,
    pub created_at: u64,
}

// ── Sort order ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortOrder {
    #[default]
    CaptureDateDesc,
    CaptureDateAsc,
    ImportDateDesc,
    RatingDesc,
    FilenameAsc,
}

// ── LibraryDb trait ───────────────────────────────────────────────────────────

/// All library logic talks to this trait. Only [`crate::stoolap_db::StoolapDb`]
/// (and test fakes) implement it, keeping the concrete DB dependency isolated.
pub trait LibraryDb: Send + Sync {
    /// Create tables and indexes if they don't exist.
    fn init(&self) -> anyhow::Result<()>;

    // ── Photos ────────────────────────────────────────────────────────────

    fn insert_photo(
        &self,
        hash: &str,
        lib_path: &str,
        lmta: &LibraryMeta,
        width: u32,
        height: u32,
        stack_id: Option<&str>,
    ) -> anyhow::Result<PhotoId>;

    fn photo_by_hash(&self, hash: &str) -> anyhow::Result<Option<PhotoRow>>;

    fn update_lmta(&self, photo_id: PhotoId, lmta: &LibraryMeta) -> anyhow::Result<()>;

    fn set_has_edits(&self, photo_id: PhotoId, has_edits: bool) -> anyhow::Result<()>;

    fn update_lmta_batch(&self, updates: &[(PhotoId, LibraryMeta)]) -> anyhow::Result<()>;

    fn delete_photo(&self, photo_id: PhotoId) -> anyhow::Result<()>;

    fn all_photos(&self, sort: SortOrder) -> anyhow::Result<Vec<PhotoRow>>;

    // ── Search ────────────────────────────────────────────────────────────

    fn search(&self, filter: &SearchFilter, sort: SortOrder) -> anyhow::Result<Vec<PhotoRow>>;

    fn photos_by_session(&self, session_id: &str) -> anyhow::Result<Vec<PhotoRow>>;

    fn collection_photos(&self, collection_id: CollectionId) -> anyhow::Result<Vec<PhotoRow>>;

    // ── Import sessions ───────────────────────────────────────────────────

    fn insert_session(
        &self,
        id: &str,
        name: &str,
        started_at: u64,
        source_dir: Option<&str>,
    ) -> anyhow::Result<()>;

    fn rename_session(&self, id: &str, name: &str) -> anyhow::Result<()>;

    fn update_session_count(&self, id: &str, count: i64) -> anyhow::Result<()>;

    fn all_sessions(&self) -> anyhow::Result<Vec<ImportSessionRow>>;

    // ── Stacks ────────────────────────────────────────────────────────────

    fn photos_in_stack(&self, stack_id: &str) -> anyhow::Result<Vec<PhotoRow>>;

    // ── Collections ───────────────────────────────────────────────────────

    fn create_collection(&self, name: &str, created_at: u64) -> anyhow::Result<CollectionId>;

    fn rename_collection(&self, id: CollectionId, name: &str) -> anyhow::Result<()>;

    fn delete_collection(&self, id: CollectionId) -> anyhow::Result<()>;

    fn all_collections(&self) -> anyhow::Result<Vec<CollectionRow>>;

    fn add_to_collection(
        &self,
        collection_id: CollectionId,
        photo_ids: &[PhotoId],
    ) -> anyhow::Result<()>;

    fn remove_from_collection(
        &self,
        collection_id: CollectionId,
        photo_ids: &[PhotoId],
    ) -> anyhow::Result<()>;

    // ── Bulk rebuild ──────────────────────────────────────────────────────

    /// Drop all rows from every table. Used by `rebuild_index`.
    fn clear_all(&self) -> anyhow::Result<()>;
}
