//! Integration tests for the v5 `RECC` layout.
//!
//! v5 exists because v4 could not recover from three things: a corrupt chunk
//! length field (which severed the chain walk that located the parity),
//! truncation deep enough to reach the first of two adjacent tail copies, and
//! any truncation from the front.  Every case below is one of those, plus the
//! v4 files that must keep working.

// Bad-sector maps are genuinely one-element lists of ranges, not a mistaken
// `vec![a..b]` that meant `(a..b).collect()`.
#![allow(clippy::single_range_in_vec_init)]

use rasterlab_core::{
    degraded_read::DegradedRead,
    pipeline::PipelineState,
    project::{
        FORMAT_VERSION_V4, FORMAT_VERSION_V5, RlabFile, RlabMeta, SavedCopy,
        read_original_filename, read_original_hash, verify_and_repair, verify_and_repair_degraded,
    },
};
use tempfile::NamedTempFile;

/// 1 MiB of `ORIG` puts the fixture on the large-file parity path (shards grow
/// past the 4 KiB minimum), which is where the interesting budgets live.  The
/// fill byte never forms the sequence `RECC`, so scanning for the tag in test
/// helpers stays unambiguous.
const ORIG_LEN: usize = 1024 * 1024;
const ORIG_FILL: u8 = 0x55;

fn fixture() -> RlabFile {
    let meta = RlabMeta::new("0.3.0", Some("test.jpg"), 1024, 1024);
    let copies = vec![SavedCopy {
        name: "Copy 1".into(),
        pipeline_state: PipelineState {
            entries: vec![],
            cursor: 0,
        },
    }];
    RlabFile::new(meta, vec![ORIG_FILL; ORIG_LEN], copies, 0, None)
}

fn write_v5_bytes() -> Vec<u8> {
    let tmp = NamedTempFile::new().unwrap();
    fixture().write_v5(tmp.path()).unwrap();
    std::fs::read(tmp.path()).unwrap()
}

fn write_v4_bytes() -> Vec<u8> {
    let tmp = NamedTempFile::new().unwrap();
    fixture().write_v4(tmp.path()).unwrap();
    std::fs::read(tmp.path()).unwrap()
}

// ── Layout introspection ──────────────────────────────────────────────────────

/// Offsets and shard geometry of a v5 file, derived from its own bytes.
struct Layout {
    recc_a: usize,
    recc_b: usize,
    /// On-disk size of one `RECC` chunk (tag + length + payload + hash).
    chunk_len: usize,
    protected_start: usize,
    protected_len: usize,
    shard_size: usize,
    data_shards: usize,
    parity_shards: usize,
    /// Offset of the `META` tag, i.e. the first content chunk.
    meta: usize,
    file_len: usize,
}

fn tag_offsets(bytes: &[u8], tag: &[u8; 4]) -> Vec<usize> {
    (0..bytes.len().saturating_sub(4))
        .filter(|&i| &bytes[i..i + 4] == tag)
        .collect()
}

fn layout(bytes: &[u8]) -> Layout {
    let recc = tag_offsets(bytes, b"RECC");
    assert_eq!(recc.len(), 2, "expected exactly two RECC copies");

    let payload_start = recc[0] + 12;
    let payload_len =
        u64::from_le_bytes(bytes[recc[0] + 4..recc[0] + 12].try_into().unwrap()) as usize;
    let shard_size =
        u32::from_le_bytes(bytes[payload_start..payload_start + 4].try_into().unwrap()) as usize;
    let data_shards = u32::from_le_bytes(
        bytes[payload_start + 4..payload_start + 8]
            .try_into()
            .unwrap(),
    ) as usize;
    let parity_shards = u32::from_le_bytes(
        bytes[payload_start + 8..payload_start + 12]
            .try_into()
            .unwrap(),
    ) as usize;
    let protected_len = u64::from_le_bytes(
        bytes[payload_start + 12..payload_start + 20]
            .try_into()
            .unwrap(),
    ) as usize;

    let chunk_len = 12 + payload_len + 32;
    Layout {
        recc_a: recc[0],
        recc_b: recc[1],
        chunk_len,
        protected_start: recc[0] + chunk_len,
        protected_len,
        shard_size,
        data_shards,
        parity_shards,
        meta: tag_offsets(bytes, b"META")[0],
        file_len: bytes.len(),
    }
}

// ── Damage helpers ────────────────────────────────────────────────────────────

fn flip(bytes: &mut [u8], at: usize, len: usize) {
    for b in &mut bytes[at..at + len] {
        *b ^= 0xFF;
    }
}

/// Drop `n` bytes from the front, shifting everything that remains.
fn cut_head(bytes: &mut Vec<u8>, n: usize) {
    bytes.drain(..n);
}

fn cut_tail(bytes: &mut Vec<u8>, n: usize) {
    bytes.truncate(bytes.len() - n);
}

/// Run `verify_and_repair` over `bytes` and, on success, confirm the repaired
/// file parses and its `ORIG` payload came back byte-exact.
fn repair(bytes: &[u8]) -> Option<RlabFile> {
    let damaged = NamedTempFile::new().unwrap();
    std::fs::write(damaged.path(), bytes).unwrap();
    let out = NamedTempFile::new().unwrap();

    let report = verify_and_repair(damaged.path(), Some(out.path())).ok()?;
    if !report.repaired {
        return None;
    }
    let file = RlabFile::read(out.path()).expect("repaired file must parse");
    assert_eq!(file.original_bytes.len(), ORIG_LEN);
    assert!(
        file.original_bytes.iter().all(|&b| b == ORIG_FILL),
        "ORIG payload was not restored byte-exact"
    );
    Some(file)
}

// ── Layout ────────────────────────────────────────────────────────────────────

#[test]
fn v5_roundtrip_clean() {
    let tmp = NamedTempFile::new().unwrap();
    fixture().write_v5(tmp.path()).unwrap();

    let loaded = RlabFile::read(tmp.path()).unwrap();
    assert_eq!(loaded.format_version, FORMAT_VERSION_V5);
    assert_eq!(loaded.original_bytes.len(), ORIG_LEN);

    let report = verify_and_repair(tmp.path(), None).unwrap();
    assert!(report.file_hash_ok);
    assert!(report.recc_present);
    assert!(report.damaged_chunks.is_empty());
    assert!(!report.repaired);
    assert_eq!(report.format_version, Some(FORMAT_VERSION_V5));
}

#[test]
fn v5_brackets_content_with_one_parity_copy_at_each_end() {
    let bytes = write_v5_bytes();
    let l = layout(&bytes);

    // Leading copy sits immediately after the 10-byte header, ahead of META.
    assert_eq!(l.recc_a, 10);
    assert!(l.meta > l.recc_a && l.meta < l.recc_b);

    // Trailing copy closes the protected region, followed only by the file hash.
    assert_eq!(l.recc_b, l.protected_start + l.protected_len);
    assert_eq!(l.file_len, l.recc_b + l.chunk_len + 32);

    // Both copies are byte-identical.
    assert_eq!(
        bytes[l.recc_a..l.recc_a + l.chunk_len],
        bytes[l.recc_b..l.recc_b + l.chunk_len]
    );
}

// ── The v4 failure cases ──────────────────────────────────────────────────────

/// A corrupt length field used to sever the chunk-chain walk that located the
/// parity, making a single damaged shard unrecoverable.  Parity is now found by
/// signature scan, so the chain no longer gates recovery.
#[test]
fn corrupt_length_field_no_longer_hides_the_parity() {
    for (name, offset_of) in [
        (
            "META length",
            (|l: &Layout| l.meta + 4) as fn(&Layout) -> usize,
        ),
        ("leading RECC length", |l: &Layout| l.recc_a + 4),
    ] {
        let mut bytes = write_v5_bytes();
        let l = layout(&bytes);
        flip(&mut bytes, offset_of(&l), 8);
        assert!(
            repair(&bytes).is_some(),
            "{name} field must stay repairable"
        );
    }
}

/// Truncation that takes the file hash and the entire trailing parity copy —
/// the case v4 got wrong by 32 bytes, because it always peeled the last 32
/// bytes off as a file hash and so clipped the surviving copy's own digest.
#[test]
fn end_truncation_through_the_trailing_parity_copy_repairs() {
    let mut bytes = write_v5_bytes();
    let l = layout(&bytes);
    cut_tail(&mut bytes, 32 + l.chunk_len);

    assert_eq!(bytes.len(), l.recc_b);
    assert!(repair(&bytes).is_some());
}

/// Truncation from the front shifts every offset, so v4 had no way to recover
/// shard alignment.  The trailing copy's offset plus its recorded
/// `protected_len` now pin the original alignment exactly, turning the missing
/// prefix into ordinary erased shards.
#[test]
fn head_truncation_repairs_up_to_the_parity_budget() {
    let bytes = write_v5_bytes();
    let l = layout(&bytes);

    // Everything ahead of the protected region is free: the header is two
    // constants and the leading parity copy is redundant with the trailing one.
    for lost_shards in [0, 1, l.parity_shards] {
        let mut damaged = bytes.clone();
        cut_head(&mut damaged, l.protected_start + lost_shards * l.shard_size);
        assert!(
            repair(&damaged).is_some(),
            "losing {lost_shards} leading shards must repair"
        );
    }

    // One shard past the budget there is nothing left to reconstruct from.
    let mut damaged = bytes.clone();
    cut_head(
        &mut damaged,
        l.protected_start + (l.parity_shards + 1) * l.shard_size,
    );
    assert!(repair(&damaged).is_none());
}

#[test]
fn end_truncation_repairs_up_to_the_parity_budget() {
    let bytes = write_v5_bytes();
    let l = layout(&bytes);

    // Cut back to a shard boundary so the erasure count is exact.
    let keep_shards = l.data_shards - l.parity_shards;
    let mut damaged = bytes.clone();
    damaged.truncate(l.protected_start + keep_shards * l.shard_size);
    assert!(
        repair(&damaged).is_some(),
        "losing exactly the parity budget from the tail must repair"
    );

    let mut damaged = bytes.clone();
    damaged.truncate(l.protected_start + (keep_shards - 1) * l.shard_size);
    assert!(repair(&damaged).is_none());
}

/// Overwriting the leading bytes in place (no shift) hits the magic and the
/// first content chunk's header — the parity must still be reachable.
#[test]
fn leading_overwrite_repairs() {
    let mut bytes = write_v5_bytes();
    flip(&mut bytes, 0, 512);
    let repaired = repair(&bytes).expect("in-place head damage must repair");
    assert_eq!(repaired.format_version, FORMAT_VERSION_V5);
}

// ── Limits that remain ────────────────────────────────────────────────────────

#[test]
fn damage_beyond_the_parity_budget_still_fails() {
    let bytes = write_v5_bytes();
    let l = layout(&bytes);

    let mut damaged = bytes.clone();
    for i in 0..=l.parity_shards {
        flip(&mut damaged, l.protected_start + i * l.shard_size, 16);
    }
    assert!(repair(&damaged).is_none());
}

#[test]
fn both_parity_copies_destroyed_is_unrepairable() {
    let mut bytes = write_v5_bytes();
    let l = layout(&bytes);
    flip(&mut bytes, l.recc_a + 12, 64);
    flip(&mut bytes, l.recc_b + 12, 64);

    // A single damaged data shard would otherwise be trivially correctable.
    flip(&mut bytes, l.protected_start + 3 * l.shard_size, 16);
    assert!(repair(&bytes).is_none());
}

// ── v4 compatibility ──────────────────────────────────────────────────────────

#[test]
fn v4_files_still_read_and_repair() {
    let tmp = NamedTempFile::new().unwrap();
    fixture().write_v4(tmp.path()).unwrap();
    assert_eq!(
        RlabFile::read(tmp.path()).unwrap().format_version,
        FORMAT_VERSION_V4
    );

    // The v4 protected region includes the file header, so shard 0 sits at
    // offset 0 rather than after a leading parity copy.
    let mut bytes = std::fs::read(tmp.path()).unwrap();
    flip(&mut bytes, 5000, 64);

    let repaired = repair(&bytes).expect("v4 damage must repair");
    assert_eq!(
        repaired.format_version, FORMAT_VERSION_V5,
        "repair should migrate v4 input to the v5 layout"
    );
}

/// The v4 tail-truncation cliff: both copies are adjacent, so losing the second
/// one leaves the first only if the cut stops short of it.  Repair must still
/// handle the case that v4's reader mishandled.
#[test]
fn v4_end_truncation_through_second_copy_repairs() {
    let bytes = write_v4_bytes();
    let recc = tag_offsets(&bytes, b"RECC");
    assert_eq!(recc.len(), 2);

    let mut damaged = bytes.clone();
    damaged.truncate(recc[1]);
    assert!(repair(&damaged).is_some());
}

// ── Unreadable media ──────────────────────────────────────────────────────────
//
// A latent sector error surfaces as an EIO, not as corrupt bytes, and is the
// most common disk fault by a wide margin. The degraded reader turns the lost
// sectors into zeros plus a range list; from there they are ordinary erasures.

/// Build the `DegradedRead` a failing drive would produce: `ranges` zeroed and
/// reported, everything else verbatim.
fn with_bad_sectors(bytes: &[u8], ranges: &[std::ops::Range<usize>]) -> DegradedRead {
    let mut data = bytes.to_vec();
    for r in ranges {
        data[r.clone()].fill(0);
    }
    DegradedRead {
        data,
        unreadable: ranges.to_vec(),
    }
}

fn repair_degraded(read: &DegradedRead) -> Option<RlabFile> {
    let out = NamedTempFile::new().unwrap();
    let report = verify_and_repair_degraded(read, Some(out.path())).ok()?;
    assert_eq!(report.unreadable_bytes, read.unreadable_bytes());
    if !report.repaired {
        return None;
    }
    let file = RlabFile::read(out.path()).expect("repaired file must parse");
    assert!(
        file.original_bytes.len() == ORIG_LEN
            && file.original_bytes.iter().all(|&b| b == ORIG_FILL),
        "ORIG payload was not restored byte-exact"
    );
    Some(file)
}

#[test]
fn unreadable_sectors_are_reconstructed_from_parity() {
    let bytes = write_v5_bytes();
    let l = layout(&bytes);

    // Scattered dead sectors across the content, well inside the budget.
    let ranges: Vec<_> = (0..5)
        .map(|i| {
            let at = l.protected_start + (i * 7 + 2) * l.shard_size + 512;
            at..at + 4096
        })
        .collect();

    let read = with_bad_sectors(&bytes, &ranges);
    assert!(repair_degraded(&read).is_some());
}

#[test]
fn unreadable_sectors_inside_a_parity_copy_survive() {
    let bytes = write_v5_bytes();
    let l = layout(&bytes);

    // The leading copy is unreadable; the trailing one carries the repair.
    let read = with_bad_sectors(&bytes, &[l.recc_a..l.recc_a + 3 * 4096]);
    assert!(repair_degraded(&read).is_some());
}

#[test]
fn unreadable_sectors_beyond_the_budget_still_fail() {
    let bytes = write_v5_bytes();
    let l = layout(&bytes);

    let ranges: Vec<_> = (0..=l.parity_shards)
        .map(|i| {
            let at = l.protected_start + i * l.shard_size;
            at..at + 4096
        })
        .collect();

    let read = with_bad_sectors(&bytes, &ranges);
    assert!(repair_degraded(&read).is_none());
}

/// When the bytes the reader substituted for a dead sector happen to match what
/// was there, nothing fails a hash — but the media is still failing, so the file
/// must be rewritten to relocate it rather than reported as clean.
#[test]
fn verifying_content_is_still_rewritten_when_sectors_were_unreadable() {
    let read = DegradedRead {
        data: write_v5_bytes(),
        unreadable: vec![0..4096],
    };

    let out = NamedTempFile::new().unwrap();
    let report = verify_and_repair_degraded(&read, Some(out.path())).unwrap();

    assert!(report.file_hash_ok, "content itself is intact");
    assert!(report.damaged_chunks.is_empty());
    assert_eq!(report.unreadable_bytes, 4096);
    assert!(
        report.repaired,
        "a file on failing media must be rewritten even when its bytes verify"
    );
    RlabFile::read(out.path()).expect("rewritten file must parse");
}

// ── Identity ──────────────────────────────────────────────────────────────────

/// The library names files for the Blake3 of their embedded original, so this
/// is the value that lets a caller tell whether a file is the photo its path
/// claims. It must agree with hashing ORIG directly, and must not require
/// loading the payload.
#[test]
fn read_original_hash_matches_the_embedded_payload() {
    let tmp = NamedTempFile::new().unwrap();
    fixture().write_v5(tmp.path()).unwrap();

    let expected = blake3::hash(&vec![ORIG_FILL; ORIG_LEN]);
    assert_eq!(
        read_original_hash(tmp.path()).unwrap(),
        *expected.as_bytes()
    );

    // Also agrees with what a full parse reports.
    assert_eq!(
        read_original_hash(tmp.path()).unwrap(),
        RlabFile::read(tmp.path()).unwrap().original_hash
    );
}

#[test]
fn read_original_hash_works_across_layouts() {
    for (label, write) in [
        (
            "v3",
            (|f: &RlabFile, p: &std::path::Path| f.write(p))
                as fn(&RlabFile, &std::path::Path) -> rasterlab_core::error::RasterResult<()>,
        ),
        ("v4", |f: &RlabFile, p: &std::path::Path| f.write_v4(p)),
        ("v5", |f: &RlabFile, p: &std::path::Path| f.write_v5(p)),
    ] {
        let tmp = NamedTempFile::new().unwrap();
        write(&fixture(), tmp.path()).unwrap();
        assert_eq!(
            read_original_hash(tmp.path()).unwrap(),
            *blake3::hash(&vec![ORIG_FILL; ORIG_LEN]).as_bytes(),
            "{label}"
        );
    }
}

#[test]
fn read_original_hash_rejects_a_non_rlab_file() {
    let tmp = NamedTempFile::new().unwrap();
    std::fs::write(tmp.path(), b"not a project file at all").unwrap();
    assert!(read_original_hash(tmp.path()).is_err());
}

/// A library file's own name is the Blake3 of its content, so anything showing
/// the user a list of frames has to get the name from inside the file. `LMTA`
/// carries what was imported; an editor-only project falls back to the file
/// name in its `META` source path.
#[test]
fn read_original_filename_prefers_library_metadata() {
    let tmp = NamedTempFile::new().unwrap();

    // Editor-only project: no LMTA, so the META source path answers.
    let mut file = fixture();
    file.write_v5(tmp.path()).unwrap();
    assert_eq!(
        read_original_filename(tmp.path()).unwrap().as_deref(),
        Some("test.jpg"),
    );

    // Imported photo: LMTA wins, and the meaningless hashed path never shows.
    file.set_lmta(Some(rasterlab_core::library_meta::LibraryMeta {
        original_filename: Some("DSC_0042.NEF".to_owned()),
        source_path: Some("/cards/DCIM/DSC_0042.NEF".to_owned()),
        ..Default::default()
    }));
    file.write_v5(tmp.path()).unwrap();
    assert_eq!(
        read_original_filename(tmp.path()).unwrap().as_deref(),
        Some("DSC_0042.NEF"),
    );
}

#[test]
fn read_original_filename_is_none_without_a_recorded_source() {
    let tmp = NamedTempFile::new().unwrap();
    let meta = RlabMeta::new("0.3.0", None::<String>, 8, 8);
    let copies = vec![SavedCopy {
        name: "Copy 1".into(),
        pipeline_state: PipelineState {
            entries: vec![],
            cursor: 0,
        },
    }];
    RlabFile::new(meta, vec![ORIG_FILL; 64], copies, 0, None)
        .write_v5(tmp.path())
        .unwrap();

    assert_eq!(read_original_filename(tmp.path()).unwrap(), None);
}

#[test]
fn read_original_filename_works_across_layouts() {
    for (label, write) in [
        (
            "v3",
            (|f: &RlabFile, p: &std::path::Path| f.write(p))
                as fn(&RlabFile, &std::path::Path) -> rasterlab_core::error::RasterResult<()>,
        ),
        ("v4", |f: &RlabFile, p: &std::path::Path| f.write_v4(p)),
        ("v5", |f: &RlabFile, p: &std::path::Path| f.write_v5(p)),
    ] {
        let tmp = NamedTempFile::new().unwrap();
        write(&fixture(), tmp.path()).unwrap();
        assert_eq!(
            read_original_filename(tmp.path()).unwrap().as_deref(),
            Some("test.jpg"),
            "{label}",
        );
    }
}
