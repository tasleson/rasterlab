//! End-to-end checks for `rasterlab library`, driving the real binary the way a
//! headless operator would: a library path on the command line, a tally on
//! stdout, and an exit status a cron job can act on.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use rasterlab_library::{Library, db_trait::SortOrder};

fn jpeg_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test_images")
        .join("meta_test.jpg")
}

/// A library with one photo in it, plus that photo's hash.
fn library_with_one_photo(root: &Path) -> String {
    let lib = Library::open_or_create(root).expect("create library");
    let session = lib.import_files(&[jpeg_path()], |_| {}).expect("import");
    assert!(session.errors.is_empty(), "import: {:?}", session.errors);
    lib.all_photos(SortOrder::default()).unwrap()[0]
        .hash
        .clone()
}

fn rasterlab(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rasterlab"))
        .args(args)
        .output()
        .expect("run rasterlab")
}

fn stdout_of(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Both commands take the library root, and neither may quietly create one at a
/// path the user got wrong — a rebuild against a fresh empty library would
/// report success having done nothing.
#[test]
fn a_path_that_is_not_a_library_is_rejected_without_creating_one() {
    for command in ["rebuild", "scrub"] {
        let tmp = tempfile::tempdir().unwrap();
        let not_a_library = tmp.path().join("photos");
        std::fs::create_dir(&not_a_library).unwrap();

        let out = rasterlab(&["library", command, not_a_library.to_str().unwrap()]);
        assert!(!out.status.success(), "{command} accepted a non-library");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("files/"), "{command}: unhelpful: {stderr}");
        assert!(
            !not_a_library.join("files").exists(),
            "{command} created a library at the rejected path"
        );

        let missing = tmp.path().join("nowhere");
        let out = rasterlab(&["library", command, missing.to_str().unwrap()]);
        assert!(!out.status.success(), "{command} accepted a missing path");
        assert!(!missing.exists(), "{command} created the missing path");
    }
}

/// The reason the command exists: an index that has lost a photo the files
/// still hold gets it back.
#[test]
fn rebuild_reindexes_a_photo_the_index_forgot() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let hash = library_with_one_photo(&root);

    // Drop the index entirely: the harshest case the rebuild is for.
    std::fs::remove_dir_all(root.join("library.db")).unwrap();

    let out = rasterlab(&["library", "rebuild", root.to_str().unwrap()]);
    assert!(out.status.success(), "rebuild failed: {out:?}");
    assert!(
        stdout_of(&out).contains("Rebuild complete: 1 of 1 indexed, 0 errors"),
        "unexpected tally: {}",
        stdout_of(&out)
    );

    let lib = Library::open_or_create(&root).unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    assert_eq!(photos.len(), 1);
    assert_eq!(photos[0].hash, hash);
}

#[test]
fn scrub_passes_a_healthy_library_and_leaves_the_index_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    library_with_one_photo(&root);

    let out = rasterlab(&["library", "scrub", root.to_str().unwrap()]);
    assert!(out.status.success(), "scrub failed: {out:?}");
    assert!(
        stdout_of(&out).contains("Scrub complete: 1 checked, 0 repaired, 0 upgraded, 0 errors"),
        "unexpected tally: {}",
        stdout_of(&out)
    );
}

/// Damage past what the parity can correct has to leave a non-zero status
/// behind, or a scheduled scrub is a scheduled way of not noticing.
#[test]
fn uncorrectable_damage_is_reported_and_exits_non_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let hash = library_with_one_photo(&root);

    let rlab = Library::open_or_create(&root).unwrap().rlab_path(&hash);
    let wrecked = vec![0x5au8; std::fs::metadata(&rlab).unwrap().len() as usize];
    std::fs::write(&rlab, wrecked).unwrap();

    let out = rasterlab(&["library", "scrub", root.to_str().unwrap()]);
    assert!(!out.status.success(), "a wrecked file passed the scrub");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&hash[..8]),
        "the failing file is not named: {stderr}"
    );
    assert!(
        stdout_of(&out).contains("1 error"),
        "unexpected tally: {}",
        stdout_of(&out)
    );
}

/// `--quiet` drops the running progress, not the answer.
#[test]
fn quiet_keeps_the_tally() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    library_with_one_photo(&root);

    for command in ["rebuild", "scrub"] {
        let out = rasterlab(&["library", command, root.to_str().unwrap(), "--quiet"]);
        assert!(out.status.success(), "{command} failed: {out:?}");
        assert!(
            stdout_of(&out).contains("0 errors"),
            "{command} lost its tally: {}",
            stdout_of(&out)
        );
    }
}
