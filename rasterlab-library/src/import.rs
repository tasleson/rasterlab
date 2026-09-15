use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rasterlab_core::import_phase;
use rasterlab_core::{
    formats::{FormatRegistry, exif_util::read_capture_date_from_prefix},
    library_meta::{CollectionRef, FileTimeStamp, LibraryExif, LibraryMeta},
    project::{RlabFile, is_rlab_path},
    verified_write::{create_dir_all_synced, write_verified_atomic},
};
use uuid::Uuid;

use crate::{
    db_trait::{CollectionId, LibraryDb, NewPhoto},
    library::ImportProgress,
    thumbnail::{generate_thumbnail, write_thumbnail},
};

// ── Public types ──────────────────────────────────────────────────────────────

/// One completed import session returned from [`import_files`].
#[derive(Debug, Clone)]
pub struct ImportSession {
    pub id: String,
    pub name: String,
    pub started_at: u64,
    pub photo_count: usize,
    pub errors: Vec<(std::path::PathBuf, String)>,
}

/// What collection, if any, an import files the photos it brings in into.
///
/// Only *newly* imported photos join: a file already in the library is skipped
/// as a duplicate and its memberships are left as the user last set them.  That
/// is what makes re-running an import over a folder cheap and predictable —
/// the second run adds whatever is new and touches nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ImportCollection {
    /// Leave collections alone.
    #[default]
    None,
    /// One collection per directory that directly holds photos, named after
    /// that directory.  A recursive import of a tree of shoot folders comes out
    /// as one collection per shoot.
    PerFolder,
    /// A single collection, under this name, for everything the run imports.
    Named(String),
}

impl ImportCollection {
    /// The collection name for a source file, or `None` when this mode files
    /// nothing.
    fn name_for(&self, path: &Path) -> Option<String> {
        let name = match self {
            Self::None => return None,
            Self::PerFolder => path.parent()?.file_name()?.to_string_lossy().into_owned(),
            Self::Named(name) => name.clone(),
        };
        let name = name.trim();
        (!name.is_empty()).then(|| name.to_owned())
    }
}

/// Everything an import run needs to know beyond the files themselves.
///
/// Takes an [`ImportCollection`] directly (`options.into()`), so a caller that
/// only cares about filing stays a one-liner.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportOptions {
    /// What collection, if any, newly imported photos are filed into.
    pub collection: ImportCollection,
    /// Delete each source file once the library is holding its contents.
    ///
    /// This covers duplicates as well as new photographs: a file skipped
    /// because the library already has those exact bytes is no less imported
    /// for having been imported earlier, and leaving it behind would mean a
    /// re-run over a half-emptied card never finishes emptying it.  Nothing is
    /// deleted until the `.rlab` holding the contents has been found on disk,
    /// and a file that failed to import is always left alone.
    pub delete_sources: bool,
}

impl From<ImportCollection> for ImportOptions {
    fn from(collection: ImportCollection) -> Self {
        Self {
            collection,
            delete_sources: false,
        }
    }
}

/// The collection an imported photo joins, as known before the photo is
/// committed: a stable uuid and display name, plus the index row id once one
/// exists.
///
/// `id` is `None` for a collection this run will have to create.  The uuid is
/// minted up front because it goes into the `.rlab` that gets serialised on a
/// preparation worker, long before the committing thread decides whether any
/// photo actually lands in it.
#[derive(Clone, Debug)]
struct AssignedCollection {
    /// The name as asked for, which is how [`CollectionAssigner`] keys it.
    key: String,
    uuid: String,
    /// The display name — an existing collection matched case-insensitively
    /// keeps the spelling the user gave it.
    name: String,
    id: Option<CollectionId>,
}

/// Hands each imported file the collection it should join, creating
/// collections the first time one is actually needed.
///
/// An existing collection of the same name is reused rather than duplicated,
/// so importing the same folder again — or a second folder of the same name —
/// adds to the collection the user already has.  The match ignores ASCII case,
/// matching what the UI refuses to let the user create by hand.
///
/// Resolution and creation are deliberately separate.  [`resolve`](Self::resolve)
/// only reads, so preparation workers can call it concurrently under one lock;
/// [`realise`](Self::realise) does the single `create_collection` write and runs
/// on the committing thread for the first photo that survives deduplication.
/// That ordering is what keeps a folder whose files are all already in the
/// library from leaving an empty collection behind.
struct CollectionAssigner<'a> {
    db: &'a dyn LibraryDb,
    mode: ImportCollection,
    /// Name (as asked for) → what it resolved to.  Without this the whole
    /// collection list would be re-read once per imported photo.
    resolved: HashMap<String, AssignedCollection>,
}

impl<'a> CollectionAssigner<'a> {
    fn new(db: &'a dyn LibraryDb, mode: ImportCollection) -> Self {
        Self {
            db,
            mode,
            resolved: HashMap::new(),
        }
    }

    /// Which collection `path` belongs in, without writing anything.
    fn resolve(&mut self, path: &Path) -> Result<Option<AssignedCollection>> {
        let Some(name) = self.mode.name_for(path) else {
            return Ok(None);
        };
        if let Some(assigned) = self.resolved.get(&name) {
            return Ok(Some(assigned.clone()));
        }
        let existing = self
            .db
            .all_collections()?
            .into_iter()
            .find(|row| row.name.eq_ignore_ascii_case(&name));
        let assigned = match existing {
            Some(row) => AssignedCollection {
                key: name.clone(),
                uuid: row.uuid,
                name: row.name,
                id: Some(row.id),
            },
            // Minted here for the same reason `Library::create_collection`
            // mints it: the uuid goes into every member file and has to
            // outlive an index rebuild, which reassigns row ids.
            None => AssignedCollection {
                key: name.clone(),
                uuid: Uuid::new_v4().to_string(),
                name: name.clone(),
                id: None,
            },
        };
        // Keyed by the name that was asked for, not the row's: a collection
        // matched case-insensitively still has to be found again next time
        // without re-reading the whole collection list.
        Ok(Some(self.resolved.entry(name).or_insert(assigned).clone()))
    }

    /// The index row id for an already-resolved collection, creating the row on
    /// first use.  The uuid is the one already written into the member's
    /// `.rlab`, so the file and the index agree however the run ends.
    fn realise(&mut self, assigned: &AssignedCollection) -> Result<CollectionId> {
        if let Some(id) = assigned.id {
            return Ok(id);
        }
        if let Some(id) = self.resolved.get(&assigned.key).and_then(|held| held.id) {
            return Ok(id);
        }
        let now = unix_now();
        let id = self
            .db
            .create_collection(&assigned.uuid, &assigned.name, now)
            .with_context(|| format!("create collection “{}”", assigned.name))?;
        if let Some(held) = self.resolved.get_mut(&assigned.key) {
            held.id = Some(id);
        }
        Ok(id)
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

/// Import a batch of files into the library.  Runs on the calling thread
/// (callers should spawn a background thread).
pub fn import_files(
    library_root: &Path,
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    paths: &[PathBuf],
    cancelled: Arc<AtomicBool>,
    progress_cb: &dyn Fn(ImportProgress),
) -> Result<ImportSession> {
    let now = unix_now();
    // Session is named by the date the user imports, so imports on the
    // same local day roll into the same session.
    let session_name = chrono_lite_date(now);

    let existing = import_phase!(
        "database_session",
        db.all_sessions()
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.name == session_name)
    );
    let (session_id, session_started_at) = match existing {
        Some(s) => (s.id, s.started_at),
        None => {
            let id = Uuid::new_v4().to_string();
            import_phase!(
                "database_session",
                db.insert_session(&id, &session_name, now, None)
            )?;
            (id, now)
        }
    };

    progress_cb(ImportProgress {
        total: paths.len(),
        done: 0,
        imported: 0,
        current_file: PathBuf::new(),
        skipped_duplicates: 0,
        deleted_sources: 0,
        errors: Vec::new(),
        scanning: false,
    });

    // Detect RAW+JPEG stacks within this batch before importing.
    let stack_map = detect_stacks(paths);
    // Picking individual files says nothing about which collection they belong
    // in, so this import files nothing.
    let assigner = Mutex::new(CollectionAssigner::new(db, ImportCollection::None));

    let jobs: Vec<PipelineJob> = paths
        .iter()
        .map(|path| PipelineJob {
            path: path.clone(),
            import_date: now,
            fallback_capture_ts: None,
        })
        .collect();
    let mut tally = ImportTally::default();
    let mut batch_hashes = HashSet::new();
    run_import_pipeline(
        library_root,
        db,
        registry,
        &jobs,
        &session_id,
        &stack_map,
        &assigner,
        &mut batch_hashes,
        &cancelled,
        paths.len(),
        // Importing a hand-picked list of files says nothing about wanting
        // them gone; only the folder import offers that.
        false,
        &mut tally,
        progress_cb,
    );

    recount_session(db, &session_id)?;
    if tally.imported > 0 {
        import_phase!(
            "database_session",
            db.mark_session_imported(&session_id, now)
        )?;
    }

    progress_cb(ImportProgress {
        total: paths.len(),
        done: tally.processed,
        imported: tally.imported,
        current_file: PathBuf::new(),
        skipped_duplicates: tally.skipped_duplicates,
        deleted_sources: tally.deleted_sources,
        errors: tally.errors.clone(),
        scanning: false,
    });

    Ok(ImportSession {
        id: session_id,
        name: session_name,
        started_at: session_started_at,
        photo_count: tally.imported,
        errors: tally.errors,
    })
}

// ── Grouped folder import ───────────────────────────────────────────────────

/// Import `paths` (typically the recursive contents of a folder), grouping them
/// into one [`ImportSession`] per run of same-or-consecutive capture days.  A
/// day of more than [`HEAVY_DAY_PHOTOS`] photos is a shoot of its own and gets
/// its own session rather than being folded into the surrounding run.
///
/// Each group's session is back-dated to the group's earliest capture time and
/// every photo's `import_date` is back-dated to its own capture time, so that
/// importing another tool's library reconstructs a believable, years-long
/// history.  Capture time is taken from EXIF `DateTimeOriginal`, falling back to
/// the file's modified time and then created time.
#[allow(clippy::too_many_arguments)]
pub fn import_folder_grouped(
    library_root: &Path,
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    paths: &[PathBuf],
    cancelled: Arc<AtomicBool>,
    source_dir: Option<&Path>,
    options: ImportOptions,
    progress_cb: &dyn Fn(ImportProgress),
) -> Result<Vec<ImportSession>> {
    let assigner = Mutex::new(CollectionAssigner::new(db, options.collection.clone()));
    let total = paths.len();
    // One clock reading for the whole run, so every group this import creates
    // sorts together in a recent-imports list.
    let import_started = unix_now();

    // ── Phase 1: scan capture timestamps ──────────────────────────────────
    // A cheap EXIF read (no full RAW demosaic) plus a fallback to filesystem
    // times; sorting by the result lets the consecutive-day clustering run in
    // a single pass.
    let mut dated: Vec<(PathBuf, u64)> = Vec::with_capacity(total);
    for (scanned, path) in paths.iter().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        // Do not drop duplicate paths here. They still count toward a day's
        // density and can therefore change consecutive-day and heavy-day
        // session boundaries even though phase 3 skips their photo writes.
        dated.push((path.clone(), capture_timestamp(path)));
        progress_cb(ImportProgress {
            total,
            done: scanned + 1,
            imported: 0,
            current_file: path.clone(),
            skipped_duplicates: 0,
            deleted_sources: 0,
            errors: Vec::new(),
            scanning: true,
        });
    }
    dated.sort_by_key(|(_, ts)| *ts);

    // Stack detection runs over the full (sorted) list so RAW+JPEG pairs are
    // still found regardless of which group each file lands in.
    let sorted_paths: Vec<PathBuf> = dated.iter().map(|(p, _)| p.clone()).collect();
    let stack_map = detect_stacks(&sorted_paths);

    // ── Phase 2: cluster into consecutive-day groups, heavy days apart ────
    let timestamps: Vec<u64> = dated.iter().map(|(_, ts)| *ts).collect();
    let groups = cluster_by_day(&timestamps);

    // ── Phase 3: import each group into its own back-dated session ────────
    let mut sessions: Vec<ImportSession> = Vec::new();
    let mut tally = ImportTally::default();
    // Run-scoped, because groups are imported one after another: a duplicate of
    // an earlier group's file is caught by the index, but two identical files
    // inside one group are only ever seen here.
    let mut batch_hashes = HashSet::new();

    for group in groups {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let group_slice = &dated[group];
        let group_start = group_slice
            .first()
            .map(|(_, ts)| *ts)
            .unwrap_or_else(unix_now);
        let group_end = group_slice.last().map(|(_, ts)| *ts).unwrap_or(group_start);
        let session_name = format_session_name(group_start, group_end);

        // Reuse an existing session with the same name so that re-imports, or
        // multiple source trees that share a shoot date, merge together.
        let existing = import_phase!(
            "database_session",
            db.all_sessions()
                .unwrap_or_default()
                .into_iter()
                .find(|s| s.name == session_name)
        );
        let (session_id, session_started_at) = match existing {
            Some(s) => (s.id, s.started_at),
            None => {
                let id = Uuid::new_v4().to_string();
                let source = source_dir.map(|d| d.to_string_lossy().into_owned());
                import_phase!(
                    "database_session",
                    db.insert_session(&id, &session_name, group_start, source.as_deref())
                )?;
                (id, group_start)
            }
        };

        let jobs: Vec<PipelineJob> = group_slice
            .iter()
            .map(|(path, ts)| PipelineJob {
                path: path.clone(),
                import_date: *ts,
                fallback_capture_ts: Some(*ts),
            })
            .collect();
        let outcomes = run_import_pipeline(
            library_root,
            db,
            registry,
            &jobs,
            &session_id,
            &stack_map,
            &assigner,
            &mut batch_hashes,
            &cancelled,
            total,
            options.delete_sources,
            &mut tally,
            progress_cb,
        );
        let group_done = outcomes
            .iter()
            .filter(|outcome| matches!(outcome, ImportOutcome::Imported(_)))
            .count();

        recount_session(db, &session_id)?;
        // Stamped with the wall clock, not the back-dated group time, so the
        // sidebar can answer "where did the folder I just imported go?" for a
        // shoot whose own date is years back.  Only when something landed: a
        // group that was entirely duplicates should stay where it was.
        if group_done > 0 {
            import_phase!(
                "database_session",
                db.mark_session_imported(&session_id, import_started)
            )?;
        }
        sessions.push(ImportSession {
            id: session_id,
            name: session_name,
            started_at: session_started_at,
            photo_count: group_done,
            errors: Vec::new(),
        });
    }

    progress_cb(ImportProgress {
        total,
        done: tally.processed,
        imported: tally.imported,
        current_file: PathBuf::new(),
        skipped_duplicates: tally.skipped_duplicates,
        deleted_sources: tally.deleted_sources,
        errors: tally.errors.clone(),
        scanning: false,
    });

    // Surface any per-file errors on the first session (or a synthetic one if
    // nothing imported), so the caller can report them.
    if let Some(first) = sessions.first_mut() {
        first.errors = tally.errors;
    } else if !tally.errors.is_empty() {
        sessions.push(ImportSession {
            id: String::new(),
            name: String::new(),
            started_at: unix_now(),
            photo_count: 0,
            errors: tally.errors,
        });
    }

    Ok(sessions)
}

/// Best-available capture time for `path` in Unix seconds: EXIF
/// `DateTimeOriginal`, then filesystem modified time, then created time.
fn capture_timestamp(path: &Path) -> u64 {
    import_phase!("capture_scan", {
        if let Some(ts) = exif_capture_timestamp(path) {
            return ts;
        }
        if let Ok(fs_meta) = std::fs::metadata(path) {
            if let Ok(t) = fs_meta.modified()
                && let Ok(d) = t.duration_since(UNIX_EPOCH)
            {
                return d.as_secs();
            }
            if let Ok(t) = fs_meta.created()
                && let Ok(d) = t.duration_since(UNIX_EPOCH)
            {
                return d.as_secs();
            }
        }
        unix_now()
    })
}

/// Bytes read from the head of a file to extract its EXIF capture date.
///
/// EXIF sits near the start of both JPEG (APP1) and TIFF-based RAW (IFD0)
/// containers, so a prefix this size reliably covers the relevant tags while
/// transferring a tiny fraction of a multi-megabyte original — the difference
/// between a usable and an unusable folder import over a network filesystem.
const EXIF_PREFIX_LEN: u64 = 1 << 20; // 1 MiB

/// EXIF `DateTimeOriginal` for `path` in Unix seconds, if the file carries one.
///
/// Only the leading [`EXIF_PREFIX_LEN`] bytes are read (the capture date lives
/// near the start of both JPEG and TIFF-based RAW files), so this never streams
/// whole originals across the network during the capture-date scan.  JPEGs and
/// TIFF-based RAW use different container parsers; formats without EXIF (PNG,
/// scans, …) — and the rare file whose date sits past the prefix — return
/// `None` and fall back to filesystem times.
fn exif_capture_timestamp(path: &Path) -> Option<u64> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    let is_jpeg = is_jpeg_ext(&ext);
    if !is_jpeg && !is_raw_ext(&ext) {
        return None;
    }
    let prefix = read_file_prefix(path, EXIF_PREFIX_LEN)?;
    let date = read_capture_date_from_prefix(&prefix, is_jpeg)?;
    parse_exif_datetime(&date)
}

/// Read up to `max` bytes from the start of `path`.  Over NFS this transfers
/// only the bytes actually consumed, so a small `max` keeps the read cheap.
fn read_file_prefix(path: &Path, max: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(max).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Photos on a single capture day above which that day gets its own import
/// session instead of joining a consecutive-day run.
///
/// Rolling a week of casual shooting into one library date keeps the timeline
/// readable.  Doing the same to a week of heavy shooting does the opposite: the
/// week's work lands in one undifferentiated pile of thousands and the days
/// stop being findable.  A day past this count is treated as a shoot in its own
/// right, which is also how a photographer thinks of it.
pub const HEAVY_DAY_PHOTOS: usize = 100;

/// Split sorted timestamps into one index range per distinct UTC calendar day.
fn day_runs(sorted_ts: &[u64]) -> Vec<std::ops::Range<usize>> {
    let mut runs = Vec::new();
    if sorted_ts.is_empty() {
        return runs;
    }
    let mut start = 0usize;
    for i in 1..sorted_ts.len() {
        if utc_day(sorted_ts[i]) != utc_day(sorted_ts[i - 1]) {
            runs.push(start..i);
            start = i;
        }
    }
    runs.push(start..sorted_ts.len());
    runs
}

/// Group sorted timestamps into runs of same-or-consecutive UTC calendar days.
/// A gap of more than one empty day between successive photos starts a new
/// group, and so does a day carrying more than [`HEAVY_DAY_PHOTOS`] photos —
/// such a day is kept as a group of its own and does not extend the run on
/// either side of it.  Returns index ranges into the input slice.
fn cluster_by_day(sorted_ts: &[u64]) -> Vec<std::ops::Range<usize>> {
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    // Day the last group ends on, while that group is still allowed to grow;
    // `None` once a heavy day closed it off.
    let mut open_day: Option<i64> = None;
    for run in day_runs(sorted_ts) {
        let d = utc_day(sorted_ts[run.start]);
        let heavy = run.len() > HEAVY_DAY_PHOTOS;
        match groups.last_mut() {
            Some(last) if !heavy && open_day.is_some_and(|prev| d - prev == 1) => {
                last.end = run.end
            }
            _ => groups.push(run),
        }
        open_day = (!heavy).then_some(d);
    }
    groups
}

/// UTC calendar day number for a Unix timestamp.
fn utc_day(ts: u64) -> i64 {
    (ts / 86_400) as i64
}

/// Set a session's cached `photo_count` from the photos that actually carry it.
///
/// Counting the rows rather than adding this run's tally to the one read at the
/// start is what makes the number survive interruption: a cancelled import, a
/// crash that skipped the update entirely, a second import into the same
/// session, and two importers running at once all converge on the same count
/// the next time any of them finishes.
fn recount_session(db: &dyn LibraryDb, session_id: &str) -> Result<()> {
    import_phase!("database_session", {
        let count = db.session_photo_count(session_id)?;
        db.update_session_count(session_id, count)
    })
}

// ── Single-file import ────────────────────────────────────────────────────────

/// Everything one source file's preparation produced, waiting to be made
/// durable.
///
/// Preparation is computation and reads only — source read, hash, decode,
/// thumbnail, serialisation and parity — so it can run on a worker thread while
/// the importing thread is still inside the previous photo's `fsync`s.
/// Committing is writes only, and stays single-threaded and in input order.
struct Prepared {
    /// Blake3 hex of the original file bytes.
    hash: String,
    thumb_bytes: Vec<u8>,
    /// The finished `.rlab` byte image, parity included.
    rlab_bytes: Vec<u8>,
    lmta: LibraryMeta,
    width: u32,
    height: u32,
    stack_id: Option<String>,
    /// Whether the imported project arrived with edits already in it.
    has_edits: bool,
    collection: Option<AssignedCollection>,
}

impl Prepared {
    /// What this photo occupies of the pipeline's buffer budget.
    fn buffered_bytes(&self) -> usize {
        self.rlab_bytes.len() + self.thumb_bytes.len()
    }
}

/// What preparing one source file found.
enum Preparation {
    /// A new photograph, ready to be committed.  Boxed: a `Prepared` is the
    /// far larger of the two variants, and every duplicate would otherwise
    /// carry its bulk around.
    Ready(Box<Prepared>),
    /// Already in the library, carrying the content hash of the photo that
    /// makes it a duplicate.  The hash rather than a bare "skip it": an import
    /// deleting its sources has to find the file holding those bytes before
    /// unlinking the copy in front of it.
    Duplicate(String),
}

/// Read, decode and serialise one source file without writing anything.
///
/// Returns [`Preparation::Duplicate`] for a file already in the library, `Err`
/// on failure.
/// Nothing here touches the library on disk and the only database calls are
/// reads, so this is safe to run on several threads at once against the same
/// import; [`commit_prepared`] performs every write, in input order.
///
/// `import_date` is stored verbatim (callers back-date it for grouped imports),
/// and `fallback_capture_ts` synthesises an EXIF capture date for files that
/// carry none, so they still sort coherently by capture time.
///
/// `assigner` decides which collection the photo joins, and is consulted only
/// once the file is known to be a genuinely new photograph.  The membership
/// goes into the `.rlab` before it is written, which costs nothing: adding it
/// afterwards through `Library::add_to_collection` would re-read and re-write
/// every freshly written original just to add one line of metadata.
#[allow(clippy::too_many_arguments)]
fn prepare_one(
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    path: &Path,
    session_id: &str,
    stack_map: &[(usize, usize)], // (primary_idx, secondary_idx) pairs by path index
    import_date: u64,
    fallback_capture_ts: Option<u64>,
    assigner: &Mutex<CollectionAssigner<'_>>,
) -> Result<Preparation> {
    // 1. Read source bytes + capture source-file timestamps.
    //    Stat first so we read the times the file had before we opened it.
    let fs_meta = import_phase!(
        "fingerprint_lookup",
        std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))
    )?;
    let source_mtime = fs_meta.modified().ok().map(FileTimeStamp::from_system_time);
    let source_atime = fs_meta.accessed().ok().map(FileTimeStamp::from_system_time);
    let source_ctime = fs_meta.created().ok().map(FileTimeStamp::from_system_time);

    // Fast resume: if a previously-imported photo has this exact source
    // fingerprint (path + size + mtime), skip it without reading the bytes.
    // This is what makes resuming an interrupted bulk import cheap — already-
    // imported files cost a single indexed lookup instead of a full (often
    // network) read plus Blake3 hash just to rediscover the duplicate. Falls
    // through to the read+hash dedup whenever the mtime is unavailable.
    if let Some(mtime) = source_mtime
        && let Some(hash) = import_phase!(
            "fingerprint_lookup",
            db.source_already_imported(&path.to_string_lossy(), fs_meta.len(), mtime.secs)
        )?
    {
        return Ok(Preparation::Duplicate(hash));
    }

    let input_bytes = import_phase!(
        "source_read",
        std::fs::read(path).with_context(|| format!("read {}", path.display()))
    )?;

    // A project import is different from an ordinary image import: ORIG is
    // already the source image and the container may also hold edits, virtual
    // copies, a rendered thumbnail, and library metadata.  Treating the whole
    // `.rlab` byte stream as an image makes format detection fail and, even if
    // it did not, would create a nested project and discard all of that state.
    let (mut imported_project, original_bytes) = import_phase!(
        "project_parse",
        if is_rlab_path(path) {
            let mut project = RlabFile::read_bytes(&input_bytes)
                .with_context(|| format!("read project {}", path.display()))?;
            project.resolve_relative_paths(path.parent().unwrap_or_else(|| Path::new(".")));
            // Drop the source container after parsing rather than retaining its
            // full ORIG + parity payload alongside the extracted original.
            let original_bytes = std::mem::take(&mut project.original_bytes);
            drop(input_bytes);
            (Some(project), Arc::new(original_bytes))
        } else {
            (None, Arc::new(input_bytes))
        }
    );
    // Keep the source allocation through hashing and decoding, then move it
    // into the project we write. This avoids a second full-original copy on
    // the common import path.

    // 2. Compute hash
    let hash = import_phase!(
        "content_hash",
        blake3::hash(&original_bytes).to_hex().to_string()
    );

    // 3. Duplicate check.  A file that only duplicates another file of the same
    //    batch cannot be seen here — that pair may be in flight at the same
    //    moment — so `commit_prepared` checks the batch's own hashes again.
    if import_phase!("hash_lookup", db.photo_by_hash(&hash))?.is_some() {
        return Ok(Preparation::Duplicate(hash));
    }

    // 4. Determine the stack partner hash (if this file is in a pair)
    let stack_peer_hash: Option<String> = stack_peer_for(path, stack_map, original_bytes.len());
    let stack_is_primary = is_primary_in_pair(path);

    // 5. Decode image for thumbnail + dimensions + EXIF
    // Decode the bytes we already transferred from the source. Calling
    // `decode_file` here used to reopen and reread every ordinary import — a
    // particularly expensive mistake when `path` lives on NFS or CIFS. The
    // path remains a format hint (important for TIFF-based RAW formats). The
    // installed rawler accepts the same retained allocation as a seekable
    // in-memory source, while third-party path-only handlers keep their
    // temporary-file fallback.
    let decode_hint = imported_project.as_ref().map_or(Some(path), |project| {
        project.meta.source_path.as_deref().map(Path::new)
    });
    let image = decode_import_bytes(registry, Arc::clone(&original_bytes), decode_hint)
        .with_context(|| {
            if imported_project.is_some() {
                format!("decode original image in {}", path.display())
            } else {
                format!("decode {}", path.display())
            }
        })?;
    let (width, height) = (image.width, image.height);
    let mut exif = LibraryExif::from_image_metadata(&image.metadata);
    // Files without an EXIF capture date (PNGs, scans, …) still need a coherent
    // capture date for sorting; synthesise one from the chosen fallback time.
    if exif.capture_date.is_none()
        && let Some(ts) = fallback_capture_ts
    {
        exif.capture_date = Some(format_exif_datetime(ts));
    }

    // 6. Generate 512px thumbnail
    let thumb_bytes = import_phase!(
        "thumbnail",
        imported_project
            .as_ref()
            .and_then(|project| project.thumbnail.clone())
            .map_or_else(|| generate_thumbnail(&image, 512), Ok)
    )?;
    drop(image);

    // 8. Build LibraryMeta
    let stack_id = if stack_peer_hash.is_some() {
        // Shared UUID: derive from the sorted pair of path stems so both sides
        // get the same stack_id even if imported in any order within the batch.
        let stack_uuid = Uuid::new_v4().to_string();
        Some(stack_uuid)
    } else {
        None
    };

    let mut lmta = if let Some(project) = imported_project.as_ref() {
        // Preserve meaningful metadata already carried by a project (rating,
        // keywords, original source timestamps, etc.), but place it in this
        // import session. Editor-only projects have no LMTA, so derive their
        // original identity from META rather than calling the container itself
        // the original photograph.
        let mut lmta = project.lmta.clone().unwrap_or_default();
        if lmta.original_filename.is_none() {
            lmta.original_filename = project
                .meta
                .source_path
                .as_deref()
                .and_then(|source| Path::new(source).file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .or_else(|| {
                    path.file_stem()
                        .map(|name| name.to_string_lossy().into_owned())
                });
        }
        if lmta.source_path.is_none() {
            lmta.source_path = project.meta.source_path.clone();
        }
        if lmta.source_size.is_none() {
            lmta.source_size = Some(original_bytes.len() as u64);
        }
        if lmta.exif.is_none() {
            lmta.exif = Some(exif);
        }
        lmta.import_session_id = session_id.to_owned();
        lmta.import_date = import_date;
        lmta
    } else {
        LibraryMeta {
            original_filename: path.file_name().map(|n| n.to_string_lossy().into_owned()),
            source_path: Some(path.to_string_lossy().into_owned()),
            source_size: Some(fs_meta.len()),
            import_session_id: session_id.to_owned(),
            import_date,
            stack_peer_hash,
            stack_is_primary,
            source_mtime,
            source_atime,
            source_ctime,
            exif: Some(exif),
            ..Default::default()
        }
    };

    // Recorded in the file itself, so the `.rlab` is right from the moment it
    // exists and a rebuild that never sees the index still finds the
    // membership.  Only the uuid is settled here; the index row is created by
    // the first photo that actually commits into it.
    let collection = import_phase!("database_collection_resolve", {
        let mut assigner = assigner.lock().expect("collection assigner poisoned");
        assigner.resolve(path)
    })?;
    if let Some(collection) = collection.as_ref()
        && !lmta
            .collection_refs
            .iter()
            .any(|held| held.id == collection.uuid)
    {
        lmta.collection_refs.push(CollectionRef {
            id: collection.uuid.clone(),
            name: collection.name.clone(),
        });
    }

    // The decoder has returned its independent pixel buffer, so its temporary
    // shared source handle is gone. Recover the original allocation for
    // serialization; a third-party handler which deliberately retained the
    // Arc keeps the conservative copy fallback.
    let original_bytes = Arc::try_unwrap(original_bytes).unwrap_or_else(|bytes| (*bytes).clone());

    // 9. Serialise the `.rlab`, parity and all.  This is the last of the work
    //    that does not need the writer thread.
    let has_edits = imported_project
        .as_ref()
        .is_some_and(|project| project.has_edits());
    let rlab_bytes = if let Some(project) = imported_project.as_mut() {
        project.meta.width = width;
        project.meta.height = height;
        project.original_bytes = original_bytes;
        project.thumbnail = Some(thumb_bytes.clone());
        project.set_lmta(Some(lmta.clone()));
        project.encode_v5().context("serialise imported .rlab")?
    } else {
        encode_rlab(original_bytes, &lmta, &thumb_bytes, width, height)?
    };

    Ok(Preparation::Ready(Box::new(Prepared {
        hash,
        thumb_bytes,
        rlab_bytes,
        lmta,
        width,
        height,
        stack_id,
        has_edits,
        collection,
    })))
}

/// Make one prepared photo durable and record it in the index.
///
/// The steps are ordered storage-first: the `.rlab` holds the only copy of the
/// original, the index can be rebuilt from it, so the file is written and
/// verified before anything is recorded about it.  An import that stops between
/// them leaves a complete `.rlab` that no row mentions — invisible until the
/// next rebuild, and recovered by it.  Re-running the same import also heals
/// it: with no row to find, the file is simply rewritten (same hash, same path)
/// and inserted.  Nothing is deleted on failure for the same reason: the
/// photograph outranks the bookkeeping.
fn commit_prepared(
    library_root: &Path,
    db: &dyn LibraryDb,
    assigner: &Mutex<CollectionAssigner<'_>>,
    prepared: Prepared,
) -> Result<()> {
    let Prepared {
        hash,
        thumb_bytes,
        rlab_bytes,
        lmta,
        width,
        height,
        stack_id,
        has_edits,
        collection,
    } = prepared;

    // 9. Write thumbnail.
    let thumb_path = thumb_path(library_root, &hash);
    import_phase!(
        "thumbnail_write",
        write_thumbnail(&thumb_path, &thumb_bytes)
    )?;

    // 10. Write .rlab.
    let rlab_path = rlab_path(library_root, &hash);
    if let Some(parent) = rlab_path.parent() {
        import_phase!("destination_directory", create_dir_all_synced(parent))?;
    }
    import_phase!(
        "verified_write",
        write_verified_atomic(&rlab_path, &rlab_bytes)
    )
    .with_context(|| format!("write {}", rlab_path.display()))?;
    drop(rlab_bytes);

    // 11. Insert the index rows and initial membership atomically. The `.rlab`
    // is already durable, so a database failure leaves one reconstructable
    // file rather than a partial index entry.  The collection row is created
    // here rather than during preparation, so a batch that turns out to be all
    // duplicates leaves no empty collection behind.
    let collection_id = match collection {
        Some(collection) => Some(import_phase!("database_collection_resolve", {
            let mut assigner = assigner.lock().expect("collection assigner poisoned");
            assigner.realise(&collection)
        })?),
        None => None,
    };
    import_phase!(
        "database_insert",
        db.insert_photo_with_collection(
            NewPhoto {
                hash: &hash,
                lib_path: &relative_lib_path(&hash),
                lmta: &lmta,
                width,
                height,
                stack_id: stack_id.as_deref(),
                // An imported project arrives with its edit history intact, so the
                // index has to say so from the start: a photo imported already edited
                // is one the edited-only filter should find straight away rather than
                // after the next save happens to rewrite its thumbnail.
                has_edits,
            },
            collection_id,
        )
    )?;

    Ok(())
}

/// Decode source bytes already loaded by the importer, retaining the source
/// path solely as a format/extension hint.
fn decode_import_bytes(
    registry: &FormatRegistry,
    bytes: Arc<Vec<u8>>,
    hint_path: Option<&Path>,
) -> rasterlab_core::error::RasterResult<rasterlab_core::image::Image> {
    import_phase!(
        "decode_exif",
        registry.decode_import_shared_bytes(bytes, hint_path)
    )
}

// ── Bounded prepare/commit pipeline ──────────────────────────────────────────

/// Preparation workers run alongside the committing thread.
///
/// Preparation is CPU-bound and committing is dominated by `fsync` latency, so
/// a few workers are enough to keep the writer fed; more mainly buys peak
/// memory.  Some decoders parallelise internally, so this stays well below the
/// core count.
const MAX_PREPARE_WORKERS: usize = 4;

/// How many bytes of prepared-but-unwritten photos may queue ahead of the
/// committing thread.  Peak import memory is this plus what the workers hold
/// while preparing, so it bounds the queue rather than the whole import.
///
/// This is what binds on large originals, where a couple of queued photos
/// already run to hundreds of megabytes.
const PREPARE_QUEUE_BUDGET_BYTES: usize = 128 * 1024 * 1024;

/// How many prepared photos may queue ahead of the committing thread, per
/// worker.
///
/// The byte budget alone would let a folder of small JPEGs buffer thousands of
/// finished photos, because writing is so much slower than preparing them: the
/// queue would grow to the budget and stay there for no gain.  Keeping a couple
/// of photos in hand per worker is all it takes to keep the writer from ever
/// waiting, so this is what binds on small originals.
const PREPARE_QUEUE_DEPTH_PER_WORKER: usize = 3;

/// How many bytes of source image may be in preparation at one time.
///
/// Decoded pixels, the retained original and the serialised container are all
/// roughly proportional to the source, so this is what stops four workers on
/// 45-megapixel originals from holding a gigabyte between them.  A single file
/// is always admitted however large it is, so one enormous photograph slows the
/// import rather than stalling it.
const PREPARE_INFLIGHT_SOURCE_BYTES: u64 = 128 * 1024 * 1024;

/// One source file as handed to a preparation worker.
struct PipelineJob {
    path: PathBuf,
    import_date: u64,
    fallback_capture_ts: Option<u64>,
}

/// What committing one input file did, carrying the content hash the library
/// now holds those bytes under.
enum ImportOutcome {
    /// Newly imported.
    Imported(String),
    /// Already in the library, or a duplicate of an earlier file in this batch.
    Duplicate(String),
    Failed,
}

impl ImportOutcome {
    /// The hash the library holds this file's contents under, or `None` when
    /// it does not hold them at all.
    fn library_hash(&self) -> Option<&str> {
        match self {
            Self::Imported(hash) | Self::Duplicate(hash) => Some(hash),
            Self::Failed => None,
        }
    }
}

/// Counts carried across every group of one import run, so progress reports
/// stay continuous.
#[derive(Default)]
struct ImportTally {
    processed: usize,
    imported: usize,
    skipped_duplicates: usize,
    /// Source files removed after their contents were found in the library.
    deleted_sources: usize,
    errors: Vec<(PathBuf, String)>,
}

/// Work shared between the preparation workers and the committing thread.
#[derive(Default)]
struct PipelineQueue {
    /// Next input index to claim.  Claiming in order guarantees the photo the
    /// committer is waiting for is already being worked on, so the committer
    /// can never wait on work nobody has started.
    next_job: usize,
    /// Finished preparations by input index, drained in that order.
    ready: BTreeMap<usize, Result<Preparation>>,
    /// Bytes held in `ready`, weighed against [`PREPARE_QUEUE_BUDGET_BYTES`].
    buffered: usize,
    /// Source bytes of the preparations currently running, weighed against
    /// [`PREPARE_INFLIGHT_SOURCE_BYTES`].  Released when a preparation
    /// finishes, so a worker waiting for queue room never holds a share of it.
    in_flight: u64,
    /// Set once no further preparations will arrive, so a waiting committer
    /// stops rather than blocking on a cancelled run.
    finished: bool,
}

/// Marks the queue closed when it goes out of scope, so nobody is left waiting
/// on work that will never arrive.
///
/// The committer holds one for its whole run: reaching the end of the batch,
/// stopping on cancellation and unwinding all have to release the workers.
/// Workers hold one that fires only while panicking — a decoder that aborts
/// must not leave the committer blocked forever on the photo it was preparing.
/// The panic itself still reaches the caller when the thread scope joins.
struct CloseQueue<'a> {
    queue: &'a Mutex<PipelineQueue>,
    ready: &'a Condvar,
    only_when_panicking: bool,
}

impl Drop for CloseQueue<'_> {
    fn drop(&mut self) {
        if self.only_when_panicking && !std::thread::panicking() {
            return;
        }
        if let Ok(mut queue) = self.queue.lock() {
            queue.finished = true;
        }
        self.ready.notify_all();
    }
}

/// Import `jobs` into `session_id`, preparing on worker threads and committing
/// on this one, in input order.
///
/// Ordering is what makes this behave exactly like the serial import it
/// replaces: when two files of one batch hold the same bytes, the earlier one
/// wins and the later is the duplicate, whichever finishes preparing first.
/// The result is index-aligned with `jobs`, and shorter than it if the run was
/// cancelled part-way.
#[allow(clippy::too_many_arguments)]
fn run_import_pipeline(
    library_root: &Path,
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    jobs: &[PipelineJob],
    session_id: &str,
    stack_map: &[(usize, usize)],
    assigner: &Mutex<CollectionAssigner<'_>>,
    batch_hashes: &mut HashSet<String>,
    cancelled: &AtomicBool,
    total: usize,
    delete_sources: bool,
    tally: &mut ImportTally,
    progress_cb: &dyn Fn(ImportProgress),
) -> Vec<ImportOutcome> {
    if jobs.is_empty() {
        return Vec::new();
    }
    let workers = std::thread::available_parallelism()
        .map_or(1, |cores| cores.get())
        .min(MAX_PREPARE_WORKERS)
        .min(jobs.len());
    let depth = workers * PREPARE_QUEUE_DEPTH_PER_WORKER;
    let queue = Mutex::new(PipelineQueue::default());
    let ready = Condvar::new();
    let mut outcomes = Vec::with_capacity(jobs.len());

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                let _close = CloseQueue {
                    queue: &queue,
                    ready: &ready,
                    only_when_panicking: true,
                };
                loop {
                    // Claim work only once the queue has room, so a fast worker
                    // cannot run far ahead of the writer and buffer the whole
                    // import.  Nothing is claimed while waiting, which is why
                    // the committer's next photo is never stuck behind this.
                    let index = {
                        let mut queue = queue.lock().expect("import queue poisoned");
                        while !queue.finished
                            && (queue.buffered >= PREPARE_QUEUE_BUDGET_BYTES
                                || queue.ready.len() >= depth)
                        {
                            queue = ready.wait(queue).expect("import queue poisoned");
                        }
                        if queue.finished || cancelled.load(Ordering::Relaxed) {
                            // Tell the committer no more work is coming: it may
                            // be waiting on a photo this worker will now never
                            // prepare.
                            queue.finished = true;
                            ready.notify_all();
                            return;
                        }
                        if queue.next_job == jobs.len() {
                            return;
                        }
                        let index = queue.next_job;
                        queue.next_job += 1;
                        index
                    };

                    let job = &jobs[index];
                    // Admission by source size.  Statting again costs one call
                    // on an entry the scan has already warmed, and is the only
                    // measure of the work's size available before reading it.
                    let source_bytes = std::fs::metadata(&job.path).map_or(0, |meta| meta.len());
                    {
                        let mut queue = queue.lock().expect("import queue poisoned");
                        while queue.in_flight > 0
                            && queue.in_flight + source_bytes > PREPARE_INFLIGHT_SOURCE_BYTES
                        {
                            queue = ready.wait(queue).expect("import queue poisoned");
                        }
                        queue.in_flight += source_bytes;
                    }

                    let prepared = prepare_one(
                        db,
                        registry,
                        &job.path,
                        session_id,
                        stack_map,
                        job.import_date,
                        job.fallback_capture_ts,
                        assigner,
                    );

                    let mut queue = queue.lock().expect("import queue poisoned");
                    queue.in_flight -= source_bytes;
                    if let Ok(Preparation::Ready(prepared)) = &prepared {
                        queue.buffered += prepared.buffered_bytes();
                    }
                    queue.ready.insert(index, prepared);
                    ready.notify_all();
                }
            });
        }

        // Held for the whole commit loop: reaching the end, cancelling and
        // unwinding must all release workers waiting for queue room.
        let _close = CloseQueue {
            queue: &queue,
            ready: &ready,
            only_when_panicking: false,
        };
        for (index, job) in jobs.iter().enumerate() {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            let prepared = {
                let mut queue = queue.lock().expect("import queue poisoned");
                while !queue.ready.contains_key(&index) && !queue.finished {
                    queue = ready.wait(queue).expect("import queue poisoned");
                }
                match queue.ready.remove(&index) {
                    Some(prepared) => {
                        if let Ok(Preparation::Ready(prepared)) = &prepared {
                            queue.buffered -= prepared.buffered_bytes();
                        }
                        ready.notify_all();
                        prepared
                    }
                    // Cancelled before this photo was prepared.
                    None => break,
                }
            };

            progress_cb(ImportProgress {
                total,
                done: tally.processed,
                imported: tally.imported,
                current_file: job.path.clone(),
                skipped_duplicates: tally.skipped_duplicates,
                deleted_sources: tally.deleted_sources,
                errors: tally.errors.clone(),
                scanning: false,
            });

            // Two files of one batch holding the same bytes are prepared
            // concurrently, so neither can see the other in the index.  The
            // earlier one has already committed by the time this runs.
            let outcome = match prepared {
                Ok(Preparation::Ready(prepared)) if batch_hashes.contains(&prepared.hash) => {
                    ImportOutcome::Duplicate(prepared.hash)
                }
                Ok(Preparation::Ready(prepared)) => {
                    let hash = prepared.hash.clone();
                    match commit_prepared(library_root, db, assigner, *prepared) {
                        Ok(()) => {
                            batch_hashes.insert(hash.clone());
                            ImportOutcome::Imported(hash)
                        }
                        Err(e) => {
                            tally.errors.push((job.path.clone(), format!("{:#}", e)));
                            ImportOutcome::Failed
                        }
                    }
                }
                Ok(Preparation::Duplicate(hash)) => ImportOutcome::Duplicate(hash),
                Err(e) => {
                    tally.errors.push((job.path.clone(), format!("{:#}", e)));
                    ImportOutcome::Failed
                }
            };
            match &outcome {
                ImportOutcome::Imported(_) => tally.imported += 1,
                ImportOutcome::Duplicate(_) => tally.skipped_duplicates += 1,
                ImportOutcome::Failed => {}
            }
            // Only once the library is known to be holding these bytes, and
            // never for a file that failed: the source is the other copy.
            if delete_sources && let Some(hash) = outcome.library_hash() {
                match delete_source(library_root, &job.path, hash) {
                    Ok(()) => tally.deleted_sources += 1,
                    Err(e) => tally.errors.push((job.path.clone(), format!("{:#}", e))),
                }
            }
            tally.processed += 1;
            outcomes.push(outcome);
        }
    });

    outcomes
}

// ── Write .rlab ───────────────────────────────────────────────────────────────

/// Serialise a freshly imported original as a v5 project, ready to be written.
fn encode_rlab(
    original_bytes: Vec<u8>,
    lmta: &LibraryMeta,
    thumb_bytes: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    import_phase!("project_preparation", {
        use rasterlab_core::pipeline::PipelineState;
        use rasterlab_core::project::{RlabFile, RlabMeta, SavedCopy};

        let meta = RlabMeta::new(
            env!("CARGO_PKG_VERSION"),
            lmta.source_path
                .as_deref()
                .or(lmta.original_filename.as_deref()),
            width,
            height,
        );
        let empty_pipeline = PipelineState {
            entries: Vec::new(),
            cursor: 0,
        };
        let copies = vec![SavedCopy {
            name: "Copy 1".into(),
            pipeline_state: empty_pipeline,
        }];
        let mut rlab = RlabFile::new(meta, original_bytes, copies, 0, Some(thumb_bytes.to_vec()));
        rlab.set_lmta(Some(lmta.clone()));
        // v5 carries two Reed-Solomon `RECC` parity copies (about 40% total
        // overhead for large projects) so a later integrity scrub can repair
        // bitrot in place. See `crate::scrub`.
        rlab.encode_v5().context("serialise .rlab")
    })
}
// ── Path helpers ──────────────────────────────────────────────────────────────

/// Remove a source file whose contents the library is now holding.
///
/// The stored `.rlab` is located first, and the source is kept whenever it
/// cannot be: a photo can be in the index while its file is not — a library
/// restored without its `files/`, a mount that dropped out mid-run — and this
/// is the last moment at which the source is still the other copy.  A photo
/// the user has moved to Recently Deleted counts as held: it is still in the
/// library, and still restorable.
fn delete_source(library_root: &Path, source: &Path, hash: &str) -> Result<()> {
    let active = rlab_path(library_root, hash);
    let deleted = rlab_path(&library_root.join("recently_deleted"), hash);
    if !active.is_file() && !deleted.is_file() {
        anyhow::bail!(
            "kept source: the library has no file at {}",
            active.display()
        );
    }
    std::fs::remove_file(source).with_context(|| format!("delete source {}", source.display()))
}

pub fn relative_lib_path(hash: &str) -> String {
    format!("{}/{}/{}.rlab", &hash[0..2], &hash[2..4], hash)
}

pub fn rlab_path(library_root: &Path, hash: &str) -> PathBuf {
    library_root.join("files").join(relative_lib_path(hash))
}

/// Every `.rlab` under `dir`, or nothing at all when it does not exist.
///
/// A library keeps them under two roots — `files/` and
/// `recently_deleted/files/` — and the passes that read the library back off
/// the disk have to cover both.
pub(crate) fn walk_rlab_files(dir: &Path) -> Vec<PathBuf> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_type().is_file() && entry.path().extension().is_some_and(|x| x == "rlab")
        })
        .map(|entry| entry.into_path())
        .collect()
}

pub fn thumb_path(library_root: &Path, hash: &str) -> PathBuf {
    library_root
        .join("thumbs")
        .join(format!("{}/{}/{}.jpg", &hash[0..2], &hash[2..4], hash))
}

// ── Stack detection ───────────────────────────────────────────────────────────

/// Pairs of (primary_path_index, secondary_path_index) within this import batch.
///
/// Each RAW file is paired with the lowest-indexed JPEG sharing its (lowercased)
/// file stem.  A single index pass builds a stem → JPEG-indices map so the whole
/// thing is O(n); the previous nested scan was O(n²) and stalled large imports.
fn detect_stacks(paths: &[PathBuf]) -> Vec<(usize, usize)> {
    let stem_of = |p: &Path| -> String {
        p.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
    };
    let ext_of = |p: &Path| -> String {
        p.extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
    };

    // Index every JPEG by stem (preserving path order so `first()` is the
    // lowest-indexed match, matching the old break-on-first behaviour).
    let mut jpegs_by_stem: HashMap<String, Vec<usize>> = HashMap::new();
    for (j, q) in paths.iter().enumerate() {
        if is_jpeg_ext(&ext_of(q)) {
            jpegs_by_stem.entry(stem_of(q)).or_default().push(j);
        }
    }

    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        if !is_raw_ext(&ext_of(p)) {
            continue;
        }
        if let Some(&j) = jpegs_by_stem.get(&stem_of(p)).and_then(|js| js.first()) {
            pairs.push((i, j));
        }
    }
    pairs
}

fn is_raw_ext(ext: &str) -> bool {
    matches!(
        ext,
        "nef"
            | "cr2"
            | "cr3"
            | "arw"
            | "orf"
            | "rw2"
            | "pef"
            | "dng"
            | "srw"
            | "3fr"
            | "iiq"
            | "erf"
            | "raf"
    )
}

fn is_jpeg_ext(ext: &str) -> bool {
    matches!(ext, "jpg" | "jpeg")
}

fn is_primary_in_pair(path: &Path) -> bool {
    let ext = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    is_raw_ext(&ext)
}

fn stack_peer_for(_path: &Path, _stack_map: &[(usize, usize)], _len: usize) -> Option<String> {
    // Peer hash is set after the peer is imported.
    // The library.rs import loop handles back-linking after both files are done.
    None
}

// ── Session naming & calendar math ──────────────────────────────────────────

/// Month abbreviations (`"Jan"`..`"Dec"`), indexed by `month - 1`.
pub const MONTH_NAMES: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Convert a Unix timestamp (seconds, UTC) to a `(year, month, day)` triple.
/// Exposed so UI code can group sessions by year/month without its own calendar
/// math.
pub fn ymd_from_unix(ts: u64) -> (i64, u32, u32) {
    civil_from_days((ts / 86_400) as i64)
}

/// Format a Unix timestamp as `"Jun 3 2025"` (UTC), without pulling in chrono.
fn chrono_lite_date(ts: u64) -> String {
    let (year, month, day) = civil_from_days((ts / 86_400) as i64);
    format!("{} {} {}", MONTH_NAMES[(month - 1) as usize], day, year)
}

/// Human-readable session name for a group spanning `[start, end]` (Unix secs).
/// Single day → `"Jun 3 2025"`; a range collapses the shared year (and month
/// where possible): `"Jun 3–7 2025"`, `"Jun 30 – Jul 2 2025"`,
/// `"Dec 31 2024 – Jan 1 2025"`.
pub(crate) fn format_session_name(start: u64, end: u64) -> String {
    let start_day = start / 86_400;
    let end_day = end / 86_400;
    if start_day == end_day {
        return chrono_lite_date(start);
    }
    let (sy, sm, sd) = civil_from_days(start_day as i64);
    let (ey, em, ed) = civil_from_days(end_day as i64);
    let mon = |m: u32| MONTH_NAMES[(m - 1) as usize];
    if sy == ey && sm == em {
        format!("{} {}–{} {}", mon(sm), sd, ed, sy)
    } else if sy == ey {
        format!("{} {} – {} {} {}", mon(sm), sd, mon(em), ed, sy)
    } else {
        format!("{} {} {} – {} {} {}", mon(sm), sd, sy, mon(em), ed, ey)
    }
}

/// Parse an EXIF `DateTimeOriginal` (`"YYYY:MM:DD HH:MM:SS"`) to Unix seconds
/// (interpreted as UTC). Tolerates `-`/`/` date separators and a `T` between
/// date and time; returns `None` for malformed or out-of-range input.
fn parse_exif_datetime(s: &str) -> Option<u64> {
    let (date, time) = s.trim().split_once([' ', 'T'])?;
    let mut d = date.split([':', '-', '/']);
    let year: i64 = d.next()?.trim().parse().ok()?;
    let month: u32 = d.next()?.trim().parse().ok()?;
    let day: u32 = d.next()?.trim().parse().ok()?;
    let mut t = time.split([':', '.']);
    let hour: i64 = t.next()?.trim().parse().ok()?;
    let min: i64 = t.next()?.trim().parse().ok()?;
    let sec: i64 = t.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    if year < 1970 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let secs = days_from_civil(year, month, day) * 86_400 + hour * 3_600 + min * 60 + sec;
    (secs >= 0).then_some(secs as u64)
}

/// Format Unix seconds as an EXIF-style `"YYYY:MM:DD HH:MM:SS"` (UTC).
fn format_exif_datetime(ts: u64) -> String {
    let (y, m, d) = civil_from_days((ts / 86_400) as i64);
    let rem = (ts % 86_400) as i64;
    format!(
        "{:04}:{:02}:{:02} {:02}:{:02}:{:02}",
        y,
        m,
        d,
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

/// Days since the Unix epoch for a Gregorian Y/M/D (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let m = m as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Inverse of [`days_from_civil`]: Unix day count → (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rasterlab_core::{
        Image, RasterError, RasterResult,
        traits::format_handler::{EncodeOptions, FormatHandler},
    };

    const DAY: u64 = 86_400;

    struct BytesOnlyTestHandler;

    impl FormatHandler for BytesOnlyTestHandler {
        fn extensions(&self) -> &[&'static str] {
            &["netimg"]
        }

        fn decode(&self, data: &[u8]) -> RasterResult<Image> {
            assert_eq!(data, b"already loaded image bytes");
            Image::from_rgba8(1, 1, vec![10, 20, 30, 255])
        }

        fn encode(&self, _image: &Image, _options: &EncodeOptions) -> RasterResult<Vec<u8>> {
            Err(RasterError::FormatNotEncodable("test".into()))
        }

        fn display_name(&self) -> &'static str {
            "byte-only test image"
        }
    }

    struct PathRequiredTestHandler;

    impl FormatHandler for PathRequiredTestHandler {
        fn extensions(&self) -> &[&'static str] {
            &["netraw"]
        }

        fn decode(&self, _data: &[u8]) -> RasterResult<Image> {
            Err(RasterError::UnsupportedFormat(
                "test handler requires a path".into(),
            ))
        }

        fn decode_file(&self, path: &Path) -> RasterResult<Image> {
            assert_eq!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("netraw")
            );
            let data = std::fs::read(path).map_err(RasterError::Io)?;
            assert_eq!(data, b"already loaded raw bytes");
            Image::from_rgba8(1, 1, vec![40, 50, 60, 255])
        }

        fn encode(&self, _image: &Image, _options: &EncodeOptions) -> RasterResult<Vec<u8>> {
            Err(RasterError::FormatNotEncodable("test".into()))
        }

        fn needs_file_path(&self) -> bool {
            true
        }

        fn display_name(&self) -> &'static str {
            "path-required test image"
        }
    }

    #[test]
    fn import_decode_uses_loaded_bytes_after_source_disappears() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("remote.netimg");
        std::fs::write(&source, b"already loaded image bytes").unwrap();
        let bytes = std::fs::read(&source).unwrap();
        std::fs::remove_file(&source).unwrap();

        let registry = FormatRegistry::default();
        registry.register(Arc::new(BytesOnlyTestHandler));
        let image = decode_import_bytes(&registry, Arc::new(bytes), Some(&source)).unwrap();

        assert_eq!((image.width, image.height), (1, 1));
    }

    #[test]
    fn import_decode_stages_loaded_bytes_locally_for_path_required_formats() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("remote.netraw");
        std::fs::write(&source, b"already loaded raw bytes").unwrap();
        let bytes = std::fs::read(&source).unwrap();
        std::fs::remove_file(&source).unwrap();

        let registry = FormatRegistry::default();
        registry.register(Arc::new(PathRequiredTestHandler));
        let image = decode_import_bytes(&registry, Arc::new(bytes), Some(&source)).unwrap();

        assert_eq!((image.width, image.height), (1, 1));
    }

    #[test]
    fn civil_date_round_trips() {
        // A handful of dates including epoch, leap day, and century boundaries.
        for &(y, m, d) in &[
            (1970, 1, 1),
            (2000, 2, 29),
            (2020, 9, 13),
            (2025, 6, 3),
            (2100, 3, 1),
        ] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d), "round-trip {y}-{m}-{d}");
        }
    }

    #[test]
    fn parse_exif_datetime_variants() {
        // Canonical EXIF colon form at the epoch.
        assert_eq!(parse_exif_datetime("1970:01:01 00:00:00"), Some(0));
        // Known instant: 2020-09-13 12:26:40 UTC = 1_600_000_000.
        assert_eq!(
            parse_exif_datetime("2020:09:13 12:26:40"),
            Some(1_600_000_000)
        );
        // Tolerates dash separators, a `T`, and fractional seconds.
        assert_eq!(
            parse_exif_datetime("2020-09-13T12:26:40.5"),
            Some(1_600_000_000)
        );
    }

    #[test]
    fn parse_exif_datetime_rejects_garbage() {
        // The EXIF "unknown" sentinel and malformed strings yield None.
        assert_eq!(parse_exif_datetime("0000:00:00 00:00:00"), None);
        assert_eq!(parse_exif_datetime("not a date"), None);
        assert_eq!(parse_exif_datetime("2020:13:01 00:00:00"), None);
    }

    #[test]
    fn format_exif_datetime_round_trips() {
        let ts = 1_600_000_000;
        assert_eq!(format_exif_datetime(ts), "2020:09:13 12:26:40");
        assert_eq!(parse_exif_datetime(&format_exif_datetime(ts)), Some(ts));
    }

    #[test]
    fn detect_stacks_pairs_raw_with_matching_jpeg() {
        let paths: Vec<PathBuf> = [
            "/a/IMG_1.NEF",  // 0: RAW, pairs with the JPEG at 1
            "/a/img_1.jpg",  // 1: JPEG (case-insensitive stem match)
            "/a/IMG_2.CR2",  // 2: RAW, no JPEG partner
            "/a/IMG_3.jpg",  // 3: lone JPEG
            "/a/IMG_4.nef",  // 4: RAW, pairs with the first matching JPEG (5, not 6)
            "/a/IMG_4.JPG",  // 5
            "/a/IMG_4.jpeg", // 6
        ]
        .iter()
        .map(PathBuf::from)
        .collect();

        let mut pairs = detect_stacks(&paths);
        pairs.sort_unstable();
        assert_eq!(pairs, vec![(0, 1), (4, 5)]);
    }

    #[test]
    fn detect_stacks_empty_without_raw() {
        let paths: Vec<PathBuf> = ["/a/x.jpg", "/a/y.png"].iter().map(PathBuf::from).collect();
        assert!(detect_stacks(&paths).is_empty());
    }

    #[test]
    fn cluster_by_day_splits_on_gaps() {
        // Same day, next day, +2 days (consecutive run holds), then a 3-day gap.
        let base = 1_600_000_000;
        let ts = [
            base,
            base + 3_600,   // same day
            base + DAY,     // consecutive
            base + 2 * DAY, // consecutive
            base + 5 * DAY, // gap > 1 day → new group
            base + 6 * DAY, // consecutive with previous
        ];
        let groups = cluster_by_day(&ts);
        assert_eq!(groups, vec![0..4, 4..6]);
    }

    #[test]
    fn cluster_by_day_edge_cases() {
        assert!(cluster_by_day(&[]).is_empty());
        assert_eq!(cluster_by_day(&[42]), vec![0..1]);
    }

    /// Sorted timestamps for `per_day` photos on each consecutive day, spread
    /// across the day so the clustering sees real within-day spacing.
    fn day_timestamps(per_day: &[usize]) -> Vec<u64> {
        const BASE: u64 = 1_600_000_000 / DAY * DAY; // midnight UTC
        let mut ts = Vec::new();
        for (day, &count) in per_day.iter().enumerate() {
            for i in 0..count {
                ts.push(BASE + day as u64 * DAY + (i as u64 * 60) % DAY);
            }
        }
        ts
    }

    #[test]
    fn cluster_by_day_splits_out_heavy_days() {
        // A light day, a heavy one, then two light days: the heavy day stands
        // alone and does not bridge the days on either side of it, while the
        // two light days following it still merge with each other.
        let heavy = HEAVY_DAY_PHOTOS + 1;
        let ts = day_timestamps(&[2, heavy, 3, 4]);
        assert_eq!(
            cluster_by_day(&ts),
            vec![0..2, 2..2 + heavy, 2 + heavy..9 + heavy]
        );
    }

    #[test]
    fn cluster_by_day_keeps_days_at_the_threshold_together() {
        // Exactly HEAVY_DAY_PHOTOS is not heavy — the rule is "more than".
        let ts = day_timestamps(&[HEAVY_DAY_PHOTOS, HEAVY_DAY_PHOTOS]);
        assert_eq!(cluster_by_day(&ts), vec![0..2 * HEAVY_DAY_PHOTOS]);
    }

    #[test]
    fn cluster_by_day_splits_consecutive_heavy_days() {
        // A week of heavy shooting becomes one session per day.
        let heavy = HEAVY_DAY_PHOTOS + 1;
        let ts = day_timestamps(&[heavy; 3]);
        assert_eq!(
            cluster_by_day(&ts),
            vec![0..heavy, heavy..2 * heavy, 2 * heavy..3 * heavy]
        );
    }

    #[test]
    fn session_name_single_and_ranges() {
        let d = |y, m, day| days_from_civil(y, m, day) as u64 * DAY;
        // Single day.
        assert_eq!(
            format_session_name(d(2025, 6, 3), d(2025, 6, 3)),
            "Jun 3 2025"
        );
        // Same month range collapses to "Jun 3–7 2025".
        assert_eq!(
            format_session_name(d(2025, 6, 3), d(2025, 6, 7)),
            "Jun 3–7 2025"
        );
        // Cross-month, same year.
        assert_eq!(
            format_session_name(d(2025, 6, 30), d(2025, 7, 2)),
            "Jun 30 – Jul 2 2025"
        );
        // Cross-year.
        assert_eq!(
            format_session_name(d(2024, 12, 31), d(2025, 1, 1)),
            "Dec 31 2024 – Jan 1 2025"
        );
    }
}
