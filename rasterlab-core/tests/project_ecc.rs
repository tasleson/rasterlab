//! Integration tests for v4 RECC encode / verify / repair.

use rasterlab_core::{
    pipeline::PipelineState,
    project::{FORMAT_VERSION_V4, RlabFile, RlabMeta, SavedCopy, verify_and_repair},
};
use tempfile::NamedTempFile;

fn make_rlab() -> RlabFile {
    let meta = RlabMeta::new("0.2.0", Some("test.jpg"), 100, 100);
    // Use 32 KiB so ORIG data spans well past any header offset.
    let orig = vec![0x55u8; 32 * 1024];
    let copies = vec![SavedCopy {
        name: "Copy 1".into(),
        pipeline_state: PipelineState {
            entries: vec![],
            cursor: 0,
        },
    }];
    RlabFile::new(meta, orig, copies, 0, None)
}

#[test]
fn v4_roundtrip_clean() {
    let rlab = make_rlab();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    // Must read back as v4
    let loaded = RlabFile::read(tmp.path()).unwrap();
    assert_eq!(loaded.format_version, FORMAT_VERSION_V4);
    assert_eq!(loaded.original_bytes, rlab.original_bytes);

    // Verify should report clean
    let report = verify_and_repair(tmp.path(), None).unwrap();
    assert!(report.file_hash_ok);
    assert!(report.damaged_chunks.is_empty());
    assert!(report.recc_present);
    assert!(!report.repaired);
}

#[test]
fn v4_detect_corruption() {
    let rlab = make_rlab();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    // Flip bytes known to be inside ORIG chunk data (well past any header).
    let mut bytes = std::fs::read(tmp.path()).unwrap();
    bytes[5000] ^= 0xFF;
    bytes[5001] ^= 0xFF;
    std::fs::write(tmp.path(), &bytes).unwrap();

    // Verify should report damage (file hash fails, chunk hash fails)
    let report = verify_and_repair(tmp.path(), None).unwrap();
    assert!(!report.file_hash_ok);
    assert!(!report.damaged_chunks.is_empty());
    assert!(report.recc_present);
    assert!(!report.repaired);
}

#[test]
fn v4_repair_corruption() {
    let rlab = make_rlab();
    let orig_tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(orig_tmp.path()).unwrap();

    // Corrupt 16 bytes inside ORIG chunk data (well past any header).
    let mut bytes = std::fs::read(orig_tmp.path()).unwrap();
    for b in bytes[5000..5016].iter_mut() {
        *b ^= 0xAB;
    }
    std::fs::write(orig_tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(report.recc_present);
    assert!(report.repaired, "repair should succeed: {:?}", report);

    // Repaired file must load cleanly and contain the original data.
    let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
    assert_eq!(loaded.original_bytes, rlab.original_bytes);
}

// ── Multi-shard tests ─────────────────────────────────────────────────────────
//
// The small fixture above produces only 8 data shards / 1 parity shard, so it
// can only exercise single-shard damage. These tests use a larger ORIG payload
// to force parity_shards == 2, which is what makes "at the parity limit" and
// "beyond the parity budget" distinguishable.

const SHARD_SIZE: usize = 4096;

/// 100 KiB ORIG → 26 data shards, `parity_shards = 26 / 10 = 2`.
/// Uses a non-constant pattern so an accidental shard-shuffle in the repair
/// path would break the round-trip equality check.
fn make_rlab_large() -> RlabFile {
    let meta = RlabMeta::new("0.2.0", Some("test.jpg"), 100, 100);
    let orig: Vec<u8> = (0..100 * 1024).map(|i| (i % 251) as u8).collect();
    let copies = vec![SavedCopy {
        name: "Copy 1".into(),
        pipeline_state: PipelineState {
            entries: vec![],
            cursor: 0,
        },
    }];
    RlabFile::new(meta, orig, copies, 0, None)
}

fn flip_16_bytes(bytes: &mut [u8], offset: usize) {
    for b in bytes[offset..offset + 16].iter_mut() {
        *b ^= 0xAB;
    }
}

// Offsets land inside distinct data shards and inside ORIG data (ORIG begins
// well before 4 KiB since META is small).
const SHARD1_OFFSET: usize = SHARD_SIZE + 500; //  4_596 — shard 1
const SHARD3_OFFSET: usize = 3 * SHARD_SIZE + 500; // 12_788 — shard 3
const SHARD7_OFFSET: usize = 7 * SHARD_SIZE + 500; // 29_172 — shard 7

#[test]
fn v4_repair_multi_shard_at_parity_limit() {
    let rlab = make_rlab_large();
    let orig_tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(orig_tmp.path()).unwrap();

    // Damage two distinct data shards; with parity_shards == 2 this sits
    // exactly at the correction budget.
    let mut bytes = std::fs::read(orig_tmp.path()).unwrap();
    flip_16_bytes(&mut bytes, SHARD1_OFFSET);
    flip_16_bytes(&mut bytes, SHARD3_OFFSET);
    std::fs::write(orig_tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(!report.file_hash_ok);
    assert!(report.recc_present);
    assert!(
        report.repaired,
        "damage across two shards must be repairable: {report:?}"
    );

    let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
    assert_eq!(loaded.original_bytes, rlab.original_bytes);
}

#[test]
fn v4_repair_beyond_parity_budget_reports_unrepairable() {
    let rlab = make_rlab_large();
    let orig_tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(orig_tmp.path()).unwrap();

    // Damage three distinct data shards; with parity_shards == 2 this exceeds
    // the correction budget and must be rejected (no panic, no false success).
    let mut bytes = std::fs::read(orig_tmp.path()).unwrap();
    flip_16_bytes(&mut bytes, SHARD1_OFFSET);
    flip_16_bytes(&mut bytes, SHARD3_OFFSET);
    flip_16_bytes(&mut bytes, SHARD7_OFFSET);
    std::fs::write(orig_tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(!report.file_hash_ok);
    assert!(report.recc_present);
    assert!(!report.damaged_chunks.is_empty());
    assert!(
        !report.repaired,
        "three-shard damage must exceed the 2-parity budget: {report:?}"
    );
}

#[test]
fn v4_repaired_file_reverifies_clean() {
    let rlab = make_rlab_large();
    let orig_tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(orig_tmp.path()).unwrap();

    let mut bytes = std::fs::read(orig_tmp.path()).unwrap();
    flip_16_bytes(&mut bytes, SHARD1_OFFSET);
    flip_16_bytes(&mut bytes, SHARD3_OFFSET);
    std::fs::write(orig_tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let first = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(first.repaired);

    // The repaired file must report fully clean on a fresh verify, including
    // a valid file-level hash and intact RECC chunk.
    let second = verify_and_repair(repaired_tmp.path(), None).unwrap();
    assert!(second.file_hash_ok);
    assert!(second.damaged_chunks.is_empty());
    assert!(second.recc_present);
    assert!(!second.repaired);
}

// ── RECC redundancy tests ─────────────────────────────────────────────────────
//
// v4 files store the parity chunk twice back-to-back. These tests exercise the
// redundancy: damage to one parity copy must be detected, survivable, and
// healed on repair. Damage to *both* copies leaves the file unrepairable.

/// Return the byte offsets of every `"RECC"` tag in the file.
/// Safe because the small fixture's ORIG data is all `0x55` and neither META
/// nor VCPS JSON contains the four-byte sequence `RECC`.
fn find_recc_tag_offsets(bytes: &[u8]) -> Vec<usize> {
    (0..bytes.len().saturating_sub(4))
        .filter(|&i| &bytes[i..i + 4] == b"RECC")
        .collect()
}

/// Flip one byte inside the parity data of the RECC copy at `tag_offset`.
/// The copy's chunk hash will no longer match, but the payload is recoverable
/// from the other copy.
fn flip_inside_recc_copy(bytes: &mut [u8], tag_offset: usize) {
    // Layout: [tag 4][len 8][data N][hash 32]. Flip somewhere inside `data`.
    // Offset +64 lands past the 20-byte fixed header and inside the per-shard
    // hash table / parity region of any non-trivial RECC payload.
    bytes[tag_offset + 12 + 64] ^= 0xAB;
}

#[test]
fn v4_writes_two_recc_copies() {
    let rlab = make_rlab();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let bytes = std::fs::read(tmp.path()).unwrap();
    let offsets = find_recc_tag_offsets(&bytes);
    assert_eq!(
        offsets.len(),
        2,
        "v4 writer must emit two identical RECC chunks; got offsets {offsets:?}"
    );
}

#[test]
fn v4_survives_damage_to_first_recc_copy() {
    let rlab = make_rlab();
    let orig_tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(orig_tmp.path()).unwrap();

    let mut bytes = std::fs::read(orig_tmp.path()).unwrap();
    let recc_offsets = find_recc_tag_offsets(&bytes);
    flip_inside_recc_copy(&mut bytes, recc_offsets[0]);
    std::fs::write(orig_tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(!report.file_hash_ok);
    assert!(report.recc_present);
    assert!(
        report.damaged_chunks.iter().any(|t| t == "RECC"),
        "damaged RECC copy should be surfaced in report: {report:?}"
    );
    assert!(report.repaired, "second RECC copy should heal the first");

    // Repaired file: both copies intact, re-verifies clean.
    let clean = verify_and_repair(repaired_tmp.path(), None).unwrap();
    assert!(clean.file_hash_ok);
    assert!(clean.damaged_chunks.is_empty());

    let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
    assert_eq!(loaded.original_bytes, rlab.original_bytes);
}

#[test]
fn v4_survives_damage_to_second_recc_copy() {
    let rlab = make_rlab();
    let orig_tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(orig_tmp.path()).unwrap();

    let mut bytes = std::fs::read(orig_tmp.path()).unwrap();
    let recc_offsets = find_recc_tag_offsets(&bytes);
    flip_inside_recc_copy(&mut bytes, recc_offsets[1]);
    std::fs::write(orig_tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(report.repaired, "first RECC copy should heal the second");

    let clean = verify_and_repair(repaired_tmp.path(), None).unwrap();
    assert!(clean.file_hash_ok);
    assert!(clean.damaged_chunks.is_empty());
}

#[test]
fn v4_redundancy_pays_off_when_data_and_first_recc_copy_are_both_damaged() {
    // This is the scenario redundancy exists for: the protected region needs
    // RS reconstruction, AND the first RECC copy is corrupt. Without a second
    // copy, repair would fail. With two copies, the second payload is used.
    let rlab = make_rlab();
    let orig_tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(orig_tmp.path()).unwrap();

    let mut bytes = std::fs::read(orig_tmp.path()).unwrap();

    // Damage protected region (ORIG data shard).
    for b in bytes[5000..5016].iter_mut() {
        *b ^= 0xAB;
    }
    // Damage the first RECC copy.
    let recc_offsets = find_recc_tag_offsets(&bytes);
    flip_inside_recc_copy(&mut bytes, recc_offsets[0]);
    std::fs::write(orig_tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(
        report.repaired,
        "second RECC copy must enable repair: {report:?}"
    );

    let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
    assert_eq!(loaded.original_bytes, rlab.original_bytes);
}

#[test]
fn v4_unrepairable_when_both_recc_copies_damaged() {
    let rlab = make_rlab();
    let orig_tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(orig_tmp.path()).unwrap();

    let mut bytes = std::fs::read(orig_tmp.path()).unwrap();
    let recc_offsets = find_recc_tag_offsets(&bytes);
    flip_inside_recc_copy(&mut bytes, recc_offsets[0]);
    flip_inside_recc_copy(&mut bytes, recc_offsets[1]);
    std::fs::write(orig_tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(!report.file_hash_ok);
    assert!(report.recc_present);
    assert!(
        !report.repaired,
        "both RECC copies damaged: must report unrepairable, not panic"
    );
}

// ── Adaptive parity ratio (small vs. large files) ─────────────────────────────
//
// For small files the RECC layout uses 4 KiB shards with ~10 % parity. Once a
// file is large enough that shards must grow past 4 KiB, the writer switches
// to a ~20 % parity budget so the correction capacity per byte doesn't
// collapse. These tests pin both regimes.

/// Parse the RECC payload header from a `.rlab` file on disk.
/// Returns `(shard_size, data_shards, parity_shards)`.
fn read_recc_header(bytes: &[u8]) -> (usize, usize, usize) {
    let tag_off = find_recc_tag_offsets(bytes)[0];
    // Chunk layout: [tag 4][len 8][payload N][hash 32]. Payload starts at +12.
    let p = tag_off + 12;
    let shard_size = u32::from_le_bytes(bytes[p..p + 4].try_into().unwrap()) as usize;
    let data_shards = u32::from_le_bytes(bytes[p + 4..p + 8].try_into().unwrap()) as usize;
    let parity_shards = u32::from_le_bytes(bytes[p + 8..p + 12].try_into().unwrap()) as usize;
    (shard_size, data_shards, parity_shards)
}

fn make_rlab_with_orig(size: usize) -> RlabFile {
    let meta = RlabMeta::new("0.2.0", Some("test.jpg"), 100, 100);
    // 0x55 never forms the byte sequence "RECC" (0x52 0x45 0x43 0x43),
    // so find_recc_tag_offsets stays reliable on large fixtures.
    let orig = vec![0x55u8; size];
    let copies = vec![SavedCopy {
        name: "Copy 1".into(),
        pipeline_state: PipelineState {
            entries: vec![],
            cursor: 0,
        },
    }];
    RlabFile::new(meta, orig, copies, 0, None)
}

#[test]
fn small_file_keeps_10_percent_parity() {
    // 800 KiB ORIG stays well inside 230 × 4 KiB, so the writer should use the
    // small-file path: 4 KiB shards, parity ≈ 10 %.
    let rlab = make_rlab_with_orig(800 * 1024);
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let bytes = std::fs::read(tmp.path()).unwrap();
    let (shard_size, data_shards, parity_shards) = read_recc_header(&bytes);

    assert_eq!(shard_size, 4096, "small files must keep minimum shard size");
    assert!(data_shards <= 230);
    // Matches the old `(data_shards / 10).max(1)` rule exactly.
    let expected = (data_shards / 10).max(1);
    assert_eq!(
        parity_shards, expected,
        "small-file 10% ratio must be preserved"
    );
}

#[test]
fn large_file_uses_larger_parity_budget() {
    // 1 MiB ORIG forces shard_size > 4 KiB, triggering the large-file path.
    // The new writer aims for ~20 % parity (up from 10 %).
    let rlab = make_rlab_with_orig(1024 * 1024);
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let bytes = std::fs::read(tmp.path()).unwrap();
    let (shard_size, data_shards, parity_shards) = read_recc_header(&bytes);

    assert!(
        shard_size > 4096,
        "large-file path should grow shards past the minimum; got {shard_size}"
    );
    assert!(
        data_shards + parity_shards <= 255,
        "must respect GF(2^8) limit"
    );

    // Old rule would produce parity = data_shards / 10. Require comfortably
    // more than that — at least 18 %, heading toward the 20 % target.
    let old_budget = (data_shards / 10).max(1);
    assert!(
        parity_shards > old_budget,
        "large-file parity ({parity_shards}) must exceed old 10 % budget ({old_budget})"
    );
    assert!(
        parity_shards * 100 >= data_shards * 18,
        "large-file parity ratio should be ≥ 18 %: {parity_shards}/{data_shards}"
    );
}

#[test]
fn large_file_survives_damage_beyond_old_parity_budget() {
    // With the old 10 % budget a 1 MiB file could tolerate ≈ 12 corrupted
    // shards. The new curve must handle more than that without falling back
    // to "unrepairable".
    let rlab = make_rlab_with_orig(1024 * 1024);
    let orig_tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(orig_tmp.path()).unwrap();

    let mut bytes = std::fs::read(orig_tmp.path()).unwrap();
    let (shard_size, _data_shards, parity_shards) = read_recc_header(&bytes);
    assert!(
        parity_shards >= 15,
        "fixture assumption: large path should yield ≥ 15 parity shards, got {parity_shards}"
    );

    // Corrupt 15 distinct data shards. Shards 1..=15 land inside ORIG payload
    // (fill byte 0x55), so chunk-header walking stays intact and the damage
    // only manifests as 15 erased data shards — which the new parity budget
    // (≥ 15) can cover. Shard 0 is skipped to preserve MAGIC and the META /
    // VCPS chunk headers that scan_chunks relies on to find RECC at all.
    for i in 1..=15 {
        let off = i * shard_size + 100;
        bytes[off] ^= 0xAB;
    }
    std::fs::write(orig_tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(!report.file_hash_ok);
    assert!(report.recc_present);
    assert!(
        report.repaired,
        "new parity budget ({parity_shards}) must cover 15 shard erasures: {report:?}"
    );

    let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
    assert_eq!(loaded.original_bytes, rlab.original_bytes);
}

#[test]
fn v3_still_reads() {
    let rlab = make_rlab();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write(tmp.path()).unwrap(); // v3

    let loaded = RlabFile::read(tmp.path()).unwrap();
    assert_eq!(loaded.format_version, 3);
    assert_eq!(loaded.original_bytes, rlab.original_bytes);

    // Verify on a clean v3 file: no RECC chunk.
    let report = verify_and_repair(tmp.path(), None).unwrap();
    assert!(report.file_hash_ok);
    assert!(!report.recc_present);
}
