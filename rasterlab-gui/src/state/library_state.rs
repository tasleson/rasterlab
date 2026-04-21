use std::{
    collections::{HashMap, HashSet},
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

    // Thumbnail cache: hash → egui texture handle
    pub thumb_cache: HashMap<String, egui::TextureHandle>,
    /// Hashes for which a bg load has already been requested (to avoid dupes).
    pub thumb_requested: HashSet<String>,

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
}

impl Default for LibraryState {
    fn default() -> Self {
        Self {
            library: None,
            view: LibraryView::default(),
            filter: SearchFilter::default(),
            sort: SortOrder::ImportDateDesc,
            results: Vec::new(),
            selected: Vec::new(),
            thumb_scale: 0.5,
            import_progress: None,
            thumb_cache: HashMap::new(),
            thumb_requested: HashSet::new(),
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
                self.thumb_cache.clear();
                self.thumb_requested.clear();
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
            self.thumb_cache.remove(hash);
            self.thumb_requested.remove(hash);
        }
        self.selected.clear();
        self.refresh();
    }
}
