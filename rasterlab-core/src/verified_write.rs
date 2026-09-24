//! Durable writes: what actually landed on the media, under a name that stays.
//!
//! Every digest in a `.rlab` file is computed over the in-memory buffer *before*
//! the write, so the file describes what we meant to store.  A write path that
//! corrupts in transit — driver, controller, cable, firmware — therefore
//! produces a file that is internally consistent and wrong, and nothing inside
//! it can say so.  CERN's data-integrity campaign (Panzer-Steindel, CERN/IT,
//! 2007) found exactly this class by writing, reading back and comparing:
//! roughly one file in 1500 came back different, and none of it was reported by
//! the storage stack.
//!
//! [`write_verified_atomic`] closes that window at save time, while the correct
//! bytes are still in memory and the failure costs a retry, rather than leaving
//! it to be discovered by a scrub weeks later when the good copy is long gone.
//!
//! It also never writes over the previous file.  A `.rlab` embeds the only copy
//! of the original photo, and the library rewrites one whole for a star rating
//! or a collection rename; a write in place turns every one of those into a
//! window where a crash, a full disk or a lost connection takes the photograph
//! with it.  The new file is staged beside the destination and renamed into
//! place, so the old bytes stay reachable until the new ones are complete,
//! synced and verified.
//!
//! # Choosing a primitive
//!
//! Every file the application writes should go through one of these, so that no
//! path leaves a half-written file where a whole one used to be:
//!
//! * [`write_verified_atomic`] — for bytes that are the only copy of something:
//!   `.rlab` files, thumbnails, the scrub's backup of a damaged original.  Pays
//!   a read-back to confirm the storage stack stored what it was given.
//! * [`write_atomic`] — same staging, fsync and rename, without the read-back.
//!   For files that can be produced again from something we still have:
//!   exports, preferences, autosaves, pipeline JSON.
//! * [`create_dir_all_synced`] — before writing into a directory that may not
//!   exist yet.  A file's own directory sync does not make its *parents*
//!   durable, so a fresh shard can otherwise take its file down with it.
//! * [`rename_synced`] — to install a file that is already on disk under its
//!   final name.

use std::{
    ffi::OsString,
    fs::File,
    io,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::degraded_read::compare_degraded;

/// Distinguishes the staging files of concurrent writers within one process.
static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Longest file name in bytes that a staging file may use.  255 is what the
/// common filesystems accept, and the destination's own name has to fit inside
/// it alongside the staging suffix.
const MAX_NAME_LEN: usize = 255;

/// How many links to follow before deciding a symlinked destination is a loop.
const MAX_SYMLINK_HOPS: usize = 8;

/// Whether a staged file is read back and compared before it is installed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Verify {
    /// Confirm the storage stack returns what it was given.
    ReadBack,
    /// Trust the write.  For data that can be regenerated from what we kept.
    No,
}

/// Write `bytes` to `path` atomically, confirming what landed before it counts.
///
/// The data is staged in a uniquely named file beside `path`, flushed with
/// `fsync`, read back and compared, and only then renamed over the
/// destination — so returning `Ok` means the bytes reached the device rather
/// than the page cache, and came back identical.
///
/// Rename replaces a directory entry rather than a file's contents, and does
/// so atomically, so anything reading `path` — including the next run after a
/// crash — sees either the previous file whole or the new one whole. Every
/// failure short of the rename leaves the destination exactly as it was, and
/// takes the staging file with it.
///
/// The directory entry is synced afterwards so the *name* survives a power cut
/// too, but that last step is best-effort (see [`sync_parent_dir`]): `Ok`
/// promises the bytes are on the device, not that every filesystem agreed to
/// flush the directory that names them.
pub fn write_verified_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_staged(path, bytes, Verify::ReadBack)
}

/// Write `bytes` to `path` atomically, without reading them back.
///
/// The staging, `fsync`, rename and directory sync of [`write_verified_atomic`]
/// — so a crash leaves either the old file whole or the new one whole — minus
/// the read-back verification.
///
/// Use this for files that can be produced again from something we still have:
/// an export re-renders from its `.rlab`, preferences fall back to defaults, an
/// autosave is superseded by the next one.  Prefer [`write_verified_atomic`]
/// whenever the bytes being written are the only copy.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_staged(path, bytes, Verify::No)
}

fn write_staged(path: &Path, bytes: &[u8], verify: Verify) -> io::Result<()> {
    let dst = resolve_destination(path);
    let staged = staging_path(&dst)?;
    let result = stage_and_replace(&staged, &dst, bytes, verify);
    if result.is_err() {
        // A partial or unverifiable file must not be left lying next to the
        // real one, where a later reader could mistake it for a project.
        remove_staging_file(&staged);
    }
    result
}

fn stage_and_replace(staged: &Path, dst: &Path, bytes: &[u8], verify: Verify) -> io::Result<()> {
    write_and_sync(staged, dst, bytes)?;
    if verify == Verify::ReadBack {
        verify_written(staged, bytes)?;
    }
    std::fs::rename(staged, dst)?;
    sync_parent_dir(dst);
    Ok(())
}

/// Follow a symlinked destination to the file it names.
///
/// `rename` replaces the link itself, so saving over a deliberately symlinked
/// project would silently turn it into a regular file and strand whatever it
/// pointed at.  Resolving first keeps the link intact, and stages beside the
/// real file — which is also the only place the rename is guaranteed to be on
/// one filesystem.
///
/// Anything unresolvable (a dangling link, a loop, a path that does not exist
/// yet) falls back to the path as given, which is the pre-existing behaviour.
fn resolve_destination(path: &Path) -> PathBuf {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_SYMLINK_HOPS {
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {}
            // Not a link, or nothing there yet: this is the file to replace.
            _ => return current,
        }
        let Ok(target) = std::fs::read_link(&current) else {
            return path.to_path_buf();
        };
        current = match current.parent() {
            // A relative link resolves against the directory holding the link.
            Some(dir) if target.is_relative() => dir.join(target),
            _ => target,
        };
    }
    path.to_path_buf()
}

/// Where a write to `dst` is staged: the destination's own directory, since
/// rename is only atomic within one filesystem, under a hidden name unique per
/// process and per call so two writers cannot stage over each other.
///
/// The trailing component keeps the staging file out of every `*.rlab` scan,
/// so an interrupted save cannot be picked up as a project or scrubbed.
fn staging_path(dst: &Path) -> io::Result<PathBuf> {
    let name = dst.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("cannot write to {}: no file name", dst.display()),
        )
    })?;

    let suffix = format!(
        ".tmp-{}-{}",
        std::process::id(),
        STAGE_SEQ.fetch_add(1, Ordering::Relaxed)
    );

    // Naming the staging file after its destination says what a leftover
    // belonged to, but a long project name plus the suffix can run past what
    // the filesystem accepts, which would fail a save that used to work. The
    // suffix alone makes the name unique, so a shortened stem cannot collide.
    let name = name.to_string_lossy();
    let mut keep = name
        .len()
        .min(MAX_NAME_LEN.saturating_sub(suffix.len() + ".".len()));
    while !name.is_char_boundary(keep) {
        keep -= 1;
    }

    let mut staged = OsString::from(".");
    staged.push(&name[..keep]);
    staged.push(suffix);

    // An empty parent means `dst` is a bare file name, and joining onto it
    // keeps the staging file in the same (current) directory.
    Ok(dst.parent().unwrap_or(Path::new("")).join(staged))
}

/// Give the staged file the destination's permissions.
///
/// The rename installs a new inode, so whatever the old file's mode carried —
/// say a deliberately restricted project — would otherwise silently revert to
/// the process umask.  Best effort: a filesystem that cannot express the mode
/// is no reason to fail a save, and a destination that does not exist yet has
/// nothing to inherit.
///
/// Called before the `fsync`, so the mode reaches the device with the data it
/// applies to rather than trailing behind it.
fn inherit_permissions(dst: &Path, staged: &Path) {
    if let Ok(meta) = std::fs::metadata(dst) {
        let _ = std::fs::set_permissions(staged, meta.permissions());
    }
}

/// Discard a staging file that never made it into place.
fn remove_staging_file(staged: &Path) {
    clear_readonly(staged);
    let _ = std::fs::remove_file(staged);
}

/// Windows refuses to delete a file carrying the read-only attribute, and
/// [`inherit_permissions`] may have just copied one from the destination —
/// which would strand the staging file the cleanup exists to remove.
///
/// Unix needs no equivalent: unlinking is governed by the *directory's*
/// permissions, not the file's.
#[cfg(windows)]
fn clear_readonly(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_readonly(false);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(windows))]
fn clear_readonly(_path: &Path) {}

/// Rename `from` over `to` and flush the directory entry that results.
///
/// For installing a file that is already complete on disk — the scrub's
/// repaired copy, a photo moved into Recently Deleted — with the same
/// durability the staged writers get.  Both paths must be on one filesystem
/// for the rename to be atomic, which for the callers here means one
/// directory tree.
pub fn rename_synced(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::rename(from, to)?;
    sync_parent_dir(to);
    Ok(())
}

/// Create `dir` and any missing parents, making each one durable as it goes.
///
/// `create_dir_all` followed by a file write leaves the *directories* unsynced:
/// [`write_verified_atomic`] flushes the entry it created in the innermost
/// directory, but nothing has flushed that directory's own entry in its parent.
/// A power cut after a first import into a fresh `files/ab/cd/` shard can
/// therefore take the shard — and the verified file inside it — with it.
///
/// Each component is synced into its parent before the next one is created
/// inside it, so a directory exists durably before anything depends on it.
pub fn create_dir_all_synced(dir: &Path) -> io::Result<()> {
    if dir.as_os_str().is_empty() || dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        create_dir_all_synced(parent)?;
    }
    match std::fs::create_dir(dir) {
        Ok(()) => sync_parent_dir(dir),
        // Another writer got there first, which is as good as doing it here.
        // Anything else already occupying the name is a real error.
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists && dir.is_dir() => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Flush the directory entry naming `path`.
///
/// The file's own data is already on the device; this is what makes the *name*
/// still point at it after a power loss.  It goes through [`fsync_compat`], so
/// a mount that refuses the macOS device barrier still gets a plain `fsync`
/// rather than nothing at all.
///
/// Best effort even so: by this point the new file is complete, verified and
/// in place, and a few filesystems (network mounts especially) will not sync a
/// directory by any means — reporting that as a failed save would be a lie
/// about bytes that are safely stored.
#[cfg(unix)]
pub fn sync_parent_dir(path: &Path) {
    let dir = path.parent().unwrap_or(Path::new(""));
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    if let Ok(handle) = File::open(dir) {
        let _ = fsync_compat(&handle);
    }
}

/// Windows has no directory handle to sync; the rename's own metadata update
/// is the filesystem's business there.
#[cfg(not(unix))]
pub fn sync_parent_dir(_path: &Path) {}

/// Read `path` back and compare it against the bytes it should contain.
pub fn verify_written(path: &Path, expected: &[u8]) -> io::Result<()> {
    let file = File::open(path)?;
    hint_uncached_reads(&file);

    // The degraded reader means a sector that is already unreadable reports as
    // such instead of collapsing into an opaque EIO. It compares in bounded
    // blocks: `expected` already owns the complete output, so retaining a
    // second complete readback would unnecessarily double that allocation.
    let read_back = compare_degraded(&file, expected)?;

    if read_back.unreadable_bytes != 0 {
        let first_unreadable = read_back
            .first_unreadable
            .expect("unreadable byte count has a first unreadable range");
        return Err(io::Error::other(format!(
            "{} bytes of {} were unreadable immediately after writing at bytes {}..{} — the media is failing",
            read_back.unreadable_bytes,
            path.display(),
            first_unreadable.start,
            first_unreadable.end,
        )));
    }

    if read_back.source_len != expected.len() {
        return Err(io::Error::other(format!(
            "write verification failed for {}: wrote {} bytes, read back {}",
            path.display(),
            expected.len(),
            read_back.source_len
        )));
    }

    if let Some((at, actual)) = read_back.first_difference {
        return Err(io::Error::other(format!(
            "write verification failed for {}: byte {at} of {} differs \
             (wrote {:#04x}, read back {:#04x})",
            path.display(),
            expected.len(),
            expected[at],
            actual
        )));
    }

    Ok(())
}

fn write_and_sync(staged: &Path, dst: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(staged)?;
    file.write_all(bytes)?;
    inherit_permissions(dst, staged);

    // Push the data out of our buffers and the kernel's. Without this the
    // read-back is answered from RAM and proves nothing about the device.
    fsync_compat(&file)?;
    evict_from_cache(&file);
    Ok(())
}

// ── Flushing ────────────────────────────────────────────────────────────────────

/// Flush a file to storage, asking for the strongest barrier the filesystem
/// will actually honour.
///
/// On macOS, `File::sync_all` is `fcntl(F_FULLFSYNC)`: it asks the *device* to
/// empty its own write cache, which is a stronger promise than POSIX `fsync`
/// and the reason a Mac survives a power cut with its data intact.  Not every
/// filesystem implements that fcntl, though.  An SMB mount answers it with
/// `ENOTSUP` (errno 45), and other network and virtual filesystems answer the
/// same refusal as `EINVAL` or `ENOTTY`.  Rust's std has no fallback, so on
/// such a mount every save through [`write_atomic`] or
/// [`write_verified_atomic`] fails outright, with the bytes perfectly fine and
/// nothing wrong but the barrier we asked for.
///
/// So when — and only when — the refusal says the call is not implemented
/// here, this falls back to plain `fsync(2)` and reports that instead.  That
/// is a genuinely weaker guarantee: `fsync` promises the data reached the
/// filesystem, not that the disk at the far end has flushed it out of its
/// cache and onto the platters.  On a network mount, however, the stronger
/// promise was never on offer.  `F_FULLFSYNC` is a local device barrier; the
/// SMB client cannot carry it over the wire to the server's disks, so the
/// choice on that mount is not between a strong flush and a weak one but
/// between a weak flush and refusing to save at all.  The server is where the
/// durability actually lives, and telling it to commit the file is the most
/// this end can do.
///
/// Any other error from `F_FULLFSYNC` is a real failure — a full disk, a
/// broken connection, an I/O error the device reported late — and propagates
/// untouched.  `EINTR` is retried, since a signal is not an answer.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub fn fsync_compat(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    let fd = file.as_raw_fd();

    // SAFETY: `file` is borrowed for the whole call, so `fd` stays open and
    // valid.  Neither fcntl nor fsync takes ownership of the descriptor.
    let full_fsync = || unsafe { libc::fcntl(fd, libc::F_FULLFSYNC) };
    let plain_fsync = || unsafe { libc::fsync(fd) };

    match retry_on_eintr(full_fsync) {
        Ok(()) => Ok(()),
        // The filesystem does not implement the device barrier.  Different
        // implementations phrase that refusal differently; all of these mean
        // the same thing, and none says anything went wrong with the data.
        // Darwin, unlike Linux, gives `ENOTSUP` (45) and `EOPNOTSUPP` (102)
        // separate values, so both have to be named.
        Err(e)
            if matches!(
                e.raw_os_error(),
                Some(libc::ENOTSUP)
                    | Some(libc::EOPNOTSUPP)
                    | Some(libc::EINVAL)
                    | Some(libc::ENOTTY)
            ) =>
        {
            retry_on_eintr(plain_fsync)
        }
        Err(e) => Err(e),
    }
}

/// Run a `-1`-on-failure libc call until it returns something other than
/// `EINTR`.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn retry_on_eintr(mut call: impl FnMut() -> libc::c_int) -> io::Result<()> {
    loop {
        if call() != -1 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Everywhere else `sync_all` is already the strongest flush the platform
/// offers — plain `fsync` on Linux and the BSDs, `FlushFileBuffers` on
/// Windows — and there is nothing to fall back to.
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub fn fsync_compat(file: &File) -> io::Result<()> {
    file.sync_all()
}

// ── Cache hints ───────────────────────────────────────────────────────────────
//
// Both hints are advisory. When the kernel declines, the read-back still
// verifies everything up to the page cache — short writes, a filesystem that
// filled up, and memory corruption between hashing and the write syscall — but
// not the storage stack below it. That is a weaker guarantee, never a wrong
// one: a mismatch always means real trouble.

/// Ask the kernel to drop this file's cached pages so the following read has to
/// reach the device.
#[cfg(target_os = "linux")]
fn evict_from_cache(file: &File) {
    use std::os::fd::AsRawFd;
    // SAFETY: `file` is open for the duration of the call, so the fd is valid.
    // posix_fadvise only advises; it cannot invalidate the descriptor. The
    // result is ignored because failure just means the pages stayed cached.
    unsafe {
        libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED);
    }
}

#[cfg(not(target_os = "linux"))]
fn evict_from_cache(_file: &File) {}

/// Ask for reads on this handle to bypass the cache.  macOS has no
/// `POSIX_FADV_DONTNEED`, but it can mark a descriptor uncached.
#[cfg(target_os = "macos")]
fn hint_uncached_reads(file: &File) {
    use std::os::fd::AsRawFd;
    // SAFETY: as above — `file` outlives the call and F_NOCACHE only sets a
    // per-descriptor flag.
    unsafe {
        libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1);
    }
}

#[cfg(not(target_os = "macos"))]
fn hint_uncached_reads(_file: &File) {}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    fn payload(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    /// Every entry in `dir`, sorted, as file names.
    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn round_trip_verifies_and_writes_the_bytes() {
        let tmp = NamedTempFile::new().unwrap();
        let bytes = payload(300_000);

        write_verified_atomic(tmp.path(), &bytes).unwrap();
        assert_eq!(std::fs::read(tmp.path()).unwrap(), bytes);
    }

    #[test]
    fn empty_payload_is_fine() {
        let tmp = NamedTempFile::new().unwrap();
        write_verified_atomic(tmp.path(), &[]).unwrap();
        assert!(std::fs::read(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn a_successful_write_leaves_only_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("project.rlab");

        write_verified_atomic(&dst, &payload(50_000)).unwrap();
        write_verified_atomic(&dst, &payload(20_000)).unwrap();

        assert_eq!(entries(dir.path()), ["project.rlab"]);
    }

    /// The point of staging: a save that fails after the bytes are written
    /// must cost nothing.  A non-empty directory cannot be renamed over, so
    /// the replace fails at the last step, with the staging file already
    /// written and verified.
    #[test]
    fn a_failed_replace_keeps_the_destination_and_drops_the_staging_file() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("occupied");
        std::fs::create_dir(&dst).unwrap();
        std::fs::write(dst.join("keep"), b"untouched").unwrap();

        let err = write_verified_atomic(&dst, &payload(10_000)).unwrap_err();

        assert_eq!(
            std::fs::read(dst.join("keep")).unwrap(),
            b"untouched",
            "{err}"
        );
        assert_eq!(entries(dir.path()), ["occupied"]);
    }

    #[test]
    fn a_path_without_a_file_name_is_rejected() {
        let err = write_verified_atomic(Path::new(".."), b"x").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn staging_paths_are_unique_and_hidden_beside_the_destination() {
        let dst = Path::new("/photos/ab/cd/project.rlab");
        let a = staging_path(dst).unwrap();
        let b = staging_path(dst).unwrap();

        assert_ne!(a, b);
        for staged in [&a, &b] {
            assert_eq!(staged.parent(), dst.parent());
            let name = staged.file_name().unwrap().to_string_lossy().into_owned();
            assert!(name.starts_with(".project.rlab.tmp-"), "{name}");
            // Never picked up by the library's `*.rlab` walks.
            assert_ne!(staged.extension().unwrap(), "rlab");
        }
    }

    /// Staging must not turn a name the filesystem accepts into one it does
    /// not — a project saved under a very long name still has to save.
    #[test]
    fn a_long_destination_name_still_yields_a_usable_staging_name() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("{}.rlab", "é".repeat(120)); // 246 bytes
        let dst = dir.path().join(&long);

        let staged = staging_path(&dst).unwrap();
        let name = staged.file_name().unwrap().as_encoded_bytes();
        assert!(name.len() <= 255, "{} bytes", name.len());

        write_verified_atomic(&dst, &payload(1000)).unwrap();
        assert_eq!(entries(dir.path()), [long]);
    }

    /// Replacing a file must not quietly change who can read it.
    #[cfg(unix)]
    #[test]
    fn the_destination_keeps_its_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("project.rlab");
        std::fs::write(&dst, payload(100)).unwrap();
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o640)).unwrap();

        write_verified_atomic(&dst, &payload(5_000)).unwrap();

        let mode = std::fs::metadata(&dst).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640, "mode was {mode:o}");
    }

    /// The comparison is the part worth testing directly: no portable test can
    /// make a real write corrupt in transit, so damage is applied afterwards.
    #[test]
    fn a_flipped_byte_is_caught_and_located() {
        let tmp = NamedTempFile::new().unwrap();
        let bytes = payload(100_000);
        write_verified_atomic(tmp.path(), &bytes).unwrap();

        let mut on_disk = bytes.clone();
        on_disk[54_321] ^= 0xFF;
        std::fs::write(tmp.path(), &on_disk).unwrap();

        let err = verify_written(tmp.path(), &bytes).unwrap_err().to_string();
        assert!(err.contains("byte 54321"), "{err}");
    }

    #[test]
    fn a_short_file_is_caught() {
        let tmp = NamedTempFile::new().unwrap();
        let bytes = payload(100_000);
        write_verified_atomic(tmp.path(), &bytes).unwrap();
        std::fs::write(tmp.path(), &bytes[..99_000]).unwrap();

        let err = verify_written(tmp.path(), &bytes).unwrap_err().to_string();
        assert!(err.contains("wrote 100000 bytes, read back 99000"), "{err}");
    }

    #[test]
    fn a_long_file_is_caught() {
        let tmp = NamedTempFile::new().unwrap();
        let bytes = payload(1000);
        write_verified_atomic(tmp.path(), &bytes).unwrap();

        let mut longer = bytes.clone();
        longer.extend_from_slice(b"trailing garbage");
        std::fs::write(tmp.path(), &longer).unwrap();

        let err = verify_written(tmp.path(), &bytes).unwrap_err().to_string();
        assert!(err.contains("read back 1016"), "{err}");
    }

    /// `write_atomic` skips the read-back, not the staging: the destination
    /// still goes from one whole file to the next.
    #[test]
    fn unverified_writes_are_still_staged_and_atomic() {
        let dir = tempfile::tempdir().unwrap();
        let dst = dir.path().join("prefs.yaml");

        write_atomic(&dst, &payload(40_000)).unwrap();
        write_atomic(&dst, b"replaced").unwrap();

        assert_eq!(std::fs::read(&dst).unwrap(), b"replaced");
        assert_eq!(entries(dir.path()), ["prefs.yaml"]);
    }

    /// Renaming replaces the link, so a save through a symlink has to follow it
    /// first or it silently converts the link into a regular file.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_destination_keeps_its_link_and_updates_the_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.rlab");
        let link = dir.path().join("link.rlab");
        std::fs::write(&target, payload(100)).unwrap();
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let bytes = payload(9_000);
        write_verified_atomic(&link, &bytes).unwrap();

        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the save replaced the symlink instead of following it"
        );
        assert_eq!(std::fs::read(&target).unwrap(), bytes);
        assert_eq!(entries(dir.path()), ["link.rlab", "real.rlab"]);
    }

    /// A link pointing at nothing has no target to write through, so the link
    /// itself is the file to create — the same as any other absent destination.
    #[cfg(unix)]
    #[test]
    fn a_dangling_symlink_is_written_as_the_destination() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("dangling.rlab");
        std::os::unix::fs::symlink(dir.path().join("gone.rlab"), &link).unwrap();

        write_verified_atomic(&link, b"fresh").unwrap();
        assert_eq!(std::fs::read(&link).unwrap(), b"fresh");
    }

    /// Two links pointing at each other must not spin the resolver.
    #[cfg(unix)]
    #[test]
    fn a_symlink_loop_terminates() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::os::unix::fs::symlink(&b, &a).unwrap();
        std::os::unix::fs::symlink(&a, &b).unwrap();

        // Either outcome is acceptable; hanging or recursing is not.
        let _ = write_verified_atomic(&a, b"x");
    }

    #[test]
    fn missing_directories_are_created() {
        let dir = tempfile::tempdir().unwrap();
        let leaf = dir.path().join("files").join("ab").join("cd");

        create_dir_all_synced(&leaf).unwrap();
        assert!(leaf.is_dir());

        // Idempotent: the library calls this before every import.
        create_dir_all_synced(&leaf).unwrap();
    }

    /// A regular file occupying a directory's name is a real error, not a
    /// directory that happens to already exist.
    #[test]
    fn a_file_in_the_way_of_a_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let occupied = dir.path().join("files");
        std::fs::write(&occupied, b"not a directory").unwrap();

        assert!(create_dir_all_synced(&occupied.join("ab")).is_err());
    }

    #[test]
    fn a_synced_rename_installs_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("repaired.tmp");
        let to = dir.path().join("photo.rlab");
        std::fs::write(&from, payload(2_000)).unwrap();

        rename_synced(&from, &to).unwrap();

        assert_eq!(std::fs::read(&to).unwrap(), payload(2_000));
        assert_eq!(entries(dir.path()), ["photo.rlab"]);
    }

    /// The flush has to succeed on an ordinary local file, which is the only
    /// case a portable test can reach: reproducing the `ENOTSUP` fallback
    /// needs a mount whose filesystem refuses `F_FULLFSYNC`, and no temp
    /// directory is one.  What this does pin down is that the fcntl path is
    /// wired up correctly and reports success on files of every shape the
    /// writers produce.
    #[test]
    fn flushing_succeeds_on_ordinary_files() {
        let dir = tempfile::tempdir().unwrap();
        for (name, len) in [("empty", 0), ("small", 1), ("page", 4096), ("big", 300_000)] {
            let path = dir.path().join(name);
            let mut file = File::create(&path).unwrap();
            file.write_all(&payload(len)).unwrap();
            fsync_compat(&file).expect(name);

            // A second flush with nothing left to write must be just as happy.
            fsync_compat(&file).expect(name);
        }
    }

    /// A read-only handle has nothing to flush, and must not turn that into a
    /// failed save.  Worth pinning because `F_FULLFSYNC` and `fsync` differ
    /// from `FlushFileBuffers` here, and the fallback must not mistake a
    /// permission refusal for "not implemented".
    #[test]
    fn flushing_a_read_only_handle_is_not_an_error() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), payload(100)).unwrap();
        let file = File::open(tmp.path()).unwrap();
        fsync_compat(&file).unwrap();
    }

    /// One of the two staged writers, named for a failure message.
    type Writer = fn(&Path, &[u8]) -> io::Result<()>;

    /// Both writers flush through [`fsync_compat`], so both have to round-trip
    /// unchanged for payloads that cross the interesting size boundaries.
    #[test]
    fn both_writers_round_trip_through_the_flush() {
        let dir = tempfile::tempdir().unwrap();
        let cases: [(&str, Writer); 2] = [
            ("verified", write_verified_atomic),
            ("unverified", write_atomic),
        ];

        for (label, write) in cases {
            for len in [0, 1, 4095, 4096, 4097, 250_000] {
                let dst = dir.path().join(format!("{label}-{len}.rlab"));
                let bytes = payload(len);
                write(&dst, &bytes).unwrap_or_else(|e| panic!("{label} {len}: {e}"));
                assert_eq!(std::fs::read(&dst).unwrap(), bytes, "{label} {len}");
            }
        }
    }

    #[test]
    fn a_longer_existing_file_is_replaced_whole() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), payload(500_000)).unwrap();

        let bytes = payload(1000);
        write_verified_atomic(tmp.path(), &bytes).unwrap();
        assert_eq!(std::fs::read(tmp.path()).unwrap(), bytes);
    }
}
