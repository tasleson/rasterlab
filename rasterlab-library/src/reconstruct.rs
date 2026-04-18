use std::path::Path;

use anyhow::Result;
use rasterlab_core::{formats::FormatRegistry, project::RlabFile};
use walkdir::WalkDir;

use crate::{
    db_trait::LibraryDb,
    import::thumb_path,
    thumbnail::generate_thumbnail,
};

#[derive(Debug, Clone)]
pub struct RebuildProgress {
    pub total:   usize,
    pub done:    usize,
    pub current: std::path::PathBuf,
    pub errors:  Vec<(std::path::PathBuf, String)>,
}

/// Rebuild the database index by scanning all `.rlab` files in `library_root/files/`.
/// On completion the DB reflects the current on-disk state.
pub fn rebuild(
    library_root: &Path,
    db:           &dyn LibraryDb,
    registry:     &FormatRegistry,
    progress_cb:  &dyn Fn(RebuildProgress),
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
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().map_or(false, |x| x == "rlab")
        })
        .map(|e| e.into_path())
        .collect();

    let total = rlab_paths.len();
    let mut errors: Vec<(std::path::PathBuf, String)> = Vec::new();

    for (i, rlab_file_path) in rlab_paths.iter().enumerate() {
        progress_cb(RebuildProgress {
            total,
            done: i,
            current: rlab_file_path.clone(),
            errors: errors.clone(),
        });

        if let Err(e) = reindex_one(library_root, db, registry, rlab_file_path) {
            errors.push((rlab_file_path.clone(), e.to_string()));
        }
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
    library_root:   &Path,
    db:             &dyn LibraryDb,
    registry:       &FormatRegistry,
    rlab_file_path: &Path,
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
    if !tpath.exists() {
        if let Ok(image) = registry.decode_bytes(&rlab.original_bytes, None) {
            if let Ok(thumb) = generate_thumbnail(&image, 512) {
                if let Some(parent) = tpath.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                std::fs::write(&tpath, thumb).ok();
            }
        }
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
        lmta.stack_peer_hash.as_deref().map(|_| hash.as_str()).map(|_| {
            // Generate a stable stack_id from the sorted pair of hashes
            // (same logic used during import)
            hash.as_str()
        }),
    )?;

    // Rebuild import session row if needed
    if !lmta.import_session_id.is_empty() {
        db.insert_session(&lmta.import_session_id, "Recovered session", lmta.import_date, None)
            .ok();
    }

    Ok(())
}
