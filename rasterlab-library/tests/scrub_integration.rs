use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

use rasterlab_core::project::{RlabFile, verify_and_repair};
use rasterlab_library::{Library, ScrubOutcome, db_trait::SortOrder, import::relative_lib_path};

fn jpeg_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test_images")
        .join("meta_test.jpg")
}

fn import_one(lib: &Library) -> (String, PathBuf) {
    let session = lib.import_files(&[jpeg_path()], |_| {}).expect("import");
    assert!(
        session.errors.is_empty(),
        "import errors: {:?}",
        session.errors
    );
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let hash = photos[0].hash.clone();
    let path = lib.rlab_path(&hash);
    (hash, path)
}

fn run_scrub(lib: &Library) -> ScrubOutcome {
    lib.scrub(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("scrub")
}

fn flip(bytes: &mut [u8], range: std::ops::Range<usize>, mask: u8) {
    for b in bytes[range].iter_mut() {
        *b ^= mask;
    }
}

#[test]
fn scrub_clean_v4_library_makes_no_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::open_or_create(tmp.path()).unwrap();
    import_one(&lib);

    let out = run_scrub(&lib);
    assert_eq!(out.checked, 1);
    assert_eq!(out.repaired, 0);
    // Imports are already written as v4, so nothing to upgrade.
    assert_eq!(out.upgraded, 0);
    assert!(out.errors.is_empty(), "{out:?}");
    assert!(!out.cancelled);
    assert!(!tmp.path().join("recovered").exists());
}

#[test]
fn scrub_repairs_corruption_and_backs_up_original() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::open_or_create(tmp.path()).unwrap();
    let (hash, path) = import_one(&lib);

    let original = RlabFile::read(&path).unwrap().original_bytes;

    // Corrupt 16 bytes inside the protected region (a single 4 KiB shard),
    // which sits within the parity budget and is therefore correctable.
    let mut corrupted = std::fs::read(&path).unwrap();
    flip(&mut corrupted, 200..216, 0xAB);
    std::fs::write(&path, &corrupted).unwrap();
    assert!(!verify_and_repair(&path, None).unwrap().file_hash_ok);

    let out = run_scrub(&lib);
    assert_eq!(out.repaired, 1, "{out:?}");
    assert!(out.errors.is_empty(), "{out:?}");

    // The repaired file loads cleanly and round-trips the original bytes.
    let post = verify_and_repair(&path, None).unwrap();
    assert!(
        post.file_hash_ok && post.damaged_chunks.is_empty(),
        "{post:?}"
    );
    assert_eq!(RlabFile::read(&path).unwrap().original_bytes, original);

    // The corrupted original was backed up verbatim under recovered/.
    let backup = tmp.path().join("recovered").join(relative_lib_path(&hash));
    assert!(backup.exists(), "backup missing at {}", backup.display());
    assert_eq!(std::fs::read(&backup).unwrap(), corrupted);
}

#[test]
fn scrub_upgrades_v3_file_to_v4() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::open_or_create(tmp.path()).unwrap();
    let (_hash, path) = import_one(&lib);

    // Rewrite the file in the older v3 format (no RECC parity).
    RlabFile::read(&path).unwrap().write(&path).unwrap();
    let pre = verify_and_repair(&path, None).unwrap();
    assert!(pre.file_hash_ok && !pre.recc_present, "{pre:?}");

    let out = run_scrub(&lib);
    assert_eq!(out.upgraded, 1, "{out:?}");
    assert_eq!(out.repaired, 0);
    assert!(out.errors.is_empty(), "{out:?}");

    // It now carries parity, and a clean upgrade leaves no backup behind.
    let post = verify_and_repair(&path, None).unwrap();
    assert!(post.file_hash_ok && post.recc_present, "{post:?}");
    assert!(!tmp.path().join("recovered").exists());
}

#[test]
fn scrub_reports_uncorrectable_corruption() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::open_or_create(tmp.path()).unwrap();
    let (_hash, path) = import_one(&lib);

    // Destroy the bulk of the file — including both RECC copies — so repair is
    // impossible. The file hash trailer is left intact only to be safe.
    let mut corrupted = std::fs::read(&path).unwrap();
    let end = corrupted.len() - 40;
    flip(&mut corrupted, 100..end, 0xFF);
    std::fs::write(&path, &corrupted).unwrap();

    let out = run_scrub(&lib);
    assert_eq!(out.repaired, 0, "{out:?}");
    assert_eq!(out.errors.len(), 1, "{out:?}");
    assert_eq!(out.errors[0].0, path);
    // An uncorrectable file is left untouched (not replaced, not backed up).
    assert_eq!(std::fs::read(&path).unwrap(), corrupted);
    assert!(!tmp.path().join("recovered").exists());
}

#[test]
fn scrub_honours_cancellation() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = Library::open_or_create(tmp.path()).unwrap();
    import_one(&lib);

    // A flag that is already set stops the scrub before the first file.
    let out = lib
        .scrub(Arc::new(AtomicBool::new(true)), |_| {})
        .expect("scrub");
    assert!(out.cancelled);
    assert_eq!(out.checked, 0);
}
