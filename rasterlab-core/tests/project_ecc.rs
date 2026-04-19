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
