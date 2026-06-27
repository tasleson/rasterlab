//! Background integrity scrub.
//!
//! Walks every `.rlab` file under `library_root/files/`, verifies its per-chunk
//! and whole-file Blake3 hashes, and acts on the result:
//!
//! * **Clean, with parity** — left untouched.
//! * **Clean, no parity** (older v3 files) — rewritten as v4 so they gain
//!   Reed-Solomon `RECC` parity and become repairable in future scrubs. This is
//!   a lossless re-save; the corrupted-file backup path is *not* taken.
//! * **Correctable corruption** — the damaged original is copied to
//!   `library_root/recovered/` (mirroring its `ab/cd/{hash}.rlab` layout) and
//!   the file is repaired from its `RECC` parity and re-saved in place.
//! * **Uncorrectable corruption** — reported as a per-file error (also written
//!   to stderr) for the caller to surface in a dialog.
//!
//! The walk honours a shared cancellation flag so the GUI can stop it.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use rasterlab_core::project::{RlabFile, verify_and_repair};
use walkdir::WalkDir;

/// Extension used for the temporary file a repair/upgrade is staged into before
/// being atomically renamed over the original.
const TMP_EXT: &str = "rlab.scrub-tmp";

/// Live progress for a running scrub, delivered through the progress callback.
#[derive(Debug, Clone, Default)]
pub struct ScrubProgress {
    pub total: usize,
    /// Files checked so far.
    pub done: usize,
    /// Files repaired from `RECC` parity after corruption was found.
    pub repaired: usize,
    /// Clean v3 files rewritten as v4 to add ECC parity.
    pub upgraded: usize,
    pub current_file: PathBuf,
    /// Per-file uncorrectable failures: `(path, message)`.
    pub errors: Vec<(PathBuf, String)>,
}

/// Final tally returned once a scrub finishes (or is cancelled).
#[derive(Debug, Clone, Default)]
pub struct ScrubOutcome {
    pub checked: usize,
    pub repaired: usize,
    pub upgraded: usize,
    pub errors: Vec<(PathBuf, String)>,
    pub cancelled: bool,
}

enum ScrubAction {
    Clean,
    Repaired,
    Upgraded,
}

/// Scrub every `.rlab` file under `library_root/files/`.
///
/// `cancel` is polled before each file; when set, the scrub returns early with
/// `cancelled = true` and the tallies accumulated so far. `progress_cb` is
/// invoked before each file and once more at the end.
pub fn scrub(
    library_root: &Path,
    cancel: Arc<AtomicBool>,
    progress_cb: &dyn Fn(ScrubProgress),
) -> Result<ScrubOutcome> {
    let files_dir = library_root.join("files");
    let recovered_dir = library_root.join("recovered");

    if !files_dir.exists() {
        return Ok(ScrubOutcome::default());
    }

    let rlab_paths: Vec<PathBuf> = WalkDir::new(&files_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "rlab"))
        .map(|e| e.into_path())
        .collect();

    let total = rlab_paths.len();
    let mut progress = ScrubProgress {
        total,
        ..Default::default()
    };

    for (i, path) in rlab_paths.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Ok(ScrubOutcome {
                checked: i,
                repaired: progress.repaired,
                upgraded: progress.upgraded,
                errors: progress.errors,
                cancelled: true,
            });
        }

        progress.done = i;
        progress.current_file = path.clone();
        progress_cb(progress.clone());

        match scrub_one(&files_dir, &recovered_dir, path) {
            Ok(ScrubAction::Clean) => {}
            Ok(ScrubAction::Repaired) => progress.repaired += 1,
            Ok(ScrubAction::Upgraded) => progress.upgraded += 1,
            Err(e) => {
                let msg = e.to_string();
                eprintln!("scrub: {}: {msg}", path.display());
                progress.errors.push((path.clone(), msg));
            }
        }
    }

    progress.done = total;
    progress.current_file = PathBuf::new();
    progress_cb(progress.clone());

    Ok(ScrubOutcome {
        checked: total,
        repaired: progress.repaired,
        upgraded: progress.upgraded,
        errors: progress.errors,
        cancelled: false,
    })
}

fn scrub_one(files_dir: &Path, recovered_dir: &Path, path: &Path) -> Result<ScrubAction> {
    let tmp = path.with_extension(TMP_EXT);
    // Drop any stale temp left by an interrupted earlier run.
    let _ = std::fs::remove_file(&tmp);

    let report = verify_and_repair(path, Some(&tmp))
        .with_context(|| format!("verify {}", path.display()))?;

    let clean = report.file_hash_ok && report.damaged_chunks.is_empty();

    if clean {
        // A clean file produces no output, so `tmp` does not exist here.
        if report.recc_present {
            return Ok(ScrubAction::Clean);
        }
        // v3 (no ECC): re-save as v4 so it becomes repairable. Best-effort —
        // a write failure (e.g. a locked/protected file) leaves the intact
        // original in place and is not a corruption error.
        match upgrade_to_v4(path, &tmp) {
            Ok(()) => Ok(ScrubAction::Upgraded),
            Err(e) => {
                eprintln!("scrub: could not upgrade {} to v4: {e}", path.display());
                let _ = std::fs::remove_file(&tmp);
                Ok(ScrubAction::Clean)
            }
        }
    } else if report.repaired {
        // `tmp` now holds the repaired file. Back up the corrupted original
        // before overwriting it, then swap the repaired copy into place.
        backup_to_recovered(files_dir, recovered_dir, path)?;
        replace_atomically(&tmp, path)?;
        Ok(ScrubAction::Repaired)
    } else {
        let _ = std::fs::remove_file(&tmp);
        let what = if report.damaged_chunks.is_empty() {
            "whole-file hash mismatch".to_owned()
        } else {
            format!("bad chunks: {}", report.damaged_chunks.join(", "))
        };
        let cause = if report.recc_present {
            "uncorrectable (damage exceeds parity)"
        } else {
            "uncorrectable (no ECC parity present)"
        };
        anyhow::bail!("{cause}: {what}")
    }
}

/// Re-save a clean file as v4, staging through `tmp` and renaming over `path`.
fn upgrade_to_v4(path: &Path, tmp: &Path) -> Result<()> {
    let rlab = RlabFile::read(path).with_context(|| format!("read {}", path.display()))?;
    rlab.write_v4(tmp)
        .with_context(|| format!("write v4 {}", tmp.display()))?;
    replace_atomically(tmp, path)
}

/// Copy the (corrupted) original to `recovered/`, preserving its relative
/// `ab/cd/{hash}.rlab` layout. A timestamp suffix avoids clobbering a backup
/// from an earlier scrub of the same file.
fn backup_to_recovered(files_dir: &Path, recovered_dir: &Path, path: &Path) -> Result<()> {
    let rel = path.strip_prefix(files_dir).unwrap_or(path);
    let mut dest = recovered_dir.join(rel);
    if dest.exists() {
        let ts = unix_now();
        let stem = dest.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
        let ext = dest.extension().and_then(|s| s.to_str()).unwrap_or("rlab");
        dest.set_file_name(format!("{stem}.{ts}.{ext}"));
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::copy(path, &dest)
        .with_context(|| format!("back up corrupted file to {}", dest.display()))?;
    Ok(())
}

/// Rename `tmp` over `dst`. Both live in the same directory (hence the same
/// filesystem), so the rename is atomic.
fn replace_atomically(tmp: &Path, dst: &Path) -> Result<()> {
    std::fs::rename(tmp, dst).with_context(|| format!("replace {}", dst.display()))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
