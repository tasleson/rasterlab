//! Writes that confirm what actually landed on the media.
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

use std::{
    ffi::OsString,
    fs::File,
    io,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::degraded_read::read_degraded;

/// Distinguishes the staging files of concurrent writers within one process.
static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Longest file name in bytes that a staging file may use.  255 is what the
/// common filesystems accept, and the destination's own name has to fit inside
/// it alongside the staging suffix.
const MAX_NAME_LEN: usize = 255;

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
pub fn write_verified_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let staged = staging_path(path)?;
    let result = stage_and_replace(&staged, path, bytes);
    if result.is_err() {
        // A partial or unverifiable file must not be left lying next to the
        // real one, where a later reader could mistake it for a project.
        let _ = std::fs::remove_file(&staged);
    }
    result
}

fn stage_and_replace(staged: &Path, dst: &Path, bytes: &[u8]) -> io::Result<()> {
    write_and_sync(staged, bytes)?;
    verify_written(staged, bytes)?;
    inherit_permissions(dst, staged);
    std::fs::rename(staged, dst)?;
    sync_dir_of(dst);
    Ok(())
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
/// a library file's read-only bit, a deliberately restricted project — would
/// otherwise silently revert to the process umask.  Best effort: a filesystem
/// that cannot express the mode is no reason to fail a save, and a destination
/// that does not exist yet has nothing to inherit.
fn inherit_permissions(dst: &Path, staged: &Path) {
    if let Ok(meta) = std::fs::metadata(dst) {
        let _ = std::fs::set_permissions(staged, meta.permissions());
    }
}

/// Flush the directory entry the rename just created.
///
/// The file's own data is already on the device; this is what makes the *name*
/// still point at it after a power loss.  Best effort: by this point the new
/// file is complete, verified and in place, and some filesystems (network
/// mounts especially) refuse to sync a directory at all — reporting that as a
/// failed save would be a lie about bytes that are safely stored.
#[cfg(unix)]
fn sync_dir_of(path: &Path) {
    let dir = path.parent().unwrap_or(Path::new(""));
    let dir = if dir.as_os_str().is_empty() {
        Path::new(".")
    } else {
        dir
    };
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
}

/// Windows has no directory handle to sync; the rename's own metadata update
/// is the filesystem's business there.
#[cfg(not(unix))]
fn sync_dir_of(_path: &Path) {}

/// Read `path` back and compare it against the bytes it should contain.
pub fn verify_written(path: &Path, expected: &[u8]) -> io::Result<()> {
    let file = File::open(path)?;
    hint_uncached_reads(&file);

    // The degraded reader means a sector that is already unreadable reports as
    // such instead of collapsing into an opaque EIO.
    let read_back = read_degraded(&file)?;

    if !read_back.is_intact() {
        return Err(io::Error::other(format!(
            "{} bytes of {} were unreadable immediately after writing — the media is failing",
            read_back.unreadable_bytes(),
            path.display()
        )));
    }

    if read_back.data.len() != expected.len() {
        return Err(io::Error::other(format!(
            "write verification failed for {}: wrote {} bytes, read back {}",
            path.display(),
            expected.len(),
            read_back.data.len()
        )));
    }

    if let Some(at) = first_difference(expected, &read_back.data) {
        return Err(io::Error::other(format!(
            "write verification failed for {}: byte {at} of {} differs \
             (wrote {:#04x}, read back {:#04x})",
            path.display(),
            expected.len(),
            expected[at],
            read_back.data[at]
        )));
    }

    Ok(())
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(path)?;
    file.write_all(bytes)?;

    // Push the data out of our buffers and the kernel's. Without this the
    // read-back is answered from RAM and proves nothing about the device.
    file.sync_all()?;
    evict_from_cache(&file);
    Ok(())
}

fn first_difference(a: &[u8], b: &[u8]) -> Option<usize> {
    a.iter().zip(b).position(|(x, y)| x != y)
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

    #[test]
    fn a_longer_existing_file_is_replaced_whole() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), payload(500_000)).unwrap();

        let bytes = payload(1000);
        write_verified_atomic(tmp.path(), &bytes).unwrap();
        assert_eq!(std::fs::read(tmp.path()).unwrap(), bytes);
    }
}
