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
//! [`write_verified`] closes that window at save time, while the correct bytes
//! are still in memory and the failure costs a retry, rather than leaving it to
//! be discovered by a scrub weeks later when the good copy is long gone.

use std::{fs::File, io, io::Write, path::Path};

use crate::degraded_read::read_degraded;

/// Write `bytes` to `path`, then read them back and confirm they match.
///
/// The write is flushed with `fsync` before the read-back, so returning `Ok`
/// also means the data reached the device rather than merely the page cache.
pub fn write_verified(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_and_sync(path, bytes)?;
    verify_written(path, bytes)
}

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

    #[test]
    fn round_trip_verifies_and_writes_the_bytes() {
        let tmp = NamedTempFile::new().unwrap();
        let bytes = payload(300_000);

        write_verified(tmp.path(), &bytes).unwrap();
        assert_eq!(std::fs::read(tmp.path()).unwrap(), bytes);
    }

    #[test]
    fn empty_payload_is_fine() {
        let tmp = NamedTempFile::new().unwrap();
        write_verified(tmp.path(), &[]).unwrap();
        assert!(std::fs::read(tmp.path()).unwrap().is_empty());
    }

    /// The comparison is the part worth testing directly: no portable test can
    /// make a real write corrupt in transit, so damage is applied afterwards.
    #[test]
    fn a_flipped_byte_is_caught_and_located() {
        let tmp = NamedTempFile::new().unwrap();
        let bytes = payload(100_000);
        write_verified(tmp.path(), &bytes).unwrap();

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
        write_verified(tmp.path(), &bytes).unwrap();
        std::fs::write(tmp.path(), &bytes[..99_000]).unwrap();

        let err = verify_written(tmp.path(), &bytes).unwrap_err().to_string();
        assert!(err.contains("wrote 100000 bytes, read back 99000"), "{err}");
    }

    #[test]
    fn a_long_file_is_caught() {
        let tmp = NamedTempFile::new().unwrap();
        let bytes = payload(1000);
        write_verified(tmp.path(), &bytes).unwrap();

        let mut longer = bytes.clone();
        longer.extend_from_slice(b"trailing garbage");
        std::fs::write(tmp.path(), &longer).unwrap();

        let err = verify_written(tmp.path(), &bytes).unwrap_err().to_string();
        assert!(err.contains("read back 1016"), "{err}");
    }

    #[test]
    fn write_verified_overwrites_a_longer_existing_file() {
        let tmp = NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), payload(500_000)).unwrap();

        let bytes = payload(1000);
        write_verified(tmp.path(), &bytes).unwrap();
        assert_eq!(std::fs::read(tmp.path()).unwrap(), bytes);
    }
}
