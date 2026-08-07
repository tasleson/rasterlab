//! Fault-tolerant reads over failing media.
//!
//! A latent sector error — a sector the drive can no longer return — surfaces
//! as an `EIO` from `read(2)`, not as corrupt bytes.  Studies of large disk
//! populations (Bairavasundaram et al., *An Analysis of Latent Sector Errors in
//! Disk Drives*, SIGMETRICS 2007) put unreadable sectors roughly an order of
//! magnitude ahead of silent corruption in frequency, so a reader that gives up
//! on the first `EIO` never reaches the error-correcting parity that would have
//! covered the loss.  One bad sector should not cost a whole project file.
//!
//! [`read_degraded`] reads everything the media will surrender and zero-fills
//! the rest, reporting which ranges were lost.  Downstream that is a feature:
//! a zero-filled range is indistinguishable from corruption, so it fails its
//! shard hash and becomes an erasure Reed-Solomon can reconstruct.

use std::{fs::File, io, ops::Range, path::Path};

/// Bulk read size.  Large enough that reading a healthy file costs about the
/// same as `std::fs::read`.
const BULK_BLOCK: usize = 1024 * 1024;

/// Granularity the reader falls back to once a bulk read fails, matching the
/// physical sector size of modern drives.  Isolating finer than this buys
/// nothing: the smallest unit the parity can reconstruct is one shard, and
/// shards are never smaller than 4 KiB.
const RETRY_BLOCK: usize = 4096;

/// A random-access byte source whose reads may fail over parts of its range.
///
/// Abstracted over [`File`] so the recovery logic can be exercised against
/// injected media errors, which no portable test can provoke from a real file.
pub trait BlockSource {
    /// Total size in bytes.
    fn size(&self) -> io::Result<u64>;

    /// Read into `buf` starting at `offset`, returning the number of bytes
    /// read.  A return of `Ok(0)` means end of input.
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize>;
}

impl BlockSource for File {
    fn size(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    #[cfg(unix)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(self, buf, offset)
    }

    #[cfg(windows)]
    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(self, buf, offset)
    }
}

/// Contents of a source that may have been partly unreadable.
#[derive(Debug, Clone, Default)]
pub struct DegradedRead {
    /// File contents, with every unreadable range zero-filled.
    pub data: Vec<u8>,
    /// Unreadable ranges, ascending and non-overlapping.  Bounds are rounded
    /// out to [`RETRY_BLOCK`], the granularity the reader can isolate to.
    pub unreadable: Vec<Range<usize>>,
}

impl DegradedRead {
    /// Total bytes the media refused to return.
    pub fn unreadable_bytes(&self) -> usize {
        self.unreadable.iter().map(|r| r.len()).sum()
    }

    /// Whether every byte was read successfully.
    pub fn is_intact(&self) -> bool {
        self.unreadable.is_empty()
    }
}

/// Open `path` and read it, tolerating unreadable regions.
///
/// Fails only if the file cannot be opened or stat'd; media errors within it
/// are reported through [`DegradedRead::unreadable`] rather than as an error.
pub fn read_degraded_file(path: &Path) -> io::Result<DegradedRead> {
    read_degraded(&File::open(path)?)
}

/// Read `src` in full, zero-filling whatever it refuses to return.
pub fn read_degraded<S: BlockSource + ?Sized>(src: &S) -> io::Result<DegradedRead> {
    let len = usize::try_from(src.size()?).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "file larger than address space")
    })?;

    let mut data = vec![0u8; len];
    let mut unreadable: Vec<Range<usize>> = Vec::new();

    let mut pos = 0usize;
    while pos < len {
        let end = (pos + BULK_BLOCK).min(len);
        if read_exact_at(src, pos, &mut data[pos..end]).is_ok() {
            pos = end;
            continue;
        }

        // The bulk read failed, but a drive failing one sector fails the whole
        // request it was part of. Re-read the block in sector-sized pieces so
        // the loss is confined to the sectors actually gone, and so anything
        // the failed bulk read left half-written is overwritten.
        let mut sub = pos;
        while sub < end {
            let sub_end = (sub + RETRY_BLOCK).min(end);
            if read_exact_at(src, sub, &mut data[sub..sub_end]).is_err() {
                data[sub..sub_end].fill(0);
                push_unreadable(&mut unreadable, sub..sub_end);
            }
            sub = sub_end;
        }
        pos = end;
    }

    Ok(DegradedRead { data, unreadable })
}

/// Fill `buf` completely, retrying interrupted reads and treating a premature
/// end of input as a failure — a file that shrank mid-read has lost those bytes
/// just as surely as an unreadable sector has.
fn read_exact_at<S: BlockSource + ?Sized>(
    src: &S,
    offset: usize,
    buf: &mut [u8],
) -> io::Result<()> {
    let mut filled = 0usize;
    while filled < buf.len() {
        match src.read_at((offset + filled) as u64, &mut buf[filled..]) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Append `range`, coalescing it with the previous entry when they touch, so a
/// long run of bad sectors reports as one range rather than thousands.
fn push_unreadable(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    match ranges.last_mut() {
        Some(last) if last.end == range.start => last.end = range.end,
        _ => ranges.push(range),
    }
}

#[cfg(test)]
// These fixtures really do want a one-element list of ranges — the lint's
// suspicion, that `vec![a..b]` was meant to be `(a..b).collect()`, does not
// apply to a bad-sector map.
#[allow(clippy::single_range_in_vec_init)]
mod tests {
    use super::*;

    /// A source that fails any read overlapping one of `bad`, mimicking a drive
    /// that fails the entire request a dead sector was part of.
    struct FlakySource {
        data: Vec<u8>,
        bad: Vec<Range<usize>>,
        /// Cap on bytes returned per call, to exercise short reads.
        max_read: usize,
    }

    impl FlakySource {
        fn new(len: usize, bad: Vec<Range<usize>>) -> Self {
            Self {
                data: (0..len).map(|i| (i % 251) as u8).collect(),
                bad,
                max_read: usize::MAX,
            }
        }
    }

    impl BlockSource for FlakySource {
        fn size(&self) -> io::Result<u64> {
            Ok(self.data.len() as u64)
        }

        fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
            let start = offset as usize;
            let end = (start + buf.len().min(self.max_read)).min(self.data.len());
            if start >= end {
                return Ok(0);
            }
            if self.bad.iter().any(|b| b.start < end && start < b.end) {
                return Err(io::Error::other("simulated media error"));
            }
            buf[..end - start].copy_from_slice(&self.data[start..end]);
            Ok(end - start)
        }
    }

    #[test]
    fn intact_source_reads_verbatim() {
        let src = FlakySource::new(3 * BULK_BLOCK + 1234, vec![]);
        let got = read_degraded(&src).unwrap();
        assert!(got.is_intact());
        assert_eq!(got.unreadable_bytes(), 0);
        assert_eq!(got.data, src.data);
    }

    #[test]
    fn unreadable_range_is_zero_filled_and_reported() {
        let bad = 5000..5100;
        let src = FlakySource::new(64 * 1024, vec![bad.clone()]);
        let got = read_degraded(&src).unwrap();

        // Loss is reported at retry granularity, so the whole containing sector.
        assert_eq!(got.unreadable, vec![4096..8192]);
        assert_eq!(got.unreadable_bytes(), RETRY_BLOCK);
        assert!(!got.is_intact());

        assert!(got.data[4096..8192].iter().all(|&b| b == 0));
        assert_eq!(got.data[..4096], src.data[..4096]);
        assert_eq!(got.data[8192..], src.data[8192..]);
    }

    /// A single dead sector inside a bulk block must not cost the whole block.
    #[test]
    fn isolation_confines_loss_to_one_sector() {
        let src = FlakySource::new(2 * BULK_BLOCK, vec![777_000..777_001]);
        let got = read_degraded(&src).unwrap();
        assert_eq!(got.unreadable_bytes(), RETRY_BLOCK);
        assert_eq!(got.unreadable, vec![774_144..778_240]);
    }

    #[test]
    fn adjacent_bad_sectors_coalesce() {
        let src = FlakySource::new(64 * 1024, vec![8192..20_000]);
        let got = read_degraded(&src).unwrap();
        assert_eq!(got.unreadable, vec![8192..20_480]);
    }

    #[test]
    fn damage_spanning_a_bulk_block_boundary_is_reported_as_one_range() {
        let src = FlakySource::new(3 * BULK_BLOCK, vec![BULK_BLOCK - 10..BULK_BLOCK + 10]);
        let got = read_degraded(&src).unwrap();
        assert_eq!(
            got.unreadable,
            vec![BULK_BLOCK - RETRY_BLOCK..BULK_BLOCK + RETRY_BLOCK]
        );
    }

    #[test]
    fn short_reads_are_retried_until_the_buffer_fills() {
        let mut src = FlakySource::new(200_000, vec![]);
        src.max_read = 1000;
        let got = read_degraded(&src).unwrap();
        assert!(got.is_intact());
        assert_eq!(got.data, src.data);
    }

    #[test]
    fn a_wholly_unreadable_file_yields_zeros_not_an_error() {
        let len = 16 * 1024;
        let src = FlakySource::new(len, vec![0..len]);
        let got = read_degraded(&src).unwrap();
        assert_eq!(got.unreadable, vec![0..len]);
        assert!(got.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn empty_source_is_intact() {
        let got = read_degraded(&FlakySource::new(0, vec![])).unwrap();
        assert!(got.is_intact());
        assert!(got.data.is_empty());
    }

    #[test]
    fn real_files_read_through_the_file_impl() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let payload: Vec<u8> = (0..100_000).map(|i| (i % 251) as u8).collect();
        std::fs::write(tmp.path(), &payload).unwrap();

        let got = read_degraded_file(tmp.path()).unwrap();
        assert!(got.is_intact());
        assert_eq!(got.data, payload);
    }
}
