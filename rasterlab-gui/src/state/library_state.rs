use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
};

use rasterlab_library::{
    CollectionId, CollectionRow, ImportProgress, ImportSessionRow, Library, PhotoId, PhotoRow,
    RebuildProgress, ScrubProgress, SearchFilter, SortOrder, import::rlab_path,
};

use crate::panels::tools::shared::MIN_STACK_FRAMES;

// ── LibraryView ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LibraryView {
    #[default]
    AllPhotos,
    Session(String),
    Collection(CollectionId),
}

// ── FocusStackRequest ─────────────────────────────────────────────────────────

/// A focus stack started from a multi-selection in the library grid.
///
/// The fused result has to live somewhere, so the first selected photo is
/// opened in the editor and hosts the operation; every selected photo — that
/// one included — becomes a source frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusStackRequest {
    /// `.rlab` path of the photo to open in the editor.
    pub base_rlab_path: PathBuf,
    /// Library root, for the editor's library context.
    pub library_root: PathBuf,
    /// Hash of the base photo, likewise for the library context.
    pub base_hash: String,
    /// `.rlab` path of every frame, in grid order.
    pub frame_paths: Vec<PathBuf>,
}

/// `(hash, .rlab path)` of every selected photo, in grid order.
///
/// Grid order rather than the click order held in `selected`, so the frame
/// list — and with it the photo that hosts the result — depends only on what
/// is selected, not on how the user got there.
fn selected_frames(
    results: &[PhotoRow],
    selected: &[PhotoId],
    root: &Path,
) -> Vec<(String, PathBuf)> {
    results
        .iter()
        .filter(|p| selected.contains(&p.id))
        .map(|p| (p.hash.clone(), rlab_path(root, &p.hash)))
        .collect()
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

    /// Per-file `(path, message)` failures from the most recent import. Retained
    /// after the import finishes so the user can review what went wrong.
    pub last_import_errors: Vec<(PathBuf, String)>,

    /// When true, show the import-errors detail window.
    pub show_import_errors: bool,

    /// Progress of a running integrity scrub, or `None` when idle.
    pub scrub_progress: Option<ScrubProgress>,

    /// Progress of a running index rebuild, or `None` when idle.
    pub rebuild_progress: Option<RebuildProgress>,

    /// When the running index rebuild started; used to estimate time left.
    /// Also serves as the "rebuild in flight" guard, since it is set before
    /// the first progress message arrives.
    pub rebuild_started: Option<std::time::Instant>,

    /// Per-file `(path, message)` uncorrectable failures from the most recent
    /// scrub. Retained after it finishes so the user can review them.
    pub last_scrub_errors: Vec<(PathBuf, String)>,

    /// When true, show the scrub-errors detail window.
    pub show_scrub_errors: bool,

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

    /// Set by the grid's context menu when the user asks to focus-stack the
    /// current selection. Routed through the same unsaved-changes dialog as
    /// `pending_open_photo`, since it too opens a photo in the editor.
    pub pending_focus_stack: Option<FocusStackRequest>,

    /// When set, the grid will scroll to — and select — the photo with this
    /// hash on the next frame, then clear the field.
    pub scroll_to_hash: Option<String>,

    /// Physical-pixel max side the resident thumbnail textures are currently
    /// built for. Tracked so the grid can detect a scale/DPI change and rebuild
    /// the cache at the new resolution. Zero until the first grid frame.
    pub thumb_target_side: u32,
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
            last_import_errors: Vec::new(),
            show_import_errors: false,
            scrub_progress: None,
            rebuild_progress: None,
            rebuild_started: None,
            last_scrub_errors: Vec::new(),
            show_scrub_errors: false,
            confirm_delete: false,
            iso_exact_text: String::new(),
            aperture_exact_text: String::new(),
            shutter_exact_text: String::new(),
            iso_error: None,
            aperture_error: None,
            shutter_error: None,
            pending_open_photo: None,
            pending_focus_stack: None,
            scroll_to_hash: None,
            thumb_target_side: 0,
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

    /// Human-readable one-liner for a running index rebuild, or `None` when
    /// idle. Includes files done / total, a time-left estimate once enough
    /// has elapsed for it to be meaningful, and a running error count.
    pub fn rebuild_status_text(&self) -> Option<String> {
        let p = self.rebuild_progress.as_ref()?;
        let mut s = format!("Rebuilding library index… {}/{}", p.done, p.total);
        if let Some(started) = self.rebuild_started
            && p.done > 0
            && p.done < p.total
        {
            let elapsed = started.elapsed().as_secs_f64();
            if elapsed > 2.0 {
                let remaining = elapsed / p.done as f64 * (p.total - p.done) as f64;
                s.push_str(&format!(", about {} left", format_duration(remaining)));
            }
        }
        if !p.errors.is_empty() {
            s.push_str(&format!(", {} error(s)", p.errors.len()));
        }
        Some(s)
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

    /// Move all selected photos to the OS trash and remove them from the
    /// library. Protected photos are skipped so a protected file can never be
    /// trashed, even if the confirmation dialog is bypassed.
    pub fn delete_selected(&mut self) {
        let Some(lib) = &self.library else { return };

        // Partition the selection into deletable and protected, capturing the
        // hashes of the deletable ones so we can evict their thumbnails.
        let mut deletable: Vec<(PhotoId, String)> = Vec::new();
        let mut protected = 0usize;
        for r in &self.results {
            if self.selected.contains(&r.id) {
                if r.protected {
                    protected += 1;
                } else {
                    deletable.push((r.id, r.hash.clone()));
                }
            }
        }

        for (id, _) in &deletable {
            if let Err(e) = lib.delete_photo(*id) {
                self.last_error = Some(format!("Delete failed: {e}"));
                return;
            }
        }

        for (_, hash) in &deletable {
            self.thumbs.remove(hash);
        }
        if protected > 0 {
            let noun = if protected == 1 { "photo" } else { "photos" };
            self.last_error = Some(format!("{protected} protected {noun} were not deleted."));
        }
        self.selected.clear();
        self.refresh();
    }

    /// Mark (or unmark) all selected photos as protected.
    pub fn set_protected_selected(&mut self, protected: bool) {
        let Some(lib) = &self.library else { return };
        for id in self.selected.clone() {
            if let Err(e) = lib.set_protected(id, protected) {
                self.last_error = Some(format!("Protect failed: {e}"));
                return;
            }
        }
        self.refresh();
    }

    /// Raise a focus-stack request for the current selection, to be picked up
    /// by `app.rs`. Does nothing unless a library is open and at least
    /// [`MIN_STACK_FRAMES`] photos are selected.
    pub fn request_focus_stack(&mut self) {
        let Some(lib) = &self.library else { return };
        let frames = selected_frames(&self.results, &self.selected, lib.root());
        if frames.len() < MIN_STACK_FRAMES {
            return;
        }
        let (base_hash, base_rlab_path) = frames[0].clone();
        self.pending_focus_stack = Some(FocusStackRequest {
            base_rlab_path,
            library_root: lib.root().to_path_buf(),
            base_hash,
            frame_paths: frames.into_iter().map(|(_, path)| path).collect(),
        });
    }

    /// True if every selected photo is currently protected (and there is at
    /// least one selection). Used to choose the Protect/Unprotect label.
    pub fn all_selected_protected(&self) -> bool {
        !self.selected.is_empty()
            && self
                .results
                .iter()
                .filter(|r| self.selected.contains(&r.id))
                .all(|r| r.protected)
    }
}

// ── Thumbnail texture sizing ────────────────────────────────────────────────

/// Max side of the JPEG thumbnails written to disk at import time
/// (`rasterlab_library::thumbnail::generate_thumbnail` is called with 512).
/// Resident textures are never built larger than this — there is no extra
/// detail to recover.
pub const THUMB_SOURCE_SIDE: u32 = 512;

/// Resident texture sizes are snapped to this multiple. Bucketing keeps the
/// cache homogeneous and stops a slow drag of the size slider from rebuilding
/// the textures on every one-pixel change.
const THUMB_SIZE_BUCKET: u32 = 64;

/// Physical-pixel max side a thumbnail texture should be built at for the given
/// grid scale and display DPI.
///
/// The grid draws each cell at `512 * thumb_scale` *points*; multiplying by
/// `pixels_per_point` gives the on-screen size in device pixels, which is the
/// most resolution that can actually be shown. The result is snapped to
/// [`THUMB_SIZE_BUCKET`] and clamped to [`THUMB_SOURCE_SIDE`] so we never
/// upscale past the on-disk thumbnail.
pub fn thumb_target_side(thumb_scale: f32, pixels_per_point: f32) -> u32 {
    let thumb_px = (512.0 * thumb_scale).max(64.0);
    let raw = (thumb_px * pixels_per_point).round() as u32;
    let bucketed = ((raw + THUMB_SIZE_BUCKET / 2) / THUMB_SIZE_BUCKET).max(1) * THUMB_SIZE_BUCKET;
    bucketed.min(THUMB_SOURCE_SIDE)
}

/// Round a duration in seconds to a coarse human-readable estimate
/// ("45s", "3m 20s", "1h 12m"). Coarse on purpose: it feeds "about … left"
/// strings where false precision reads worse than none.
fn format_duration(secs: f64) -> String {
    let secs = secs.round().max(0.0) as u64;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

// ── ThumbCache ──────────────────────────────────────────────────────────────

/// Initial cap for the resident texture cache, used until the grid sets a
/// scale-aware cap on its first frame (see [`ThumbCache::set_cap`]).
const THUMB_CACHE_CAP: usize = 256;

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

    /// Update the resident-texture cap and immediately evict down to it. Called
    /// each grid frame with a value derived from the viewport so the cache holds
    /// roughly the visible thumbnails plus a few screens of scroll margin.
    pub fn set_cap(&mut self, cap: usize) {
        self.cap = cap.max(1);
        self.trim();
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
        self.trim();
    }

    /// Evict oldest entries until the resident count is within `cap`.
    fn trim(&mut self) {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: PhotoId, hash: &str) -> PhotoRow {
        PhotoRow {
            id,
            hash: hash.to_owned(),
            lib_path: format!("{}/{}/{hash}.rlab", &hash[0..2], &hash[2..4]),
            width: 100,
            height: 100,
            import_date: 0,
            import_session: "s".into(),
            capture_date: None,
            original_filename: None,
            stack_id: None,
            stack_is_primary: true,
            has_edits: false,
            protected: false,
        }
    }

    /// The frame list follows the grid, not the order the user clicked in, so
    /// the same selection always fuses the same way and always hosts the
    /// result on the same photo.
    #[test]
    fn focus_stack_frames_follow_grid_order() {
        let root = Path::new("/lib");
        let results = [row(1, "aabbcc01"), row(2, "aabbcc02"), row(3, "aabbcc03")];

        // Selected bottom-up; photo 3 was clicked first.
        let frames = selected_frames(&results, &[3, 1], root);

        let hashes: Vec<&str> = frames.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(hashes, ["aabbcc01", "aabbcc03"]);
        assert_eq!(
            frames[0].1,
            Path::new("/lib/files/aa/bb/aabbcc01.rlab"),
            "frames must resolve to the library's content-addressed .rlab files",
        );
    }

    #[test]
    fn unselected_photos_are_not_frames() {
        let root = Path::new("/lib");
        let results = [row(1, "aabbcc01"), row(2, "aabbcc02")];

        assert!(selected_frames(&results, &[], root).is_empty());
        assert_eq!(selected_frames(&results, &[2], root).len(), 1);
    }

    #[test]
    fn thumb_target_side_buckets_clamps_and_never_upscales() {
        // (thumb_scale, pixels_per_point) -> expected device-pixel max side.
        let cases = [
            // 1× display: target tracks the cell size (512 * scale), bucketed.
            (0.25, 1.0, 128), // 128 px cell
            (0.5, 1.0, 256),  // 256 px cell
            (1.0, 1.0, 512),  // 512 px cell == source
            // 2× (Retina): default scale already needs the full 512 px source;
            // larger never upscales past it.
            (0.25, 2.0, 256),
            (0.5, 2.0, 512),
            (1.0, 2.0, 512), // would be 1024 → clamped to source
            // Fractional DPI snaps to the nearest bucket.
            (0.5, 1.5, 384), // 256 * 1.5 = 384
        ];
        for (scale, ppp, expected) in cases {
            assert_eq!(
                thumb_target_side(scale, ppp),
                expected,
                "scale={scale} ppp={ppp}"
            );
            assert!(thumb_target_side(scale, ppp) <= THUMB_SOURCE_SIDE);
        }
    }
}
