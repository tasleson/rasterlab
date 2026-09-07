//! Background integrity scrub.
//!
//! Walks every `.rlab` file the library holds — under `library_root/files/`
//! and `library_root/recently_deleted/files/` alike, since a photo waiting to
//! be restored rots at the same rate as any other — verifies its per-chunk and
//! whole-file Blake3 hashes, and acts on the result:
//!
//! * **Clean, already v5** — left untouched.
//! * **Clean, older format** — rewritten as v5. v3 files gain Reed-Solomon
//!   `RECC` parity for the first time; v4 files gain the split parity placement
//!   that survives truncation from either end. This is a lossless re-save; the
//!   corrupted-file backup path is *not* taken.
//! * **Correctable corruption** — the damaged original is copied to
//!   `library_root/recovered/` (mirroring its `ab/cd/{hash}.rlab` layout,
//!   whichever root it was found under — the name is the photo either way) and
//!   the file is repaired from its `RECC` parity and re-saved in place.
//! * **Unreadable sectors** — the file is read past the bad regions, which are
//!   zero-filled and then reconstructed from parity like any other damage, so a
//!   latent sector error costs only the shards it landed in.
//! * **Uncorrectable corruption** — reported as a per-file error (also written
//!   to stderr) for the caller to surface in a dialog.
//!
//! Every file that survives the above is then checked against its own name: the
//! library is content-addressed, so the path and the embedded original are two
//! independent records of which photo this is. See [`check_identity`] — that
//! disagreement is the one class of damage the parity is blind to, because
//! nothing inside the file is wrong.
//!
//! The walk honours a shared cancellation flag so the GUI can stop it.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use crate::import::walk_rlab_files;
use anyhow::{Context, Result};
use rasterlab_core::{
    degraded_read::read_degraded_file,
    project::{FORMAT_VERSION_V5, RlabFile, read_original_hash, verify_and_repair},
    verified_write::{create_dir_all_synced, rename_synced, write_verified_atomic},
};

/// Extension used for the temporary file a repair/upgrade is staged into before
/// being atomically renamed over the original.
const TMP_EXT: &str = "rlab.scrub-tmp";

/// Length of a Blake3 digest in hex, which is what names a library file.
const HASH_HEX_LEN: usize = 64;

/// Live progress for a running scrub, delivered through the progress callback.
#[derive(Debug, Clone, Default)]
pub struct ScrubProgress {
    pub total: usize,
    /// Files checked so far.
    pub done: usize,
    /// Files repaired from `RECC` parity after corruption was found.
    pub repaired: usize,
    /// Clean pre-v5 files rewritten as v5 for stronger parity placement.
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

/// Scrub every `.rlab` file the library holds, Recently Deleted included.
///
/// `cancel` is polled before each file; when set, the scrub returns early with
/// `cancelled = true` and the tallies accumulated so far. `progress_cb` is
/// invoked before each file and once more at the end.
pub fn scrub(
    library_root: &Path,
    cancel: Arc<AtomicBool>,
    progress_cb: &dyn Fn(ScrubProgress),
) -> Result<ScrubOutcome> {
    scrub_impl(library_root, cancel, progress_cb, None)
}

/// Library-owned scrub variant that serializes each individual file check with
/// project mutations. The guard is deliberately released between files so a
/// large network library never monopolizes the mutation lock for the full walk.
pub(crate) fn scrub_with_project_lock(
    library_root: &Path,
    cancel: Arc<AtomicBool>,
    progress_cb: &dyn Fn(ScrubProgress),
    project_write_lock: &Mutex<()>,
) -> Result<ScrubOutcome> {
    scrub_impl(library_root, cancel, progress_cb, Some(project_write_lock))
}

fn scrub_impl(
    library_root: &Path,
    cancel: Arc<AtomicBool>,
    progress_cb: &dyn Fn(ScrubProgress),
    project_write_lock: Option<&Mutex<()>>,
) -> Result<ScrubOutcome> {
    let files_dir = library_root.join("files");
    let deleted_dir = library_root.join("recently_deleted/files");
    let recovered_dir = library_root.join("recovered");

    if !files_dir.exists() {
        return Ok(ScrubOutcome::default());
    }

    // Each file is carried with the root it was found under, which is what its
    // `ab/cd/{hash}.rlab` placement — the half of its identity that lives
    // outside the file — is relative to.
    let rlab_paths: Vec<(&Path, PathBuf)> = walk_rlab_files(&files_dir)
        .into_iter()
        .map(|path| (files_dir.as_path(), path))
        .chain(
            walk_rlab_files(&deleted_dir)
                .into_iter()
                .map(|path| (deleted_dir.as_path(), path)),
        )
        .collect();

    let total = rlab_paths.len();
    let mut progress = ScrubProgress {
        total,
        ..Default::default()
    };

    for (i, (base_dir, path)) in rlab_paths.iter().enumerate() {
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

        let action = if let Some(lock) = project_write_lock {
            let _write_guard = lock
                .lock()
                .map_err(|_| anyhow::anyhow!("project write lock is poisoned"))?;
            scrub_one(base_dir, &recovered_dir, path)
        } else {
            scrub_one(base_dir, &recovered_dir, path)
        };
        match action {
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

/// Check one file.  `base_dir` is the root its `ab/cd/{hash}.rlab` placement
/// is relative to, which is `files/` or the Recently Deleted mirror of it.
fn scrub_one(base_dir: &Path, recovered_dir: &Path, path: &Path) -> Result<ScrubAction> {
    let action = repair_one(base_dir, recovered_dir, path)?;
    // Runs on the settled file so the hash checked is the one that will be
    // kept. A file that was repaired and then fails this is still reported as
    // an error: the bytes were recovered, but they are not the bytes this path
    // is supposed to hold.
    check_identity(base_dir, path)?;
    Ok(action)
}

fn repair_one(base_dir: &Path, recovered_dir: &Path, path: &Path) -> Result<ScrubAction> {
    let tmp = path.with_extension(TMP_EXT);
    // Drop any stale temp left by an interrupted earlier run.
    let _ = std::fs::remove_file(&tmp);

    let report = verify_and_repair(path, Some(&tmp))
        .with_context(|| format!("verify {}", path.display()))?;

    // Unreadable sectors disqualify a file from "clean" even when its content
    // still verifies, which happens when the lost bytes were zero anyway. The
    // repair path rewrites it, and that rewrite is what moves it off the bad
    // sectors — leaving it in place would just defer the loss.
    let clean =
        report.file_hash_ok && report.damaged_chunks.is_empty() && report.unreadable_bytes == 0;

    if clean {
        // A clean file produces no output, so `tmp` does not exist here.
        if report.format_version == Some(FORMAT_VERSION_V5) {
            return Ok(ScrubAction::Clean);
        }
        // Older layout: re-save as v5. v3 gains parity at all; v4 gains the
        // split placement that survives end truncation. Best-effort — a write
        // failure leaves the intact original in place and is not a corruption
        // error.
        match upgrade_to_v5(path) {
            Ok(()) => Ok(ScrubAction::Upgraded),
            Err(e) => {
                eprintln!("scrub: could not upgrade {} to v5: {e}", path.display());
                Ok(ScrubAction::Clean)
            }
        }
    } else if report.repaired {
        // `tmp` now holds the repaired file. Back up the corrupted original
        // before overwriting it, then swap the repaired copy into place.
        backup_to_recovered(base_dir, recovered_dir, path)?;
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
        // Unreadable sectors point at failing hardware rather than bit rot, so
        // say so — the user's next step is the drive, not the file.
        let media = if report.unreadable_bytes > 0 {
            format!(
                "; {} bytes unreadable — check drive health",
                report.unreadable_bytes
            )
        } else {
            String::new()
        };
        anyhow::bail!("{cause}: {what}{media}")
    }
}

/// Confirm a file is the photo its name claims it is.
///
/// Library files are content-addressed: the name and directory come from the
/// Blake3 of the embedded original, so the path and the `ORIG` chunk are two
/// independent records of the same fact. A disagreement is an *identity
/// discrepancy* in the sense of Bairavasundaram et al. (FAST 2008) — content
/// that is intact, self-consistent, and in the wrong place. A misdirected
/// write, a cross-linked directory entry, a file restored under the wrong
/// name. No amount of parity inside the file can see it, because nothing
/// inside the file is wrong.
///
/// Note this does *not* detect a stale file: `ORIG` never changes for a given
/// photo, so an older version of the same project still hashes to the same
/// name. Catching that needs a record kept outside the file.
///
/// Files whose name is not a hash are skipped — there is nothing to check them
/// against.
fn check_identity(base_dir: &Path, path: &Path) -> Result<()> {
    let Some(named_hash) = path.file_stem().and_then(|s| s.to_str()) else {
        return Ok(());
    };
    if named_hash.len() != HASH_HEX_LEN || !named_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(());
    }

    // Placement is derived from the same hash, so a file that is correctly
    // named but in the wrong shard directory is unreachable by hash lookup.
    let rel = path.strip_prefix(base_dir).unwrap_or(path);
    let expected_rel = crate::import::relative_lib_path(named_hash);
    if rel != Path::new(&expected_rel) {
        anyhow::bail!(
            "misplaced: named {named_hash} but filed at {} instead of {expected_rel}",
            rel.display()
        );
    }

    // Same hex form the importer names files with, so the two are comparable.
    let embedded = read_original_hash(path)
        .with_context(|| format!("read ORIG hash from {}", path.display()))?;
    let embedded = blake3::Hash::from(embedded).to_hex();

    if !embedded.eq_ignore_ascii_case(named_hash) {
        anyhow::bail!(
            "identity mismatch: file is named for {named_hash} but embeds {embedded} \
             — misdirected write or misfiled copy"
        );
    }
    Ok(())
}

/// Re-save a clean file as v5.
///
/// `write_v5` already stages beside `path`, syncs, reads back and renames into
/// place, so there is nothing for this to add: an intermediate temp of our own
/// would only stage the same bytes twice and finish on a rename the writer had
/// no chance to sync.
fn upgrade_to_v5(path: &Path) -> Result<()> {
    let rlab = RlabFile::read(path).with_context(|| format!("read {}", path.display()))?;
    rlab.write_v5(path)
        .with_context(|| format!("write v5 {}", path.display()))
}

/// Copy the (corrupted) original to `recovered/`, preserving its relative
/// `ab/cd/{hash}.rlab` layout. A timestamp suffix avoids clobbering a backup
/// from an earlier scrub of the same file.
///
/// Uses the degraded reader rather than `std::fs::copy`: the file being backed
/// up may have unreadable sectors — that is one of the reasons it is being
/// repaired — and a plain copy would fail on the first `EIO`, taking the repair
/// down with it. Unreadable regions appear in the backup as zeros.
fn backup_to_recovered(base_dir: &Path, recovered_dir: &Path, path: &Path) -> Result<()> {
    let rel = path.strip_prefix(base_dir).unwrap_or(path);
    let mut dest = recovered_dir.join(rel);
    if dest.exists() {
        let ts = unix_now();
        let stem = dest.file_stem().and_then(|s| s.to_str()).unwrap_or("file");
        let ext = dest.extension().and_then(|s| s.to_str()).unwrap_or("rlab");
        dest.set_file_name(format!("{stem}.{ts}.{ext}"));
    }
    if let Some(parent) = dest.parent() {
        create_dir_all_synced(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let degraded = read_degraded_file(path).with_context(|| format!("read {}", path.display()))?;
    // The original is about to be renamed over, so this backup is the only
    // remaining copy of whatever the repair could not use.  It has to be on the
    // device before the replace, not merely handed to the page cache.
    write_verified_atomic(&dest, &degraded.data)
        .with_context(|| format!("back up corrupted file to {}", dest.display()))?;
    Ok(())
}

/// Rename `tmp` over `dst`. Both live in the same directory (hence the same
/// filesystem), so the rename is atomic; syncing the directory afterwards is
/// what keeps the new name pointing at the repaired file across a power cut.
fn replace_atomically(tmp: &Path, dst: &Path) -> Result<()> {
    rename_synced(tmp, dst).with_context(|| format!("replace {}", dst.display()))
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
