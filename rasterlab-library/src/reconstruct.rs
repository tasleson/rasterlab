use std::{collections::HashMap, path::Path};

use anyhow::Result;
use rasterlab_core::{formats::FormatRegistry, project::RlabFile};
use walkdir::WalkDir;

use crate::{
    db_trait::LibraryDb,
    import::{format_session_name, thumb_path},
    thumbnail::generate_thumbnail,
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
/// `import_date` survive in the LMTA chunks, so the count, `started_at`, and
/// display name are all rebuilt from the import-date range.
struct SessionAgg {
    count: i64,
    min_date: u64,
    max_date: u64,
}

/// Rebuild the database index by scanning all `.rlab` files in `library_root/files/`.
/// On completion the DB reflects the current on-disk state.
pub fn rebuild(
    library_root: &Path,
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    progress_cb: &dyn Fn(RebuildProgress),
) -> Result<()> {
    db.clear_all()?;

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

    for (i, rlab_file_path) in rlab_paths.iter().enumerate() {
        progress_cb(RebuildProgress {
            total,
            done: i,
            current: rlab_file_path.clone(),
            errors: errors.clone(),
        });

        if let Err(e) = reindex_one(library_root, db, registry, rlab_file_path, &mut sessions) {
            errors.push((rlab_file_path.clone(), e.to_string()));
        }
    }

    // Restore the session rows now that every photo's session membership and
    // import date are known. The name is regenerated from the session's
    // import-date range — the same scheme import uses — since the original
    // name lived only in the DB that was just cleared.
    for (id, agg) in &sessions {
        db.insert_session(
            id,
            &format_session_name(agg.min_date, agg.max_date),
            agg.min_date,
            None,
        )?;
        db.update_session_count(id, agg.count)?;
    }

    progress_cb(RebuildProgress {
        total,
        done: total,
        current: std::path::PathBuf::new(),
        errors: errors.clone(),
    });
    Ok(())
}

fn reindex_one(
    library_root: &Path,
    db: &dyn LibraryDb,
    registry: &FormatRegistry,
    rlab_file_path: &Path,
    sessions: &mut HashMap<String, SessionAgg>,
) -> Result<()> {
    let rlab = RlabFile::read(rlab_file_path)?;

    // Derive hash from the path stem (files/ab/cd/{hash}.rlab)
    let hash = rlab_file_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_owned();

    if hash.is_empty() {
        return Ok(());
    }

    let lmta = rlab.lmta.clone().unwrap_or_default();

    // Re-generate thumbnail if missing
    let tpath = thumb_path(library_root, &hash);
    if !tpath.exists()
        && let Ok(image) = registry.decode_bytes(&rlab.original_bytes, None)
        && let Ok(thumb) = generate_thumbnail(&image, 512)
    {
        if let Some(parent) = tpath.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(&tpath, thumb).ok();
    }

    let lib_path = rlab_file_path
        .strip_prefix(library_root.join("files"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| format!("{}/{}/{}.rlab", &hash[0..2], &hash[2..4], hash));

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
                count: 0,
                min_date: lmta.import_date,
                max_date: lmta.import_date,
            });
        agg.count += 1;
        agg.min_date = agg.min_date.min(lmta.import_date);
        agg.max_date = agg.max_date.max(lmta.import_date);
    }

    Ok(())
}
