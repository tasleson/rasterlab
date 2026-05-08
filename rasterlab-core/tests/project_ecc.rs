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

// ── Error-walk and escalation tests ──────────────────────────────────────────
//
// These tests fill the gaps that hardcoded-offset tests leave open:
//
//   • Every data shard (1 .. data_shards-1) is independently detectable and
//     repairable — no "dark spots" in RECC coverage.
//   • Escalating damage from 1 shard to parity_shards all repair; one shard
//     beyond that fails gracefully.
//   • Sub-shard error size is irrelevant: 1 byte, 512 bytes, and 4 KiB − 1
//     byte of damage in the same shard produce identical outcomes (per-shard
//     Blake3 detection is binary).
//   • A burst straddling a shard boundary creates two erasures and requires
//     two parity shards.
//   • Non-ORIG chunks (META) in the protected region are detectable and
//     repairable.
//   • detect-only mode (repair_to = None) must not modify the source file.
//
// NOTE — shard 0 is skipped in the sweep.  Shard 0 contains the file magic
// and chunk tag/length fields that scan_chunks needs to bootstrap RECC
// discovery.  Corruption hitting those fields prevents the repair from
// locating RECC at all, so recovery is not guaranteed.  Corruption in chunk
// DATA within shard 0 (e.g. the META payload) is still recoverable — the
// chunk walker only needs the tag/length fields to navigate.  That scenario
// is covered explicitly by meta_chunk_corruption_detected_and_repaired.

/// Walk a single-byte corruption through the midpoint of every data shard
/// (shards 1 .. data_shards − 1).  Each position must be independently
/// detected and repaired within the parity budget.
#[test]
fn shard_sweep_every_position_detectable_and_repairable() {
    // 100 KiB → 26 data shards, 2 parity. Any single-shard erasure is within budget.
    let rlab = make_rlab_large();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let clean = std::fs::read(tmp.path()).unwrap();
    let (shard_size, data_shards, _parity) = read_recc_header(&clean);

    for shard_idx in 1..data_shards {
        let mut corrupt = clean.clone();
        corrupt[shard_idx * shard_size + shard_size / 2] ^= 0xFF;

        let orig_tmp = NamedTempFile::new().unwrap();
        std::fs::write(orig_tmp.path(), &corrupt).unwrap();

        let repaired_tmp = NamedTempFile::new().unwrap();
        let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();

        assert!(
            !report.file_hash_ok,
            "shard {shard_idx}: corruption must be detected"
        );
        assert!(
            report.repaired,
            "shard {shard_idx}: single shard within 2-parity budget must be repairable; {report:?}"
        );

        let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
        assert_eq!(
            loaded.original_bytes, rlab.original_bytes,
            "shard {shard_idx}: recovered data must match original"
        );
    }
}

/// Damage 1, 2, … parity_shards distinct shards — each must repair.  One
/// shard beyond the budget must fail gracefully (no panic, no false success).
/// Uses the 100 KiB fixture (parity_shards = 2) for a predictable threshold.
#[test]
fn progressive_escalation_hits_parity_limit() {
    let rlab = make_rlab_large();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let clean = std::fs::read(tmp.path()).unwrap();
    let (shard_size, _, parity_shards) = read_recc_header(&clean);
    assert!(
        parity_shards >= 2,
        "fixture assumption: 100 KiB → ≥ 2 parity shards; got {parity_shards}"
    );

    // 1 .. parity_shards damaged shards: every count must repair.
    for n in 1..=parity_shards {
        let mut corrupt = clean.clone();
        for i in 0..n {
            corrupt[(i + 1) * shard_size + 100] ^= 0xFF; // shards 1, 2, …
        }
        let orig_tmp = NamedTempFile::new().unwrap();
        std::fs::write(orig_tmp.path(), &corrupt).unwrap();

        let repaired_tmp = NamedTempFile::new().unwrap();
        let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
        assert!(
            report.repaired,
            "{n} damaged shard(s) ≤ {parity_shards} parity: must repair; {report:?}"
        );
        let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
        assert_eq!(
            loaded.original_bytes, rlab.original_bytes,
            "{n} shards: recovered data must match original"
        );
    }

    // parity_shards + 1 shards damaged: must fail gracefully.
    let n_over = parity_shards + 1;
    {
        let mut corrupt = clean.clone();
        for i in 0..n_over {
            corrupt[(i + 1) * shard_size + 100] ^= 0xFF;
        }
        let orig_tmp = NamedTempFile::new().unwrap();
        std::fs::write(orig_tmp.path(), &corrupt).unwrap();

        let repaired_tmp = NamedTempFile::new().unwrap();
        let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
        assert!(
            !report.repaired,
            "{n_over} shards damaged (> {parity_shards} parity): must be unrepairable; {report:?}"
        );
        assert!(!report.file_hash_ok);
    }
}

/// Corruption size within a single shard does not affect recovery.  Blake3
/// per-shard detection is binary: any sub-shard damage erases the whole shard
/// and triggers the same RS reconstruction path regardless of damage extent.
#[test]
fn intra_shard_error_size_does_not_affect_recovery() {
    let rlab = make_rlab_large();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let clean = std::fs::read(tmp.path()).unwrap();
    let (shard_size, _, _) = read_recc_header(&clean);
    let shard_base = shard_size; // shard 1

    for corrupt_len in [1_usize, 512, shard_size - 1] {
        let mut corrupt = clean.clone();
        for b in corrupt[shard_base..shard_base + corrupt_len].iter_mut() {
            *b ^= 0xFF;
        }

        let orig_tmp = NamedTempFile::new().unwrap();
        std::fs::write(orig_tmp.path(), &corrupt).unwrap();

        let repaired_tmp = NamedTempFile::new().unwrap();
        let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
        assert!(
            report.repaired,
            "{corrupt_len}-byte corruption in shard 1 must be repairable; {report:?}"
        );

        let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
        assert_eq!(
            loaded.original_bytes, rlab.original_bytes,
            "{corrupt_len}-byte corruption: data must be fully recovered"
        );
    }
}

/// A burst straddling a shard boundary creates two separate erasures.  With
/// parity_shards ≥ 2 (100 KiB) it is repairable; with parity_shards = 1
/// (32 KiB) it is not.
#[test]
fn burst_across_shard_boundary_counts_as_two_erasures() {
    // 100 KiB, 2 parity: boundary burst IS repairable.
    {
        let rlab = make_rlab_large();
        let tmp = NamedTempFile::new().unwrap();
        rlab.write_v4(tmp.path()).unwrap();

        let clean = std::fs::read(tmp.path()).unwrap();
        let (shard_size, _, parity_shards) = read_recc_header(&clean);
        assert!(parity_shards >= 2);

        // 8 bytes: last 4 in shard 1, first 4 in shard 2.
        let boundary = 2 * shard_size;
        let mut corrupt = clean.clone();
        for b in corrupt[boundary - 4..boundary + 4].iter_mut() {
            *b ^= 0xFF;
        }

        let orig_tmp = NamedTempFile::new().unwrap();
        std::fs::write(orig_tmp.path(), &corrupt).unwrap();
        let repaired_tmp = NamedTempFile::new().unwrap();
        let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
        assert!(
            report.repaired,
            "boundary burst (2 erasures, {parity_shards} parity): must repair; {report:?}"
        );
        let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
        assert_eq!(loaded.original_bytes, rlab.original_bytes);
    }

    // 32 KiB, 1 parity: boundary burst IS NOT repairable.
    {
        let rlab = make_rlab();
        let tmp = NamedTempFile::new().unwrap();
        rlab.write_v4(tmp.path()).unwrap();

        let clean = std::fs::read(tmp.path()).unwrap();
        let (shard_size, _, parity_shards) = read_recc_header(&clean);
        assert_eq!(
            parity_shards, 1,
            "32 KiB fixture should have exactly 1 parity shard"
        );

        let boundary = 2 * shard_size;
        let mut corrupt = clean.clone();
        for b in corrupt[boundary - 4..boundary + 4].iter_mut() {
            *b ^= 0xFF;
        }

        let orig_tmp = NamedTempFile::new().unwrap();
        std::fs::write(orig_tmp.path(), &corrupt).unwrap();
        let repaired_tmp = NamedTempFile::new().unwrap();
        let report = verify_and_repair(orig_tmp.path(), Some(repaired_tmp.path())).unwrap();
        assert!(
            !report.repaired,
            "boundary burst (2 erasures, {parity_shards} parity): must be unrepairable; {report:?}"
        );
    }
}

/// META-chunk payload is inside the RECC-protected region. Corruption there
/// must be detected and repaired — restoring the original metadata — even
/// though it falls in shard 0.  The chunk walker can still locate RECC
/// because chunk tag and length fields (the fields it uses to navigate) are
/// not modified.
#[test]
fn meta_chunk_corruption_detected_and_repaired() {
    let rlab = make_rlab_large();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let mut bytes = std::fs::read(tmp.path()).unwrap();
    // META is the first chunk, starting at byte 10 (after magic[8] + version[2]).
    // Data begins at byte 22 (10 + tag[4] + len[8]).  Flip two bytes 18 and 19
    // bytes into the JSON body — well past the chunk header fields.
    const META_DATA_START: usize = 10 + 4 + 8; // = 22
    bytes[META_DATA_START + 18] ^= 0xFF;
    bytes[META_DATA_START + 19] ^= 0xFF;
    std::fs::write(tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(
        report.damaged_chunks.iter().any(|t| t == "META"),
        "META corruption must be reported in damaged_chunks; {report:?}"
    );
    assert!(
        report.repaired,
        "META corruption must be repairable; {report:?}"
    );

    let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
    assert_eq!(
        loaded.meta.app_version, rlab.meta.app_version,
        "repaired META must restore original app_version"
    );
    assert_eq!(loaded.original_bytes, rlab.original_bytes);
}

/// verify_and_repair with repair_to = None must detect damage and report it
/// without writing anything or modifying the source file.
#[test]
fn detect_without_repair_leaves_file_unchanged() {
    let rlab = make_rlab_large();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let clean = std::fs::read(tmp.path()).unwrap();
    let (shard_size, _, _) = read_recc_header(&clean);

    let mut corrupt = clean.clone();
    flip_16_bytes(&mut corrupt, shard_size + 100);
    std::fs::write(tmp.path(), &corrupt).unwrap();

    let report = verify_and_repair(tmp.path(), None).unwrap();

    assert!(!report.file_hash_ok, "damage must be detected");
    assert!(report.recc_present);
    assert!(!report.repaired, "repair_to=None must not produce a repair");
    assert_eq!(
        std::fs::read(tmp.path()).unwrap(),
        corrupt,
        "source file must not be modified by detect-only verify"
    );
}

/// If a file is truncated before the RECC copies, verification must report
/// corruption without panicking or claiming a repair. There is no valid parity
/// payload left to reconstruct from.
#[test]
fn truncated_file_without_recc_reports_unrepairable() {
    let rlab = make_rlab_large();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let clean = std::fs::read(tmp.path()).unwrap();
    let first_recc = find_recc_tag_offsets(&clean)[0];
    let truncated = &clean[..first_recc];
    std::fs::write(tmp.path(), truncated).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(tmp.path(), Some(repaired_tmp.path())).unwrap();

    assert!(!report.file_hash_ok, "truncation must fail file hash");
    assert!(
        !report.recc_present,
        "truncated file has no readable RECC chunk"
    );
    assert!(
        !report.repaired,
        "truncated file without RECC must not report repaired: {report:?}"
    );
}

// ── Documented bitrot / silent-corruption patterns ───────────────────────────
//
// These mirror corruption modes discussed in storage-integrity literature:
// single-bit media decay, spatially local checksum mismatches, misdirected
// writes, lost/stale writes, and parity/checksum-region damage.

#[test]
fn documented_single_bit_flip_repairs() {
    let rlab = make_rlab_large();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let mut bytes = std::fs::read(tmp.path()).unwrap();
    let (shard_size, _, _) = read_recc_header(&bytes);
    bytes[shard_size + 123] ^= 0b0000_0001;
    std::fs::write(tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(report.repaired, "single-bit flip must repair: {report:?}");

    let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
    assert_eq!(loaded.original_bytes, rlab.original_bytes);
}

#[test]
fn documented_spatially_local_corruption_repairs_within_budget() {
    let rlab = make_rlab_large();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let mut bytes = std::fs::read(tmp.path()).unwrap();
    let (shard_size, _, parity_shards) = read_recc_header(&bytes);
    assert!(parity_shards >= 2);

    // Two nearby corruptions in adjacent shards: spatial locality consumes two
    // erasures, but remains within this fixture's parity budget.
    bytes[shard_size + shard_size - 32] ^= 0x5A;
    bytes[2 * shard_size + 32] ^= 0xA5;
    std::fs::write(tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(
        report.repaired,
        "adjacent-shard localized corruption must repair: {report:?}"
    );

    let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
    assert_eq!(loaded.original_bytes, rlab.original_bytes);
}

#[test]
fn documented_misdirected_write_like_shard_swap_repairs() {
    let rlab = make_rlab_large();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let mut bytes = std::fs::read(tmp.path()).unwrap();
    let (shard_size, _, parity_shards) = read_recc_header(&bytes);
    assert!(parity_shards >= 2);

    // Simulate data written to the wrong location by swapping two complete data
    // shards. Both locations contain valid-looking bytes, but their shard hashes
    // identify them as two erasures.
    let shard_1 = shard_size..2 * shard_size;
    let shard_3 = 3 * shard_size..4 * shard_size;
    let saved = bytes[shard_1.clone()].to_vec();
    bytes.copy_within(shard_3.clone(), shard_1.start);
    bytes[shard_3].copy_from_slice(&saved);
    std::fs::write(tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(
        report.repaired,
        "misdirected-write-like shard swap must repair: {report:?}"
    );

    let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
    assert_eq!(loaded.original_bytes, rlab.original_bytes);
}

#[test]
fn documented_lost_write_like_zeroed_shard_repairs() {
    let rlab = make_rlab_large();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let mut bytes = std::fs::read(tmp.path()).unwrap();
    let (shard_size, _, _) = read_recc_header(&bytes);
    bytes[shard_size..2 * shard_size].fill(0);
    std::fs::write(tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(
        report.repaired,
        "lost-write-like zeroed shard must repair: {report:?}"
    );

    let loaded = RlabFile::read(repaired_tmp.path()).unwrap();
    assert_eq!(loaded.original_bytes, rlab.original_bytes);
}

#[test]
fn documented_parity_region_corruption_is_healed_from_duplicate_copy() {
    let rlab = make_rlab_large();
    let tmp = NamedTempFile::new().unwrap();
    rlab.write_v4(tmp.path()).unwrap();

    let mut bytes = std::fs::read(tmp.path()).unwrap();
    let recc_offsets = find_recc_tag_offsets(&bytes);
    flip_inside_recc_copy(&mut bytes, recc_offsets[0]);
    std::fs::write(tmp.path(), &bytes).unwrap();

    let repaired_tmp = NamedTempFile::new().unwrap();
    let report = verify_and_repair(tmp.path(), Some(repaired_tmp.path())).unwrap();
    assert!(
        report.damaged_chunks.iter().any(|t| t == "RECC"),
        "parity-region corruption should be reported: {report:?}"
    );
    assert!(
        report.repaired,
        "duplicate RECC copy must heal parity-region corruption: {report:?}"
    );

    let clean = verify_and_repair(repaired_tmp.path(), None).unwrap();
    assert!(clean.file_hash_ok);
    assert!(clean.damaged_chunks.is_empty());
}
