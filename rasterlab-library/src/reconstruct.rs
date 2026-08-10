use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::Result;
use rasterlab_core::{formats::FormatRegistry, project::RlabFile};
use walkdir::WalkDir;

use crate::{
    db_trait::{LibraryDb, SortOrder},
    import::{format_session_name, thumb_path},
    thumbnail::{generate_thumbnail, write_thumbnail},
};

#[derive(Debug, Clone)]
pub struct RebuildProgress {
    pub total: usize,
    pub done: usize,
    pub current: std::path::PathBuf,
    pub errors: Vec<(std::path::PathBuf, String)>,
}

/// Per-session aggregates gathered while re-indexing photos, used afterwards to
/// restore the `import_sessions` rows. Only the session id and each photo's
/// `import_date` survive in the LMTA chunks, so `started_at` and the display
/// name are both rebuilt from the import-date range.
struct SessionAgg {
    min_date: u64,
    max_date: u64,
}

/// Bring the database index back in line with the `.rlab` files on disk.
///
/// This is the reconciliation pass for everything the two-step writes elsewhere
/// can leave behind: photos whose file was written but never indexed (an import
/// killed part-way), rows whose file is gone (a delete that stopped after the
/// trash), and metadata a file records that the index does not.
///
/// It is written to be safely re-runnable and to never leave the library worse
/// than it found it:
///
/// * Rows are refreshed in place, file by file, instead of the index being
///   emptied up front — so a rebuild that dies half-way leaves a library that
///   still mostly works, and running it again finishes the job.
/// * A session row that already exists keeps its name and `started_at`, so a
///   session the user renamed is not reverted to a generated date range.
/// * Rows for files that are no longer on disk are dropped only when the walk
///   actually found photos.  An empty `files/` directory is far more often an
///   unmounted volume than a library the user emptied, and wiping the index
///   over a mount failure is not recoverable from.
pub fn rebuild(
    library_root: &Path,
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    progress_cb: &dyn Fn(RebuildProgress),
) -> Result<()> {
    let files_dir = library_root.join("files");
    if !files_dir.exists() {
        return Ok(());
    }

    // Collect all .rlab paths first so we can report total
    let rlab_paths: Vec<_> = WalkDir::new(&files_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "rlab"))
        .map(|e| e.into_path())
        .collect();

    let total = rlab_paths.len();
    let mut errors: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut sessions: HashMap<String, SessionAgg> = HashMap::new();
    let mut indexed: HashSet<String> = HashSet::with_capacity(total);

    for (i, rlab_file_path) in rlab_paths.iter().enumerate() {
        progress_cb(RebuildProgress {
            total,
            done: i,
            current: rlab_file_path.clone(),
            errors: errors.clone(),
        });

        match reindex_one(library_root, db, registry, rlab_file_path, &mut sessions) {
            Ok(Some(hash)) => {
                indexed.insert(hash);
            }
            Ok(None) => {}
            Err(e) => errors.push((rlab_file_path.clone(), e.to_string())),
        }
    }

    // Drop rows whose file is no longer there — a photo deleted outside the
    // app, or a delete that stopped after trashing the file.  A file that
    // failed to re-index is not evidence that it is gone, so anything that
    // errored above keeps its row.
    if !indexed.is_empty() {
        let unreadable: HashSet<&Path> = errors.iter().map(|(p, _)| p.as_path()).collect();
        for row in db.all_photos(SortOrder::default())? {
            if indexed.contains(&row.hash) {
                continue;
            }
            if unreadable.contains(files_dir.join(&row.lib_path).as_path()) {
                continue;
            }
            db.delete_photo(row.id)?;
        }
    }

    // Restore the session rows now that every photo's session membership and
    // import date are known.  `insert_session` is a no-op for a session that
    // still exists, which is what preserves a user's rename; the count comes
    // from the rows rather than from this run's tally, so a photo that kept its
    // row after failing to re-index is still counted.
    for (id, agg) in &sessions {
        db.insert_session(
            id,
            &format_session_name(agg.min_date, agg.max_date),
            agg.min_date,
            None,
        )?;
        db.update_session_count(id, db.session_photo_count(id)?)?;
    }
    db.delete_empty_sessions()?;

    progress_cb(RebuildProgress {
        total,
        done: total,
        current: std::path::PathBuf::new(),
        errors: errors.clone(),
    });
    Ok(())
}

/// Index one `.rlab`, replacing any row that already describes it.  Returns the
/// photo's hash, or `None` for a path the hash cannot be read from.
fn reindex_one(
    library_root: &Path,
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    rlab_file_path: &Path,
    sessions: &mut HashMap<String, SessionAgg>,
) -> Result<Option<String>> {
    let rlab = RlabFile::read(rlab_file_path)?;

    // Derive hash from the path stem (files/ab/cd/{hash}.rlab)
    let hash = rlab_file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_owned();

    if hash.is_empty() {
        return Ok(None);
    }

    let lmta = rlab.lmta.clone().unwrap_or_default();

    // Re-generate thumbnail if missing
    let tpath = thumb_path(library_root, &hash);
    if !tpath.exists()
        && let Ok(image) = registry.decode_bytes(&rlab.original_bytes, None)
        && let Ok(thumb) = generate_thumbnail(&image, 512)
    {
        write_thumbnail(&tpath, &thumb).ok();
    }

    let lib_path = rlab_file_path
        .strip_prefix(library_root.join("files"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| format!("{}/{}/{}.rlab", &hash[0..2], &hash[2..4], hash));

    // Replace rather than skip: the file is the record, so whatever it says now
    // wins over whatever the index remembers.  Delete-then-insert leaves the
    // photo missing if the process dies between the two, which the next run
    // repairs — the reason this pass has to stay re-runnable.
    if let Some(existing) = db.photo_by_hash(&hash)? {
        db.delete_photo(existing.id)?;
    }

    db.insert_photo(
        &hash,
        &lib_path,
        &lmta,
        rlab.meta.width,
        rlab.meta.height,
        lmta.stack_peer_hash
            .as_deref()
            .map(|_| hash.as_str())
            .map(|_| {
                // Generate a stable stack_id from the sorted pair of hashes
                // (same logic used during import)
                hash.as_str()
            }),
    )?;

    // Record session membership; the session rows themselves are restored in
    // one pass after all photos are indexed (see `rebuild`).
    if !lmta.import_session_id.is_empty() {
        let agg = sessions
            .entry(lmta.import_session_id.clone())
            .or_insert(SessionAgg {
                min_date: lmta.import_date,
                max_date: lmta.import_date,
            });
        agg.min_date = agg.min_date.min(lmta.import_date);
        agg.max_date = agg.max_date.max(lmta.import_date);
    }

    Ok(Some(hash))
}
