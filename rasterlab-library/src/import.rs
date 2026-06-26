use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rasterlab_core::{
    formats::{FormatRegistry, exif_util::read_capture_date_from_prefix},
    library_meta::{FileTimeStamp, LibraryExif, LibraryMeta},
};
use uuid::Uuid;

use crate::{db_trait::LibraryDb, library::ImportProgress, thumbnail::generate_thumbnail};

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

    let existing = db
        .all_sessions()
        .unwrap_or_default()
        .into_iter()
        .find(|s| s.name == session_name);
    let (session_id, existing_count, session_started_at) = match existing {
        Some(s) => (s.id, s.photo_count, s.started_at),
        None => {
            let id = Uuid::new_v4().to_string();
            db.insert_session(&id, &session_name, now, None)?;
            (id, 0, now)
        }
    };

    progress_cb(ImportProgress {
        total: paths.len(),
        done: 0,
        imported: 0,
        current_file: PathBuf::new(),
        skipped_duplicates: 0,
        errors: Vec::new(),
        scanning: false,
    });

    // Detect RAW+JPEG stacks within this batch before importing.
    let stack_map = detect_stacks(paths);

    let mut processed = 0usize;
    let mut imported = 0usize;
    let mut skipped_duplicates = 0usize;
    let mut errors: Vec<(PathBuf, String)> = Vec::new();
    let mut imported_hashes: Vec<(PathBuf, String)> = Vec::new();

    for path in paths {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }

        progress_cb(ImportProgress {
            total: paths.len(),
            done: processed,
            imported,
            current_file: path.clone(),
            skipped_duplicates,
            errors: errors.clone(),
            scanning: false,
        });

        match import_one(
            library_root,
            db,
            registry,
            path,
            &session_id,
            &stack_map,
            now,
            None,
        ) {
            Ok(None) => {
                skipped_duplicates += 1;
            }
            Ok(Some(hash)) => {
                imported_hashes.push((path.clone(), hash));
                imported += 1;
            }
            Err(e) => {
                errors.push((path.clone(), format!("{:#}", e)));
            }
        }
        processed += 1;
    }

    db.update_session_count(&session_id, existing_count + imported as i64)?;

    progress_cb(ImportProgress {
        total: paths.len(),
        done: processed,
        imported,
        current_file: PathBuf::new(),
        skipped_duplicates,
        errors: errors.clone(),
        scanning: false,
    });

    Ok(ImportSession {
        id: session_id,
        name: session_name,
        started_at: session_started_at,
        photo_count: imported,
        errors,
    })
}

// ── Grouped folder import ───────────────────────────────────────────────────

/// Import `paths` (typically the recursive contents of a folder), grouping them
/// into one [`ImportSession`] per run of same-or-consecutive capture days.
///
/// Each group's session is back-dated to the group's earliest capture time and
/// every photo's `import_date` is back-dated to its own capture time, so that
/// importing another tool's library reconstructs a believable, years-long
/// history.  Capture time is taken from EXIF `DateTimeOriginal`, falling back to
/// the file's modified time and then created time.
pub fn import_folder_grouped(
    library_root: &Path,
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    paths: &[PathBuf],
    cancelled: Arc<AtomicBool>,
    source_dir: Option<&Path>,
    progress_cb: &dyn Fn(ImportProgress),
) -> Result<Vec<ImportSession>> {
    let total = paths.len();

    // ── Phase 1: scan capture timestamps ──────────────────────────────────
    // A cheap EXIF read (no full RAW demosaic) plus a fallback to filesystem
    // times; sorting by the result lets the consecutive-day clustering run in
    // a single pass.
    let mut dated: Vec<(PathBuf, u64)> = Vec::with_capacity(total);
    for (scanned, path) in paths.iter().enumerate() {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        dated.push((path.clone(), capture_timestamp(path)));
        progress_cb(ImportProgress {
            total,
            done: scanned + 1,
            imported: 0,
            current_file: path.clone(),
            skipped_duplicates: 0,
            errors: Vec::new(),
            scanning: true,
        });
    }
    dated.sort_by_key(|(_, ts)| *ts);

    // Stack detection runs over the full (sorted) list so RAW+JPEG pairs are
    // still found regardless of which group each file lands in.
    let sorted_paths: Vec<PathBuf> = dated.iter().map(|(p, _)| p.clone()).collect();
    let stack_map = detect_stacks(&sorted_paths);

    // ── Phase 2: cluster into consecutive-day groups ──────────────────────
    let timestamps: Vec<u64> = dated.iter().map(|(_, ts)| *ts).collect();
    let groups = cluster_by_day(&timestamps);

    // ── Phase 3: import each group into its own back-dated session ────────
    let mut sessions: Vec<ImportSession> = Vec::new();
    let mut processed = 0usize;
    let mut imported = 0usize;
    let mut skipped_duplicates = 0usize;
    let mut errors: Vec<(PathBuf, String)> = Vec::new();

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
        let existing = db
            .all_sessions()
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.name == session_name);
        let (session_id, existing_count, session_started_at) = match existing {
            Some(s) => (s.id, s.photo_count, s.started_at),
            None => {
                let id = Uuid::new_v4().to_string();
                let source = source_dir.map(|d| d.to_string_lossy().into_owned());
                db.insert_session(&id, &session_name, group_start, source.as_deref())?;
                (id, 0, group_start)
            }
        };

        let mut group_done = 0usize;
        for (path, ts) in group_slice {
            if cancelled.load(Ordering::Relaxed) {
                break;
            }
            progress_cb(ImportProgress {
                total,
                done: processed,
                imported,
                current_file: path.clone(),
                skipped_duplicates,
                errors: errors.clone(),
                scanning: false,
            });
            match import_one(
                library_root,
                db,
                registry,
                path,
                &session_id,
                &stack_map,
                *ts,
                Some(*ts),
            ) {
                Ok(None) => skipped_duplicates += 1,
                Ok(Some(_)) => {
                    group_done += 1;
                    imported += 1;
                }
                Err(e) => errors.push((path.clone(), format!("{:#}", e))),
            }
            processed += 1;
        }

        db.update_session_count(&session_id, existing_count + group_done as i64)?;
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
        done: processed,
        imported,
        current_file: PathBuf::new(),
        skipped_duplicates,
        errors: errors.clone(),
        scanning: false,
    });

    // Surface any per-file errors on the first session (or a synthetic one if
    // nothing imported), so the caller can report them.
    if let Some(first) = sessions.first_mut() {
        first.errors = errors;
    } else if !errors.is_empty() {
        sessions.push(ImportSession {
            id: String::new(),
            name: String::new(),
            started_at: unix_now(),
            photo_count: 0,
            errors,
        });
    }

    Ok(sessions)
}

/// Best-available capture time for `path` in Unix seconds: EXIF
/// `DateTimeOriginal`, then filesystem modified time, then created time.
fn capture_timestamp(path: &Path) -> u64 {
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

/// Group sorted timestamps into runs of same-or-consecutive UTC calendar days.
/// A gap of more than one empty day between successive photos starts a new
/// group.  Returns index ranges into the input slice.
fn cluster_by_day(sorted_ts: &[u64]) -> Vec<std::ops::Range<usize>> {
    let mut groups = Vec::new();
    if sorted_ts.is_empty() {
        return groups;
    }
    let day = |ts: u64| (ts / 86_400) as i64;
    let mut start = 0usize;
    let mut prev_day = day(sorted_ts[0]);
    for (i, &ts) in sorted_ts.iter().enumerate().skip(1) {
        let d = day(ts);
        if d - prev_day > 1 {
            groups.push(start..i);
            start = i;
        }
        prev_day = d;
    }
    groups.push(start..sorted_ts.len());
    groups
}

// ── Single-file import ────────────────────────────────────────────────────────

/// Returns `Ok(Some(hash))` on success, `Ok(None)` if duplicate, `Err` on failure.
///
/// `import_date` is stored verbatim (callers back-date it for grouped imports),
/// and `fallback_capture_ts` synthesises an EXIF capture date for files that
/// carry none, so they still sort coherently by capture time.
#[allow(clippy::too_many_arguments)]
fn import_one(
    library_root: &Path,
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    path: &Path,
    session_id: &str,
    stack_map: &[(usize, usize)], // (primary_idx, secondary_idx) pairs by path index
    import_date: u64,
    fallback_capture_ts: Option<u64>,
) -> Result<Option<String>> {
    // 1. Read source bytes + capture source-file timestamps.
    //    Stat first so we read the times the file had before we opened it.
    let fs_meta = std::fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
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
        && db.source_already_imported(&path.to_string_lossy(), fs_meta.len(), mtime.secs)?
    {
        return Ok(None);
    }

    let original_bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;

    // 2. Compute hash
    let hash = blake3::hash(&original_bytes).to_hex().to_string();

    // 3. Duplicate check
    if db.photo_by_hash(&hash)?.is_some() {
        return Ok(None);
    }

    // 4. Determine the stack partner hash (if this file is in a pair)
    let stack_peer_hash: Option<String> = stack_peer_for(path, stack_map, original_bytes.len());
    let stack_is_primary = is_primary_in_pair(path);

    // 5. Decode image for thumbnail + dimensions + EXIF
    let image = registry
        .decode_file(path)
        .with_context(|| format!("decode {}", path.display()))?;
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
    let thumb_bytes = generate_thumbnail(&image, 512)?;

    // 8. Build LibraryMeta
    let stack_id = if stack_peer_hash.is_some() {
        // Shared UUID: derive from the sorted pair of path stems so both sides
        // get the same stack_id even if imported in any order within the batch.
        let stack_uuid = Uuid::new_v4().to_string();
        Some(stack_uuid)
    } else {
        None
    };

    let lmta = LibraryMeta {
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
    };

    // 9. Write thumbnail
    let thumb_path = thumb_path(library_root, &hash);
    if let Some(parent) = thumb_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&thumb_path, &thumb_bytes)?;

    // 10. Write .rlab
    let rlab_path = rlab_path(library_root, &hash);
    if let Some(parent) = rlab_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_rlab(
        &rlab_path,
        &original_bytes,
        &lmta,
        &thumb_bytes,
        width,
        height,
    )?;

    // 11. Insert into DB
    db.insert_photo(
        &hash,
        &relative_lib_path(&hash),
        &lmta,
        width,
        height,
        stack_id.as_deref(),
    )?;

    Ok(Some(hash))
}

// ── Write .rlab ───────────────────────────────────────────────────────────────

fn write_rlab(
    path: &Path,
    original_bytes: &[u8],
    lmta: &LibraryMeta,
    thumb_bytes: &[u8],
    width: u32,
    height: u32,
) -> Result<()> {
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
    let mut rlab = RlabFile::new(
        meta,
        original_bytes.to_vec(),
        copies,
        0,
        Some(thumb_bytes.to_vec()),
    );
    rlab.set_lmta(Some(lmta.clone()));
    rlab.write(path).context("write .rlab")
}

// ── Path helpers ──────────────────────────────────────────────────────────────

pub fn relative_lib_path(hash: &str) -> String {
    format!("{}/{}/{}.rlab", &hash[0..2], &hash[2..4], hash)
}

pub fn rlab_path(library_root: &Path, hash: &str) -> PathBuf {
    library_root.join("files").join(relative_lib_path(hash))
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
    use std::collections::HashMap;

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
fn format_session_name(start: u64, end: u64) -> String {
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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

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
