use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
    sync::Arc,
};

use rasterlab_library::{
    CollectionId, CollectionRow, ImportProgress, ImportSessionRow, Library, PhotoId, PhotoRow,
    SearchFilter, SortOrder,
};

// ── LibraryView ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LibraryView {
    #[default]
    AllPhotos,
    Session(String),
    Collection(CollectionId),
}

// ── LibraryState ──────────────────────────────────────────────────────────────

pub struct LibraryState {
    pub library: Option<Arc<Library>>,
    pub view: LibraryView,
    pub filter: SearchFilter,
    pub sort: SortOrder,
    pub results: Vec<PhotoRow>,
    pub selected: Vec<PhotoId>,
    pub thumb_scale: f32,
    pub import_progress: Option<ImportProgress>,

    /// Bounded thumbnail texture cache (evicts oldest beyond a fixed cap so a
    /// large library can't grow GPU/texture memory without limit).
    pub thumbs: ThumbCache,

    // Sidebar state
    pub sessions: Vec<ImportSessionRow>,
    pub collections: Vec<CollectionRow>,

    /// Error message to show in a status bar or dialog.
    pub last_error: Option<String>,

    /// When true, show the "Move to Trash?" confirmation dialog.
    pub confirm_delete: bool,

    // Raw text for exact-value filter inputs (persists across frames)
    pub iso_exact_text: String,
    pub aperture_exact_text: String,
    pub shutter_exact_text: String,

    // Per-field validation errors; `Some` means the input is out of the
    // reasonable domain and the filter for that field is not applied.
    pub iso_error: Option<String>,
    pub aperture_error: Option<String>,
    pub shutter_error: Option<String>,

    /// Set by the thumbnail grid when the user double-clicks a photo, so
    /// `app.rs` can route the open through the unsaved-changes confirmation
    /// dialog. Tuple is `(rlab_path, library_root, photo_hash)`.
    pub pending_open_photo: Option<(PathBuf, PathBuf, String)>,

    /// When set, the grid will scroll to — and select — the photo with this
    /// hash on the next frame, then clear the field.
    pub scroll_to_hash: Option<String>,
}

impl Default for LibraryState {
    fn default() -> Self {
        Self {
            library: None,
            view: LibraryView::default(),
            filter: SearchFilter::default(),
            sort: SortOrder::CaptureDateDesc,
            results: Vec::new(),
            selected: Vec::new(),
            thumb_scale: 0.5,
            import_progress: None,
            thumbs: ThumbCache::new(THUMB_CACHE_CAP),
            sessions: Vec::new(),
            collections: Vec::new(),
            last_error: None,
            confirm_delete: false,
            iso_exact_text: String::new(),
            aperture_exact_text: String::new(),
            shutter_exact_text: String::new(),
            iso_error: None,
            aperture_error: None,
            shutter_error: None,
            pending_open_photo: None,
            scroll_to_hash: None,
        }
    }
}

impl LibraryState {
    /// Reload results from the DB based on the current view + filter + sort.
    pub fn refresh(&mut self) {
        let Some(lib) = &self.library else { return };

        // Compose the view scope into a copy of the filter so that
        // session/collection views also honor shutter/ISO/aperture/etc.
        let mut filter = self.filter.clone();
        match &self.view {
            LibraryView::AllPhotos => {}
            LibraryView::Session(id) => filter.import_session = Some(id.clone()),
            LibraryView::Collection(id) => filter.collection_id = Some(*id),
        }

        let photos = if filter.is_empty() {
            lib.all_photos(self.sort).ok()
        } else {
            lib.search(&filter, self.sort).ok()
        };

        if let Some(photos) = photos {
            self.results = photos;
        }

        // Refresh sidebar lists
        self.sessions = lib.all_sessions().unwrap_or_default();
        self.collections = lib.all_collections().unwrap_or_default();
    }

    pub fn open_library(&mut self, path: PathBuf, thumb_scale: f32) {
        match Library::open_or_create(&path) {
            Ok(lib) => {
                self.library = Some(Arc::new(lib));
                self.thumb_scale = thumb_scale;
                self.view = LibraryView::AllPhotos;
                self.filter = SearchFilter::default();
                self.iso_exact_text.clear();
                self.aperture_exact_text.clear();
                self.shutter_exact_text.clear();
                self.iso_error = None;
                self.aperture_error = None;
                self.shutter_error = None;
                self.selected.clear();
                self.thumbs.clear();
                self.last_error = None;
                self.refresh();
            }
            Err(e) => {
                self.last_error = Some(format!("Failed to open library: {e}"));
            }
        }
    }

    pub fn is_selected(&self, id: PhotoId) -> bool {
        self.selected.contains(&id)
    }

    pub fn toggle_select(&mut self, id: PhotoId) {
        if let Some(pos) = self.selected.iter().position(|&x| x == id) {
            self.selected.remove(pos);
        } else {
            self.selected.push(id);
        }
    }

    pub fn select_only(&mut self, id: PhotoId) {
        self.selected.clear();
        self.selected.push(id);
    }

    pub fn select_none(&mut self) {
        self.selected.clear();
    }

    /// Move all selected photos to the OS trash and remove them from the library.
    pub fn delete_selected(&mut self) {
        let Some(lib) = &self.library else { return };
        // Collect hashes of selected photos so we can evict them from the cache.
        let hashes: Vec<String> = self
            .results
            .iter()
            .filter(|r| self.selected.contains(&r.id))
            .map(|r| r.hash.clone())
            .collect();

        for id in self.selected.clone() {
            if let Err(e) = lib.delete_photo(id) {
                self.last_error = Some(format!("Delete failed: {e}"));
                return;
            }
        }

        for hash in &hashes {
            self.thumbs.remove(hash);
        }
        self.selected.clear();
        self.refresh();
    }
}

// ── ThumbCache ──────────────────────────────────────────────────────────────

/// Maximum number of thumbnail textures kept resident. Each 512 px thumbnail is
/// ~1 MiB as an RGBA texture, so this caps thumbnail memory at a few hundred MiB
/// regardless of library size. With viewport-culled loading the working set is
/// far smaller; this is the safety ceiling for long scroll sessions.
const THUMB_CACHE_CAP: usize = 512;

/// Bounded thumbnail cache: hash → texture, plus the set of hashes whose load is
/// in flight (to dedupe requests). Insertion order is tracked so the oldest
/// entry is evicted once the cap is exceeded; an evicted hash is also dropped
/// from the requested set so it can be reloaded when scrolled back into view.
#[derive(Default)]
pub struct ThumbCache {
    textures: HashMap<String, egui::TextureHandle>,
    requested: HashSet<String>,
    order: VecDeque<String>,
    cap: usize,
}

impl ThumbCache {
    fn new(cap: usize) -> Self {
        Self {
            cap: cap.max(1),
            ..Default::default()
        }
    }

    pub fn get(&self, hash: &str) -> Option<&egui::TextureHandle> {
        self.textures.get(hash)
    }

    /// Number of resident textures (for the loading diagnostic).
    pub fn cached_len(&self) -> usize {
        self.textures.len()
    }

    /// Number of loads requested but not yet resident.
    pub fn pending_len(&self) -> usize {
        self.requested.len().saturating_sub(self.textures.len())
    }

    pub fn is_requested(&self, hash: &str) -> bool {
        self.requested.contains(hash)
    }

    pub fn mark_requested(&mut self, hash: String) {
        self.requested.insert(hash);
    }

    /// Store a loaded texture, evicting the oldest entry if over capacity.
    pub fn insert(&mut self, hash: String, handle: egui::TextureHandle) {
        if self.textures.insert(hash.clone(), handle).is_none() {
            self.order.push_back(hash);
        }
        while self.textures.len() > self.cap {
            let Some(evict) = self.order.pop_front() else {
                break;
            };
            self.textures.remove(&evict);
            self.requested.remove(&evict);
        }
    }

    pub fn remove(&mut self, hash: &str) {
        self.textures.remove(hash);
        self.requested.remove(hash);
        self.order.retain(|h| h != hash);
    }

    pub fn clear(&mut self) {
        self.textures.clear();
        self.requested.clear();
        self.order.clear();
    }
}
