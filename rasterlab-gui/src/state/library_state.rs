use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use rasterlab_library::{
    ImportProgress, Library, PhotoId, PhotoRow, SearchFilter, SortOrder,
    CollectionId, CollectionRow, ImportSessionRow,
};

// ── LibraryView ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryView {
    AllPhotos,
    Session(String),
    Collection(CollectionId),
}

impl Default for LibraryView {
    fn default() -> Self {
        Self::AllPhotos
    }
}

// ── LibraryState ──────────────────────────────────────────────────────────────

pub struct LibraryState {
    pub library:         Option<Arc<Library>>,
    pub view:            LibraryView,
    pub filter:          SearchFilter,
    pub sort:            SortOrder,
    pub results:         Vec<PhotoRow>,
    pub selected:        Vec<PhotoId>,
    pub thumb_scale:     f32,
    pub import_progress: Option<ImportProgress>,
    pub scroll_offset:   f32,
    pub expanded_stacks: HashSet<String>,

    // Thumbnail cache: hash → egui texture handle
    pub thumb_cache:     HashMap<String, egui::TextureHandle>,
    /// Hashes for which a bg load has already been requested (to avoid dupes).
    pub thumb_requested: HashSet<String>,

    // Sidebar state
    pub sessions:        Vec<ImportSessionRow>,
    pub collections:     Vec<CollectionRow>,

    /// Error message to show in a status bar or dialog.
    pub last_error:      Option<String>,
}

impl Default for LibraryState {
    fn default() -> Self {
        Self {
            library:         None,
            view:            LibraryView::default(),
            filter:          SearchFilter::default(),
            sort:            SortOrder::ImportDateDesc,
            results:         Vec::new(),
            selected:        Vec::new(),
            thumb_scale:     0.5,
            import_progress: None,
            scroll_offset:   0.0,
            expanded_stacks: HashSet::new(),
            thumb_cache:     HashMap::new(),
            thumb_requested: HashSet::new(),
            sessions:        Vec::new(),
            collections:     Vec::new(),
            last_error:      None,
        }
    }
}

impl LibraryState {
    /// Reload results from the DB based on the current view + filter + sort.
    pub fn refresh(&mut self) {
        let Some(lib) = &self.library else { return };

        let photos = match &self.view {
            LibraryView::AllPhotos => {
                if self.filter.is_empty() {
                    lib.all_photos(self.sort).ok()
                } else {
                    lib.search(&self.filter, self.sort).ok()
                }
            }
            LibraryView::Session(id) => lib.photos_in_session(id).ok(),
            LibraryView::Collection(id) => lib.collection_photos(*id).ok(),
        };

        if let Some(photos) = photos {
            self.results = photos;
        }

        // Refresh sidebar lists
        self.sessions    = lib.all_sessions().unwrap_or_default();
        self.collections = lib.all_collections().unwrap_or_default();
    }

    pub fn open_library(&mut self, path: PathBuf, thumb_scale: f32) {
        match Library::open_or_create(&path) {
            Ok(lib) => {
                self.library         = Some(Arc::new(lib));
                self.thumb_scale     = thumb_scale;
                self.view            = LibraryView::AllPhotos;
                self.filter          = SearchFilter::default();
                self.selected        .clear();
                self.thumb_cache     .clear();
                self.thumb_requested .clear();
                self.last_error      = None;
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
}
