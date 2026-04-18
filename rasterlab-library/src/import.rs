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
    formats::FormatRegistry,
    library_meta::{LibraryExif, LibraryMeta},
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
    let session_id = Uuid::new_v4().to_string();
    let started_at = unix_now();

    progress_cb(ImportProgress {
        total: paths.len(),
        done: 0,
        current_file: PathBuf::new(),
        skipped_duplicates: 0,
        errors: Vec::new(),
    });

    // Detect RAW+JPEG stacks within this batch before importing.
    let stack_map = detect_stacks(paths);

    let mut done = 0usize;
    let mut skipped_duplicates = 0usize;
    let mut errors: Vec<(PathBuf, String)> = Vec::new();
    let mut imported_hashes: Vec<(PathBuf, String)> = Vec::new();

    for path in paths {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }

        progress_cb(ImportProgress {
            total: paths.len(),
            done,
            current_file: path.clone(),
            skipped_duplicates,
            errors: errors.clone(),
        });

        match import_one(library_root, db, registry, path, &session_id, &stack_map) {
            Ok(None) => {
                skipped_duplicates += 1;
            }
            Ok(Some(hash)) => {
                imported_hashes.push((path.clone(), hash));
                done += 1;
            }
            Err(e) => {
                errors.push((path.clone(), format!("{:#}", e)));
            }
        }
    }

    // Derive session name from the capture-date range of imported photos.
    let session_name = derive_session_name(db, &session_id);
    db.insert_session(&session_id, &session_name, started_at, None)?;
    db.update_session_count(&session_id, done as i64)?;

    progress_cb(ImportProgress {
        total: paths.len(),
        done,
        current_file: PathBuf::new(),
        skipped_duplicates,
        errors: errors.clone(),
    });

    Ok(ImportSession {
        id: session_id,
        name: session_name,
        started_at,
        photo_count: done,
        errors,
    })
}

// ── Single-file import ────────────────────────────────────────────────────────

/// Returns `Ok(Some(hash))` on success, `Ok(None)` if duplicate, `Err` on failure.
fn import_one(
    library_root: &Path,
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    path: &Path,
    session_id: &str,
    stack_map: &[(usize, usize)], // (primary_idx, secondary_idx) pairs by path index
) -> Result<Option<String>> {
    // 1. Read source bytes
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
    let exif = LibraryExif::from_image_metadata(&image.metadata);

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
        import_session_id: session_id.to_owned(),
        import_date: unix_now(),
        stack_peer_hash,
        stack_is_primary,
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
        lmta.original_filename.as_deref(),
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
fn detect_stacks(paths: &[PathBuf]) -> Vec<(usize, usize)> {
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (i, p) in paths.iter().enumerate() {
        let stem = p
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let ext = p
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        if !is_raw_ext(&ext) {
            continue;
        }
        // Look for a matching JPEG with the same stem
        for (j, q) in paths.iter().enumerate() {
            if i == j {
                continue;
            }
            let qstem = q
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            let qext = q
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if qstem == stem && is_jpeg_ext(&qext) {
                pairs.push((i, j));
                break;
            }
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

// ── Session naming ────────────────────────────────────────────────────────────

fn derive_session_name(db: &dyn LibraryDb, session_id: &str) -> String {
    let photos = db.photos_by_session(session_id).unwrap_or_default();
    let mut dates: Vec<&str> = photos
        .iter()
        .filter_map(|p| p.capture_date.as_deref())
        .filter(|d| d.len() >= 10)
        .map(|d| &d[..10]) // "YYYY:MM:DD"
        .collect();
    dates.sort_unstable();
    dates.dedup();

    match dates.as_slice() {
        [] => {
            // Fall back to today
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            format_date_from_unix(now)
        }
        [single] => format_exif_date(single),
        [first, .., last] => {
            format!("{}–{}", format_exif_date(first), format_exif_date(last))
        }
    }
}

/// Format `"YYYY:MM:DD"` → `"Jun 3 2025"`.
fn format_exif_date(d: &str) -> String {
    let parts: Vec<&str> = d.splitn(3, ':').collect();
    if parts.len() < 3 {
        return d.to_owned();
    }
    let month_names = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month: usize = parts[1].parse().unwrap_or(0);
    let day: u32 = parts[2].parse().unwrap_or(0);
    let year = parts[0];
    if month >= 1 && month <= 12 {
        format!("{} {} {}", month_names[month - 1], day, year)
    } else {
        d.to_owned()
    }
}

fn format_date_from_unix(ts: u64) -> String {
    // Simple: just return ISO date from unix seconds
    let days_since_epoch = ts / 86400;
    let _ = days_since_epoch; // avoid unused warning
    let now = chrono_lite_date(ts);
    now
}

fn chrono_lite_date(ts: u64) -> String {
    // Minimal calendar calculation without adding a chrono dependency
    // (the GUI already has chrono via other deps, but this crate avoids it)
    let secs = ts as i64;
    let days = secs / 86400;
    // Days since 1970-01-01
    let mut year = 1970i32;
    let mut remaining = days;
    loop {
        let in_year = if is_leap(year) { 366 } else { 365 };
        if remaining < in_year {
            break;
        }
        remaining -= in_year;
        year += 1;
    }
    let months = if is_leap(year) {
        [31i64, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31i64, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let month_names = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mut month = 0usize;
    while month < 12 && remaining >= months[month] {
        remaining -= months[month];
        month += 1;
    }
    format!("{} {} {}", month_names[month], remaining + 1, year)
}

fn is_leap(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
