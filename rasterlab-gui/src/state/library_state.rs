use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use rasterlab_library::{
    CollectionId, CollectionRow, DeleteProgress, ImportCollection, ImportProgress,
    ImportSessionRow, Library, LibraryBusy, LibraryMeta, NotALibrary, PhotoId, PhotoRow,
    RebuildProgress, ScrubProgress, SearchFilter, SortOrder, import::rlab_path,
};
use serde::{Deserialize, Serialize};

use crate::panels::tools::shared::MIN_STACK_FRAMES;

// ── LibraryView ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LibraryView {
    #[default]
    AllPhotos,
    RecentlyDeleted,
    Session(String),
    Collection(CollectionId),
}

// ── Recently Deleted ──────────────────────────────────────────────────────────

/// Which bulk delete operation is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteKind {
    /// Moving the selection into Recently Deleted.
    ToRecentlyDeleted,
    /// Moving it back out.
    Restore,
    /// Erasing the selection from Recently Deleted for good.
    Permanent,
    /// Erasing everything in Recently Deleted for good.
    Empty,
    /// Deleting the marked collections, leaving their photos alone.
    Collections,
}

impl DeleteKind {
    /// What the progress line calls the operation while it runs.
    pub fn progress_verb(self) -> &'static str {
        match self {
            Self::ToRecentlyDeleted => "Moving to Recently Deleted",
            Self::Restore => "Restoring",
            Self::Permanent => "Deleting permanently",
            Self::Empty => "Emptying Recently Deleted",
            Self::Collections => "Deleting collections",
        }
    }

    /// What the status line calls it once it is over.
    pub fn past_verb(self) -> &'static str {
        match self {
            Self::ToRecentlyDeleted => "Moved to Recently Deleted",
            Self::Restore => "Restored",
            Self::Permanent | Self::Empty => "Permanently deleted",
            Self::Collections => "Deleted",
        }
    }

    /// What the operation counts, for the status line that tallies it.
    pub fn item_count(self, n: usize) -> String {
        match self {
            Self::Collections => plural(n, "collection"),
            _ => plural(n, "photo"),
        }
    }
}

/// A bulk delete operation running in the background.
///
/// Each photo costs a file rename or an unlink, and each collection a rewrite
/// of every member file, so a batch of any size on a network-mounted library
/// is minutes of work.  Running that on the UI thread is what left the window
/// unable to paint for long enough that the desktop offered to kill it, so
/// these report progress and take an answer of "stop" instead.
pub struct DeleteTask {
    pub kind: DeleteKind,
    pub progress: DeleteProgress,
    /// True once the user has asked it to stop, until the worker reports back.
    pub stopping: bool,
}

/// "1 photo" / "3 photos", for the status lines that count them.
pub fn plural(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

// ── Collections ───────────────────────────────────────────────────────────────

/// How much of the grid selection a collection already holds.
///
/// Drives the mark shown beside each collection in the grid's Collections
/// menu, and what clicking it does: a collection that holds the whole
/// selection removes it, anything else takes the photos it is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Membership {
    /// No selected photo is in the collection — an empty selection included.
    None,
    /// Some of the selection is in it, some is not.
    Partial,
    /// Every selected photo is in it.
    All,
}

/// The collection dialog currently on screen, if any.
pub enum CollectionPrompt {
    /// Naming a new collection.
    New {
        entry: NameEntry,
        /// Photos to put in it once it exists. Captured when the dialog opens,
        /// so a click in the grid's Collections menu still lands on the photos
        /// the user right-clicked even if the selection moves on.
        photos: Vec<PhotoId>,
    },
    /// Renaming an existing one.
    Rename { id: CollectionId, entry: NameEntry },
    /// Confirming that collections should go.  Several at once when several
    /// are marked in the sidebar, so a clean-up doesn't need one dialog per
    /// collection.
    Delete { ids: Vec<CollectionId> },
}

impl CollectionPrompt {
    pub fn new_collection(photos: Vec<PhotoId>) -> Self {
        Self::New {
            entry: NameEntry::default(),
            photos,
        }
    }

    pub fn rename(id: CollectionId, current_name: &str) -> Self {
        Self::Rename {
            id,
            entry: NameEntry::seeded(current_name),
        }
    }
}

// ── Folder import options ─────────────────────────────────────────────────────

/// Which of the three collection choices the folder-import dialog is on.
///
/// Kept apart from [`rasterlab_library::ImportCollection`] so the name the user
/// typed survives switching to another choice and back, and so the choice
/// itself can be remembered in the prefs file between runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportCollectionChoice {
    /// Import without touching collections.
    #[default]
    None,
    /// One collection per directory, named after it.
    PerFolder,
    /// One collection, named by the user, for the whole import.
    Named,
}

/// The question asked after a folder is picked and before its import starts:
/// what collection, if any, should the photos be filed into.
pub struct FolderImportPrompt {
    pub folder: PathBuf,
    pub choice: ImportCollectionChoice,
    /// The name for [`ImportCollectionChoice::Named`], seeded with the folder's
    /// own name so the common case needs no typing.
    pub name: String,
    /// Set once the name field has claimed keyboard focus, so it is taken on
    /// the frame the user picks `Named` and not on every frame after.
    pub focused: bool,
}

impl FolderImportPrompt {
    pub fn new(folder: PathBuf, choice: ImportCollectionChoice) -> Self {
        let name = folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Self {
            folder,
            choice,
            name,
            focused: false,
        }
    }

    /// What the library should be asked to do, or `None` when the dialog is not
    /// answerable yet — a named collection with nothing typed in it.
    pub fn to_import_collection(&self) -> Option<ImportCollection> {
        match self.choice {
            ImportCollectionChoice::None => Some(ImportCollection::None),
            ImportCollectionChoice::PerFolder => Some(ImportCollection::PerFolder),
            ImportCollectionChoice::Named => {
                let name = self.name.trim();
                (!name.is_empty()).then(|| ImportCollection::Named(name.to_owned()))
            }
        }
    }
}

/// The name being typed into one of the collection dialogs.
#[derive(Default)]
pub struct NameEntry {
    pub name: String,
    /// Why the name was rejected. The dialog stays open showing this rather
    /// than closing and making the user type the name again.
    pub error: Option<String>,
    /// Set once the field has been given keyboard focus, so it is claimed on
    /// the first frame only and the user can then tab away from it.
    pub focused: bool,
}

impl NameEntry {
    fn seeded(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ..Default::default()
        }
    }
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

const DETAIL_METADATA_DEBOUNCE: Duration = Duration::from_millis(750);

/// Small selection-scoped view of an `.rlab` file. The loader seeks over the
/// original image, preview, and ECC data, and this value is retained across UI
/// repaints so even those small reads happen only when selection changes.
pub(crate) struct SelectedPhotoDetail {
    pub id: PhotoId,
    pub hash: String,
    request_revision: u64,
    pub source_path: Option<String>,
    pub copy_names: Vec<String>,
    pub active_copy_index: usize,
    pub active_copy_saving: bool,
    pub load_error: Option<String>,
    pub loading: bool,
}

pub(crate) struct MetadataDraft {
    lmta: LibraryMeta,
    dirty: bool,
    last_edited: Option<Instant>,
    revision: u64,
    in_flight_revision: Option<u64>,
}

pub(crate) struct DetailLoadRequest {
    pub id: PhotoId,
    pub hash: String,
    pub path: PathBuf,
    pub request_revision: u64,
}

pub(crate) struct MetadataCommitRequest {
    pub id: PhotoId,
    pub revision: u64,
    pub lmta: LibraryMeta,
    pub library: Arc<Library>,
}

// ── Open failures ─────────────────────────────────────────────────────────────

/// The banner text for a failed open, plus the path to offer a retry for.
///
/// Two of these are not faults in the library at all — it is held by another
/// process, or it is on something that is not connected — and both are fixed
/// by waiting and trying again, so each says so in those terms and comes with
/// a path to retry, rather than showing `flock` wording no one asked about.
/// Anything else is the library itself being broken, and carries its own text.
fn open_failure(path: &Path, err: &anyhow::Error) -> (String, Option<PathBuf>) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    if err.downcast_ref::<LibraryBusy>().is_some() {
        return (
            format!(
                "\"{name}\" is in use by another RasterLab process — a \
                 command-line rebuild or scrub, most likely. It will open once \
                 that finishes."
            ),
            Some(path.to_path_buf()),
        );
    }
    if err.downcast_ref::<NotALibrary>().is_some() {
        return (
            format!(
                "No library at {} any more. If it lives on a drive or share \
                 that is not connected, connect it and try again.",
                path.display()
            ),
            Some(path.to_path_buf()),
        );
    }
    (format!("Failed to open library: {err}"), None)
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

    /// Collections ticked in the sidebar with ctrl- or shift-click, for the
    /// actions that can take more than one.  Kept apart from `view`, which is
    /// the single collection whose photos the grid is showing.
    pub marked_collections: Vec<CollectionId>,

    /// Which photos are in each collection, refreshed alongside `collections`.
    /// Held rather than queried on demand because the grid asks about the
    /// current selection for every collection, every frame a menu is open.
    pub collection_members: HashMap<CollectionId, HashSet<PhotoId>>,

    /// The open collection dialog (new / delete confirmation), if any.
    pub collection_prompt: Option<CollectionPrompt>,

    /// A folder waiting on the user's answer about collections before its
    /// import starts, if any.
    pub folder_import_prompt: Option<FolderImportPrompt>,
    pub all_photo_count: usize,
    pub recently_deleted_count: usize,

    /// Error message to show in a status bar or dialog.
    pub last_error: Option<String>,

    /// The library the last open failed on, when trying it again is the whole
    /// remedy: another process holds it, or it is on a volume that is not
    /// mounted.  Kept so the error banner can offer a button, since the wait
    /// can be long enough that retracing the Open Library dialog is worse.
    pub retry_library: Option<PathBuf>,

    /// Per-file `(path, message)` failures from the most recent import. Retained
    /// after the import finishes so the user can review what went wrong.
    pub last_import_errors: Vec<(PathBuf, String)>,

    /// When true, show the import-errors detail window.
    pub show_import_errors: bool,

    /// Progress of a running integrity scrub, or `None` when idle.
    pub scrub_progress: Option<ScrubProgress>,

    /// Progress of a running index rebuild, or `None` when idle.
    pub rebuild_progress: Option<RebuildProgress>,

    /// True once the user has asked a running rebuild to stop, until the
    /// worker actually reports back. Only the status line reads it; the
    /// authoritative "is one running" answer is `AppState::rebuild_running`.
    pub rebuild_stopping: bool,

    /// When the running index rebuild started; used to estimate time left.
    pub rebuild_started: Option<std::time::Instant>,

    /// Per-file `(path, message)` uncorrectable failures from the most recent
    /// scrub. Retained after it finishes so the user can review them.
    pub last_scrub_errors: Vec<(PathBuf, String)>,

    /// When true, show the scrub-errors detail window.
    pub show_scrub_errors: bool,

    /// The running bulk Recently Deleted operation, or `None` when idle.
    ///
    /// Only the progress line reads it; the authoritative "is one running"
    /// answer is `AppState::delete_running`.
    pub delete_task: Option<DeleteTask>,

    /// Per-photo `(name, message)` failures from the most recent bulk Recently
    /// Deleted operation. Retained after it finishes so the user can review
    /// which photos did not move.
    pub last_delete_errors: Vec<(String, String)>,

    /// When true, show the delete-errors detail window.
    pub show_delete_errors: bool,

    /// When true, show the "Move to Recently Deleted?" confirmation dialog.
    pub confirm_delete: bool,

    /// Confirmation for permanently deleting the selected recycle-bin items.
    pub confirm_permanent_delete: bool,

    /// Confirmation for permanently emptying the library recycle bin.
    pub confirm_empty_recently_deleted: bool,

    // Raw text for exact-value filter inputs (persists across frames)
    pub iso_exact_text: String,
    pub aperture_exact_text: String,
    pub shutter_exact_text: String,
    pub resolution_text: String,

    // Per-field validation errors; `Some` means the input is out of the
    // reasonable domain and the filter for that field is not applied.
    pub iso_error: Option<String>,
    pub aperture_error: Option<String>,
    pub shutter_error: Option<String>,
    pub resolution_error: Option<String>,

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

    /// Detail data for the current single selection. A failed load is cached
    /// too, preventing a disconnected network share from being retried on each
    /// frame.
    pub(crate) selected_detail: Option<SelectedPhotoDetail>,

    /// In-progress metadata text keyed by photo. Dirty drafts survive a failed
    /// commit and a temporary selection change.
    pub(crate) metadata_drafts: HashMap<PhotoId, MetadataDraft>,

    /// Active-copy writes keyed by project hash, independent of selection.
    pub(crate) active_copy_saves: HashSet<String>,

    /// Monotonic token used to reject stale background detail responses even
    /// when selection returns to the same photo before an old read completes.
    pub(crate) detail_request_revision: u64,
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
            marked_collections: Vec::new(),
            collection_members: HashMap::new(),
            collection_prompt: None,
            folder_import_prompt: None,
            all_photo_count: 0,
            recently_deleted_count: 0,
            last_error: None,
            retry_library: None,
            last_import_errors: Vec::new(),
            show_import_errors: false,
            scrub_progress: None,
            rebuild_progress: None,
            rebuild_stopping: false,
            rebuild_started: None,
            last_scrub_errors: Vec::new(),
            show_scrub_errors: false,
            delete_task: None,
            last_delete_errors: Vec::new(),
            show_delete_errors: false,
            confirm_delete: false,
            confirm_permanent_delete: false,
            confirm_empty_recently_deleted: false,
            iso_exact_text: String::new(),
            aperture_exact_text: String::new(),
            shutter_exact_text: String::new(),
            resolution_text: String::new(),
            iso_error: None,
            aperture_error: None,
            shutter_error: None,
            resolution_error: None,
            pending_open_photo: None,
            pending_focus_stack: None,
            scroll_to_hash: None,
            thumb_target_side: 0,
            selected_detail: None,
            metadata_drafts: HashMap::new(),
            active_copy_saves: HashSet::new(),
            detail_request_revision: 0,
        }
    }
}

impl LibraryState {
    /// Update the selection-scoped placeholder and return the one background
    /// read needed for a newly selected photo.
    pub(crate) fn begin_selected_detail(
        &mut self,
        selection: Option<(PhotoId, &str)>,
    ) -> Option<DetailLoadRequest> {
        let already_current = match (self.selected_detail.as_ref(), selection) {
            (None, None) => true,
            (Some(detail), Some((id, hash))) => detail.id == id && detail.hash == hash,
            _ => false,
        };
        if already_current {
            return None;
        }
        self.selected_detail = None;

        // Drafts outlive the selection deliberately, so an unsaved edit
        // survives a detour to another photo. A draft with nothing unsaved in
        // it has no such claim, and keeping one per photo visited would grow
        // for the length of the session.
        let keep = selection.map(|(id, _)| id);
        self.metadata_drafts
            .retain(|id, draft| draft.dirty || Some(*id) == keep);

        let (id, hash) = selection?;
        let lib = self.library.as_ref()?;
        self.detail_request_revision = self.detail_request_revision.wrapping_add(1);
        let request_revision = self.detail_request_revision;
        self.selected_detail = Some(SelectedPhotoDetail {
            id,
            hash: hash.to_owned(),
            request_revision,
            source_path: None,
            copy_names: Vec::new(),
            active_copy_index: 0,
            active_copy_saving: self.active_copy_saves.contains(hash),
            load_error: None,
            loading: true,
        });
        Some(DetailLoadRequest {
            id,
            hash: hash.to_owned(),
            // A photo shown in Recently Deleted has its file elsewhere, and
            // reading it where an active photo's would be fails the whole
            // detail load — collections included.
            path: lib.photo_rlab_path(hash),
            request_revision,
        })
    }

    pub(crate) fn finish_selected_detail(
        &mut self,
        id: PhotoId,
        hash: &str,
        request_revision: u64,
        result: Result<rasterlab_core::project::RlabLibrarySummary, String>,
    ) {
        let is_current = self.selected_detail.as_ref().is_some_and(|detail| {
            detail.id == id && detail.hash == hash && detail.request_revision == request_revision
        });
        if !is_current {
            return;
        }
        match result {
            Ok(mut summary) => {
                let source_path = summary
                    .lmta
                    .as_ref()
                    .and_then(|lmta| lmta.source_path.clone())
                    .or_else(|| summary.meta_source_path.take());
                if let Some(lmta) = summary.lmta {
                    let keep_dirty = self
                        .metadata_drafts
                        .get(&id)
                        .is_some_and(|draft| draft.dirty);
                    if !keep_dirty {
                        self.metadata_drafts.insert(
                            id,
                            MetadataDraft {
                                lmta,
                                dirty: false,
                                last_edited: None,
                                revision: 0,
                                in_flight_revision: None,
                            },
                        );
                    }
                }
                self.selected_detail = Some(SelectedPhotoDetail {
                    id,
                    hash: hash.to_owned(),
                    request_revision,
                    source_path,
                    copy_names: summary.copy_names,
                    active_copy_index: summary.active_copy_index,
                    active_copy_saving: self.active_copy_saves.contains(hash),
                    load_error: None,
                    loading: false,
                });
            }
            Err(error) => {
                let message = format!("Photo details unavailable: {error}");
                self.selected_detail = Some(SelectedPhotoDetail {
                    id,
                    hash: hash.to_owned(),
                    request_revision,
                    source_path: None,
                    copy_names: Vec::new(),
                    active_copy_index: 0,
                    active_copy_saving: self.active_copy_saves.contains(hash),
                    load_error: Some(message.clone()),
                    loading: false,
                });
                self.last_error = Some(message);
            }
        }
    }

    pub(crate) fn selected_detail_metadata(&self) -> Option<&LibraryMeta> {
        let id = self.selected_detail.as_ref()?.id;
        self.metadata_drafts.get(&id).map(|draft| &draft.lmta)
    }

    /// Replace the current in-memory edit without touching the network share.
    pub(crate) fn edit_selected_detail_metadata(&mut self, lmta: LibraryMeta) {
        let Some(id) = self.selected_detail.as_ref().map(|detail| detail.id) else {
            return;
        };
        if let Some(draft) = self.metadata_drafts.get_mut(&id) {
            draft.lmta = lmta;
            draft.dirty = true;
            draft.last_edited = Some(Instant::now());
            draft.revision = draft.revision.wrapping_add(1);
        }
    }

    pub(crate) fn selected_detail_commit_due(&self) -> bool {
        let Some(id) = self.selected_detail.as_ref().map(|detail| detail.id) else {
            return false;
        };
        self.metadata_drafts.get(&id).is_some_and(|draft| {
            draft.dirty
                && draft
                    .last_edited
                    .is_some_and(|edited| edited.elapsed() >= DETAIL_METADATA_DEBOUNCE)
        })
    }

    /// Snapshot a dirty draft for one background write. `force` is used for
    /// focus/selection changes; otherwise the idle debounce must have elapsed.
    pub(crate) fn prepare_selected_detail_commit(
        &mut self,
        force: bool,
    ) -> Option<MetadataCommitRequest> {
        let id = self.selected_detail.as_ref().map(|detail| detail.id)?;
        self.prepare_detail_commit(id, force)
    }

    /// Snapshot a specific photo's dirty draft. Completion handlers use this
    /// to drain a newer revision even when the user selected another photo
    /// while the preceding network write was in flight.
    pub(crate) fn prepare_detail_commit(
        &mut self,
        id: PhotoId,
        force: bool,
    ) -> Option<MetadataCommitRequest> {
        let draft = self.metadata_drafts.get_mut(&id)?;
        let due = draft
            .last_edited
            .is_some_and(|edited| edited.elapsed() >= DETAIL_METADATA_DEBOUNCE);
        if !draft.dirty || draft.in_flight_revision.is_some() || (!force && !due) {
            return None;
        }
        let lib = self.library.clone()?;
        draft.in_flight_revision = Some(draft.revision);
        Some(MetadataCommitRequest {
            id,
            revision: draft.revision,
            lmta: draft.lmta.clone(),
            library: lib,
        })
    }

    /// Take every unsaved draft, for a caller that is about to lose the chance
    /// to write them in the background — app exit, or closing the library.
    ///
    /// Drafts already in flight are left alone: that worker still owns them,
    /// and duplicating the write would race it.
    pub(crate) fn drain_metadata_drafts(&mut self) -> Vec<(PhotoId, LibraryMeta)> {
        let mut pending: Vec<(PhotoId, LibraryMeta)> = self
            .metadata_drafts
            .iter_mut()
            .filter(|(_, draft)| draft.dirty && draft.in_flight_revision.is_none())
            .map(|(id, draft)| {
                draft.dirty = false;
                draft.last_edited = None;
                (*id, draft.lmta.clone())
            })
            .collect();
        pending.sort_by_key(|(id, _)| *id);
        pending
    }

    pub(crate) fn finish_selected_detail_commit(
        &mut self,
        id: PhotoId,
        revision: u64,
        result: Result<(), String>,
    ) {
        let Some(draft) = self.metadata_drafts.get_mut(&id) else {
            return;
        };
        if draft.in_flight_revision != Some(revision) {
            return;
        }
        draft.in_flight_revision = None;
        match result {
            Ok(()) => {
                if draft.revision == revision {
                    draft.dirty = false;
                    draft.last_edited = None;
                }
            }
            Err(error) => {
                // Stay dirty but stop the debounce from re-firing: a library on
                // a share that has gone away must not be retried every 750 ms
                // for the rest of the session. The draft is still written by
                // the next forced commit — a selection change, or the flush on
                // exit — and the user is told it did not land.
                draft.last_edited = None;
                self.last_error = Some(format!("Metadata update failed: {error}"));
            }
        }
    }

    pub(crate) fn begin_cached_active_copy_save(&mut self, hash: &str) -> bool {
        if !self.active_copy_saves.insert(hash.to_owned()) {
            return false;
        }
        if let Some(detail) = self
            .selected_detail
            .as_mut()
            .filter(|detail| detail.hash == hash)
        {
            detail.active_copy_saving = true;
        }
        true
    }

    /// Finish an active-copy write. `copy_idx` is `Some` only when the write
    /// landed, in which case the panel reloads rather than patching its cached
    /// copy list — the file is the authority on what it now says.
    pub(crate) fn finish_cached_active_copy_save(&mut self, hash: &str, copy_idx: Option<usize>) {
        self.active_copy_saves.remove(hash);
        if copy_idx.is_some()
            && self
                .selected_detail
                .as_ref()
                .is_some_and(|detail| detail.hash == hash)
        {
            // Force a post-write selective reload. Any older detail response is
            // ignored because no current placeholder matches it.
            self.selected_detail = None;
            return;
        }
        let Some(detail) = self
            .selected_detail
            .as_mut()
            .filter(|detail| detail.hash == hash)
        else {
            return;
        };
        detail.active_copy_saving = false;
    }

    /// Reload results from the DB based on the current view + filter + sort.
    pub fn refresh(&mut self) {
        let Some(lib) = &self.library else { return };

        let deleted = lib.recently_deleted().unwrap_or_default();
        self.recently_deleted_count = deleted.len();

        let photos = if self.view == LibraryView::RecentlyDeleted {
            Ok(deleted.into_iter().map(|row| row.photo).collect())
        } else {
            // Compose the view scope into a copy of the filter so that
            // session/collection views also honor shutter/ISO/aperture/etc.
            let mut filter = self.filter.clone();
            match &self.view {
                LibraryView::AllPhotos | LibraryView::RecentlyDeleted => {}
                LibraryView::Session(id) => filter.import_session = Some(id.clone()),
                LibraryView::Collection(id) => filter.collection_id = Some(*id),
            }
            if filter.is_empty() {
                lib.all_photos(self.sort)
            } else {
                lib.search(&filter, self.sort)
            }
        };

        match photos {
            Ok(photos) => self.results = photos,
            // Leaving the previous view's photos on screen is worse than an
            // empty grid: they read as the answer to the query that just
            // failed, which is how a collection view that could not be
            // queried at all looked like one holding the wrong photos.
            Err(e) => {
                self.results.clear();
                self.last_error = Some(format!("Could not list photos: {e}"));
            }
        }

        // Refresh sidebar lists
        self.sessions = lib.all_sessions().unwrap_or_default();
        self.collections = lib.all_collections().unwrap_or_default();
        // A mark on a collection that is gone — deleted here, or dropped by a
        // rebuild — must not linger and take part in the next bulk action.
        let live: HashSet<CollectionId> = self.collections.iter().map(|c| c.id).collect();
        self.marked_collections.retain(|id| live.contains(id));
        self.collection_members.clear();
        for (collection, photo) in lib.collection_memberships().unwrap_or_default() {
            self.collection_members
                .entry(collection)
                .or_default()
                .insert(photo);
        }
        // Straight from the photo rows. Summing the sessions' cached counts
        // left this frozen through an import — those are only written when a
        // session finishes — while the grid beside it filled up.
        if let Ok(count) = lib.photo_count() {
            self.all_photo_count = count.max(0) as usize;
        }
    }

    /// Human-readable one-liner for a running index rebuild, or `None` when
    /// idle. Includes files done / total, a time-left estimate once enough
    /// has elapsed for it to be meaningful, and a running error count.
    pub fn rebuild_status_text(&self) -> Option<String> {
        let p = self.rebuild_progress.as_ref()?;
        if self.rebuild_stopping {
            // No estimate: what is left is the current file, not the rest of
            // the walk.
            return Some(format!(
                "Stopping index rebuild… {}/{} indexed",
                p.done, p.total
            ));
        }
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

    /// Open the library at `path`, which must already be one.
    pub fn open_library(&mut self, path: PathBuf, thumb_scale: f32) {
        let opened = Library::open_existing(&path);
        self.adopt_library(path, thumb_scale, opened);
    }

    /// Open the library at `path`, laying one out there if it is new.
    ///
    /// Only for File > New Library. Everywhere else a path that is not a
    /// library is a mistake to report, not an invitation to make one.
    pub fn create_library(&mut self, path: PathBuf, thumb_scale: f32) {
        let opened = Library::open_or_create(&path);
        self.adopt_library(path, thumb_scale, opened);
    }

    fn adopt_library(&mut self, path: PathBuf, thumb_scale: f32, opened: anyhow::Result<Library>) {
        match opened {
            Ok(lib) => {
                self.library = Some(Arc::new(lib));
                self.thumb_scale = thumb_scale;
                self.view = LibraryView::AllPhotos;
                self.filter = SearchFilter::default();
                self.iso_exact_text.clear();
                self.aperture_exact_text.clear();
                self.shutter_exact_text.clear();
                self.resolution_text.clear();
                self.iso_error = None;
                self.aperture_error = None;
                self.shutter_error = None;
                self.resolution_error = None;
                self.selected.clear();
                self.marked_collections.clear();
                self.selected_detail = None;
                self.metadata_drafts.clear();
                self.active_copy_saves.clear();
                self.thumbs.clear();
                self.last_error = None;
                self.retry_library = None;
                self.refresh();
            }
            Err(e) => {
                let (message, retry) = open_failure(&path, &e);
                self.last_error = Some(message);
                self.retry_library = retry;
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

    /// Human-readable one-liner for a running bulk Recently Deleted operation,
    /// or `None` when idle.
    pub fn delete_status_text(&self) -> Option<String> {
        let task = self.delete_task.as_ref()?;
        let progress = &task.progress;
        let verb = if task.stopping {
            "Stopping"
        } else {
            task.kind.progress_verb()
        };
        let mut text = format!("{verb}… {}/{}", progress.done, progress.total);
        if !progress.protected.is_empty() {
            text.push_str(&format!(", {} protected", progress.protected.len()));
        }
        if !progress.errors.is_empty() {
            text.push_str(&format!(", {} error(s)", progress.errors.len()));
        }
        Some(text)
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

    // ── Collections ───────────────────────────────────────────────────────

    /// How many photos are in a collection.
    pub fn collection_len(&self, id: CollectionId) -> usize {
        self.collection_members.get(&id).map_or(0, HashSet::len)
    }

    /// Names of the collections a photo is in, in the sidebar's order.
    ///
    /// Read from the index rather than the photo's own `LMTA`, so that adding
    /// a collection shows up while the photo stays selected: the detail
    /// panel's copy of the file is loaded once per selection and knows nothing
    /// of a change made from the grid afterwards.
    pub fn collections_for(&self, photo: PhotoId) -> Vec<&str> {
        self.collections
            .iter()
            .filter(|collection| {
                self.collection_members
                    .get(&collection.id)
                    .is_some_and(|members| members.contains(&photo))
            })
            .map(|collection| collection.name.as_str())
            .collect()
    }

    /// How much of the current selection collection `id` already holds.
    pub fn selection_membership(&self, id: CollectionId) -> Membership {
        if self.selected.is_empty() {
            return Membership::None;
        }
        let empty = HashSet::new();
        let members = self.collection_members.get(&id).unwrap_or(&empty);
        let in_collection = self
            .selected
            .iter()
            .filter(|photo| members.contains(photo))
            .count();
        match in_collection {
            0 => Membership::None,
            n if n == self.selected.len() => Membership::All,
            _ => Membership::Partial,
        }
    }

    /// Create a collection and put `photos` in it.
    ///
    /// The message in `Err` belongs in the dialog next to the name field: it
    /// says the name cannot be used, and the dialog stays open so the user can
    /// change it. Failures past that point are the library's rather than the
    /// name's, and are reported through `last_error` like every other one.
    pub fn create_collection(&mut self, name: &str, photos: &[PhotoId]) -> Result<(), String> {
        let name = name.trim();
        self.check_collection_name(name, None)?;
        let Some(lib) = self.library.clone() else {
            return Err("No library is open.".to_owned());
        };

        let collection = lib
            .create_collection(name)
            .map_err(|e| format!("Could not create the collection: {e}"))?;
        if !photos.is_empty()
            && let Err(e) = lib.add_to_collection(collection.id, photos)
        {
            self.last_error = Some(format!("Add to collection failed: {e}"));
        }
        self.refresh();
        Ok(())
    }

    /// Rename a collection, under the same name rules as creating one.
    ///
    /// One index row however many photos are in it: the files record the
    /// collection's id, not its name.
    pub fn rename_collection(&mut self, id: CollectionId, name: &str) -> Result<(), String> {
        let name = name.trim();
        self.check_collection_name(name, Some(id))?;
        let Some(lib) = self.library.clone() else {
            return Err("No library is open.".to_owned());
        };
        lib.rename_collection(id, name)
            .map_err(|e| format!("Could not rename the collection: {e}"))?;
        self.refresh();
        Ok(())
    }

    /// Reject a name no collection can be given, before any of it reaches the
    /// library.
    ///
    /// `keep` is the collection being renamed, whose own name is not a clash —
    /// otherwise correcting the capitalisation of a name would be refused as a
    /// duplicate of itself.
    fn check_collection_name(&self, name: &str, keep: Option<CollectionId>) -> Result<(), String> {
        if name.is_empty() {
            return Err("Enter a name for the collection.".to_owned());
        }
        // The index rejects an exact duplicate itself; catching it here — and
        // case-insensitively — turns a raw SQL error into an answer, and keeps
        // "Portfolio" and "portfolio" from sitting next to each other in the
        // sidebar looking like the same thing.
        let clash = self
            .collections
            .iter()
            .any(|existing| Some(existing.id) != keep && existing.name.eq_ignore_ascii_case(name));
        if clash {
            return Err(format!("A collection named “{name}” already exists."));
        }
        Ok(())
    }

    /// The name of a collection the sidebar currently lists.
    pub fn collection_name(&self, id: CollectionId) -> Option<&str> {
        self.collections
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.as_str())
    }

    /// Let go of collections that are on their way out, before the worker
    /// that deletes them has got to any of them.
    ///
    /// Their marks go now rather than when the run ends: leaving them marked
    /// invites a second run at them while the first is still going.  The grid
    /// cannot stay pointed at one either, so it falls back to All Photos — the
    /// photos in it are staying in the library.
    pub fn leave_collections(&mut self, ids: &[CollectionId]) {
        self.marked_collections.retain(|id| !ids.contains(id));
        if let LibraryView::Collection(id) = self.view
            && ids.contains(&id)
        {
            self.view = LibraryView::AllPhotos;
            self.select_none();
            self.refresh();
        }
    }

    /// Add every selected photo to a collection; the ones already in it stay
    /// as they are.
    pub fn add_selected_to_collection(&mut self, id: CollectionId) {
        self.change_collection_membership(id, true);
    }

    /// Take every selected photo out of a collection.
    pub fn remove_selected_from_collection(&mut self, id: CollectionId) {
        self.change_collection_membership(id, false);
    }

    fn change_collection_membership(&mut self, id: CollectionId, member: bool) {
        let Some(lib) = self.library.clone() else {
            return;
        };
        let photos = self.selected.clone();
        if photos.is_empty() {
            return;
        }
        let result = if member {
            lib.add_to_collection(id, &photos)
        } else {
            lib.remove_from_collection(id, &photos)
        };
        if let Err(e) = result {
            let what = if member { "Add to" } else { "Remove from" };
            self.last_error = Some(format!("{what} collection failed: {e}"));
        }
        self.refresh();
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
    use std::sync::atomic::AtomicBool;

    use super::*;

    /// Held, gone and broken are three different messages, and only the first
    /// two are worth a Retry — retrying a corrupt index just fails again.
    #[test]
    fn open_failure_separates_busy_and_missing_from_broken() {
        let path = Path::new("/photos/Main Library");

        let (message, retry) = open_failure(path, &anyhow::Error::new(LibraryBusy));
        assert_eq!(retry.as_deref(), Some(path), "a busy library is retryable");
        assert!(
            message.contains("Main Library") && message.contains("another RasterLab process"),
            "busy message names the library and the reason: {message}"
        );

        let (message, retry) =
            open_failure(path, &anyhow::Error::new(NotALibrary(path.to_path_buf())));
        assert_eq!(
            retry.as_deref(),
            Some(path),
            "a library on a disconnected drive is worth retrying"
        );
        assert!(
            message.contains("/photos/Main Library") && message.contains("connected"),
            "missing message names the path and what to do about it: {message}"
        );

        let (message, retry) = open_failure(path, &anyhow::anyhow!("index is corrupt"));
        assert_eq!(retry, None, "a broken library is not retryable");
        assert!(
            message.contains("index is corrupt"),
            "other failures keep their own text: {message}"
        );
    }

    /// The bug this guards: opening a library that is no longer there left
    /// the user in front of an empty grid, because the open had quietly made
    /// a new library at that path and succeeded.
    #[test]
    fn opening_a_library_that_is_gone_reports_it_instead_of_making_one() {
        let tmp = tempfile::tempdir().unwrap();
        let gone = tmp.path().join("Main Library");
        let mut state = LibraryState::default();

        state.open_library(gone.clone(), 0.5);

        assert!(state.library.is_none(), "no library should be open");
        assert!(!gone.exists(), "a library was created at the missing path");
        let error = state.last_error.expect("the failure has to be shown");
        assert!(error.contains("Main Library"), "{error}");
        assert_eq!(
            state.retry_library.as_deref(),
            Some(gone.as_path()),
            "the banner should offer to try again once the drive is back"
        );
    }

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

    fn draft(dirty: bool, in_flight: Option<u64>, rating: u8) -> MetadataDraft {
        MetadataDraft {
            lmta: LibraryMeta {
                rating,
                ..LibraryMeta::default()
            },
            dirty,
            last_edited: None,
            revision: 1,
            in_flight_revision: in_flight,
        }
    }

    /// The debounced write is a background worker, which is no use to a caller
    /// that is about to exit. Draining has to hand back everything still
    /// unwritten — and nothing a worker is already carrying, or the two race
    /// to write the same photo.
    #[test]
    fn draining_takes_unwritten_drafts_and_leaves_in_flight_ones() {
        let mut state = LibraryState::default();
        state.metadata_drafts.insert(1, draft(true, None, 3));
        state.metadata_drafts.insert(2, draft(false, None, 4));
        state.metadata_drafts.insert(3, draft(true, Some(1), 5));

        let drained = state.drain_metadata_drafts();

        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].0, 1);
        assert_eq!(drained[0].1.rating, 3);
        // Taken means taken: a second flush must not write it again.
        assert!(!state.metadata_drafts[&1].dirty);
        assert!(state.drain_metadata_drafts().is_empty());
        // The in-flight draft is still owned by its worker.
        assert!(state.metadata_drafts[&3].dirty);
    }

    /// Drafts survive a detour to another photo so an unsaved caption is not
    /// lost to a stray click, but a clean one has nothing to protect and must
    /// not accumulate for every photo the user browsed past.
    #[test]
    fn selecting_another_photo_keeps_only_unsaved_drafts() {
        let mut state = LibraryState::default();
        state.metadata_drafts.insert(1, draft(true, None, 3));
        state.metadata_drafts.insert(2, draft(false, None, 4));
        state.metadata_drafts.insert(3, draft(false, None, 5));

        // No library is open, so no load is requested; the pruning still runs.
        assert!(state.begin_selected_detail(Some((3, "aabbcc03"))).is_none());

        assert!(state.metadata_drafts.contains_key(&1));
        assert!(!state.metadata_drafts.contains_key(&2));
        assert!(state.metadata_drafts.contains_key(&3));
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

    fn collection(id: CollectionId, name: &str) -> CollectionRow {
        CollectionRow {
            id,
            uuid: format!("uuid-{id}"),
            name: name.to_owned(),
            created_at: 0,
        }
    }

    /// The mark beside each collection in the grid menu, and what clicking it
    /// does, both come from this: a selection the collection holds entirely is
    /// taken out, anything less is added.
    #[test]
    fn selection_membership_measures_the_whole_selection() {
        let mut state = LibraryState::default();
        state.collection_members.insert(1, HashSet::from([10, 20]));

        assert_eq!(state.collection_len(1), 2);
        assert_eq!(state.collection_len(2), 0, "unknown collection is empty");

        // Nothing selected is nothing to add or remove.
        assert_eq!(state.selection_membership(1), Membership::None);

        state.selected = vec![10, 20];
        assert_eq!(state.selection_membership(1), Membership::All);

        state.selected = vec![10, 30];
        assert_eq!(state.selection_membership(1), Membership::Partial);

        state.selected = vec![30];
        assert_eq!(state.selection_membership(1), Membership::None);
        assert_eq!(
            state.selection_membership(2),
            Membership::None,
            "a collection with no members holds no selection"
        );
    }

    /// Both name rules are checked before the library is touched, so the
    /// dialog can say what is wrong with the name rather than reporting
    /// whatever the index made of it.
    #[test]
    fn a_new_collection_needs_a_name_that_is_not_already_taken() {
        let mut state = LibraryState {
            collections: vec![collection(1, "Portfolio")],
            ..Default::default()
        };

        assert!(state.create_collection("   ", &[]).is_err());
        let taken = state
            .create_collection("portfolio", &[])
            .expect_err("a name differing only in case is the same name");
        assert!(taken.contains("already exists"), "{taken}");

        // A usable name gets past both rules and only then wants a library.
        let no_library = state
            .create_collection("Landscapes", &[])
            .expect_err("no library is open");
        assert!(no_library.contains("No library"), "{no_library}");
    }

    /// What clicking a collection in the sidebar does, end to end: the view
    /// scopes the filter, and the filter has to come back with that
    /// collection's photos and nothing else.
    ///
    /// The library-side test for this drives `search` directly, which is one
    /// branch further along than the panel gets — this goes through the same
    /// `refresh` the sidebar calls.
    #[test]
    fn selecting_a_collection_shows_its_photos() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = LibraryState::default();
        state.create_library(tmp.path().to_path_buf(), 0.5);
        let lib = state.library.clone().expect("library should open");

        let images = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("test_images");
        lib.import_files(
            &[
                images.join("meta_test.jpg"),
                images.join("color_patches.png"),
            ],
            |_| {},
        )
        .unwrap();
        state.refresh();
        assert_eq!(state.results.len(), 2, "both photos are in All Photos");

        let coll = lib.create_collection("Portfolio").unwrap();
        let first = state.results[0].clone();
        state.select_only(first.id);
        state.add_selected_to_collection(coll.id);

        state.view = LibraryView::Collection(coll.id);
        state.refresh();

        assert_eq!(state.last_error, None, "the query failed");
        assert_eq!(
            state.results.len(),
            1,
            "collection view listed {:?}",
            state
                .results
                .iter()
                .map(|row| row.hash.as_str())
                .collect::<Vec<_>>()
        );
        assert_eq!(state.results[0].hash, first.hash);
        assert_eq!(state.collection_len(coll.id), 1, "sidebar count");
    }

    /// Handing a marked set to the delete worker drops the marks, takes the
    /// grid off a collection that is on its way out, and leaves the photos in
    /// the library once the worker has been round them all.
    #[test]
    fn deleting_marked_collections_takes_them_all() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = LibraryState::default();
        state.create_library(tmp.path().to_path_buf(), 0.5);
        let lib = state.library.clone().expect("library should open");

        let images = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("test_images");
        lib.import_files(&[images.join("meta_test.jpg")], |_| {})
            .unwrap();
        state.refresh();

        let keep = lib.create_collection("Archive").unwrap();
        let doomed: Vec<CollectionId> = ["Portfolio", "Prints"]
            .iter()
            .map(|name| lib.create_collection(name).unwrap().id)
            .collect();
        state.refresh();

        // The photo is in one of the collections being deleted, and the grid
        // is showing that collection.
        let photo = state.results[0].id;
        state.select_only(photo);
        state.add_selected_to_collection(doomed[0]);
        state.view = LibraryView::Collection(doomed[0]);
        state.marked_collections = doomed.clone();
        state.refresh();

        // The view and the marks are given up as the run starts; the
        // collections themselves go on the worker's thread.
        state.leave_collections(&doomed);
        assert_eq!(
            state.view,
            LibraryView::AllPhotos,
            "the grid cannot stay on a collection that is being deleted"
        );
        let outcome = lib
            .delete_collections(&doomed, Arc::new(AtomicBool::new(false)), |_| {})
            .unwrap();
        assert_eq!(outcome.done, doomed.len());
        assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
        state.refresh();

        assert_eq!(state.last_error, None);
        let left: Vec<CollectionId> = state.collections.iter().map(|c| c.id).collect();
        assert_eq!(left, [keep.id], "only the unmarked collection is left");
        assert!(
            state.marked_collections.is_empty(),
            "marks on deleted collections must not survive"
        );
        assert_eq!(
            state.results.len(),
            1,
            "the photo itself stays in the library"
        );
    }

    /// Renaming has the same name rules as creating, except that a collection
    /// is not a clash with itself — correcting the capitalisation of a name
    /// would otherwise be refused as a duplicate of the thing being renamed.
    #[test]
    fn renaming_a_collection_does_not_clash_with_its_own_name() {
        let state = LibraryState {
            collections: vec![collection(1, "Portfolio"), collection(2, "Prints")],
            ..Default::default()
        };

        // Its own name, in any case, is free; another collection's is not.
        assert!(state.check_collection_name("Portfolio", Some(1)).is_ok());
        assert!(state.check_collection_name("PORTFOLIO", Some(1)).is_ok());
        assert!(state.check_collection_name("Prints", Some(1)).is_err());
        assert!(state.check_collection_name("Portfolio", None).is_err());

        // And an empty name is no name at all, whichever dialog is asking.
        assert!(state.check_collection_name("", Some(1)).is_err());
        assert!(state.check_collection_name("", None).is_err());
    }

    /// The detail panel lists a photo's collections in the same order the
    /// sidebar does, so the two read as one list rather than two.
    #[test]
    fn collections_for_a_photo_follow_the_sidebar_order() {
        let mut state = LibraryState {
            // `all_collections` returns them by name; the panel must not
            // re-order them by id.
            collections: vec![
                collection(3, "Archive"),
                collection(1, "Portfolio"),
                collection(2, "Prints"),
            ],
            ..Default::default()
        };
        state.collection_members.insert(1, HashSet::from([10, 20]));
        state.collection_members.insert(2, HashSet::from([20]));
        state.collection_members.insert(3, HashSet::from([10]));

        assert_eq!(state.collections_for(10), ["Archive", "Portfolio"]);
        assert_eq!(state.collections_for(20), ["Portfolio", "Prints"]);
        assert!(state.collections_for(30).is_empty());
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
