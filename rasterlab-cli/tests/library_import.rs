//! End-to-end checks for `rasterlab library create` and `rasterlab library
//! import`, driving the real binary the way someone filling a library on a
//! headless machine would.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

use rasterlab_library::{Library, db_trait::SortOrder};

fn test_image(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test_images")
        .join(name)
}

/// A source tree of two shoot folders, one photo each.
fn source_tree(root: &Path) {
    for (folder, image) in [("shoot-a", "meta_test.jpg"), ("shoot-b", "hue_wheel.png")] {
        let dir = root.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(test_image(image), dir.join(image)).unwrap();
    }
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

fn photo_count(root: &Path) -> usize {
    Library::open_or_create(root)
        .unwrap()
        .all_photos(SortOrder::default())
        .unwrap()
        .len()
}

#[test]
fn create_lays_out_a_library_that_import_can_use() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");

    let out = rasterlab(&["library", "create", root.to_str().unwrap()]);
    assert!(out.status.success(), "create failed: {out:?}");
    assert!(root.join("files").is_dir(), "no files/ directory");

    let src = tmp.path().join("src");
    source_tree(&src);
    let out = rasterlab(&[
        "library",
        "import",
        root.to_str().unwrap(),
        src.to_str().unwrap(),
    ]);
    assert!(out.status.success(), "import failed: {out:?}");
    assert_eq!(photo_count(&root), 2);
}

/// A second `create` at the same path must not quietly succeed: the user who
/// typed it is either confused about which library they have or about where it
/// lives, and either way a fresh empty one is not the answer.
#[test]
fn create_refuses_a_path_that_is_already_taken() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    assert!(
        rasterlab(&["library", "create", root.to_str().unwrap()])
            .status
            .success()
    );

    let out = rasterlab(&["library", "create", root.to_str().unwrap()]);
    assert!(!out.status.success(), "created a library twice");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("already a library"),
        "unhelpful: {out:?}"
    );

    // A directory holding something else is just as much a mistake.
    let occupied = tmp.path().join("photos");
    std::fs::create_dir(&occupied).unwrap();
    std::fs::write(occupied.join("holiday.jpg"), b"not really").unwrap();
    let out = rasterlab(&["library", "create", occupied.to_str().unwrap()]);
    assert!(!out.status.success(), "created a library over other files");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not empty"),
        "unhelpful: {out:?}"
    );
}

/// Importing into a path that is not a library must say so rather than making
/// one, which would answer a typo with a library nobody can find again.
#[test]
fn import_into_a_non_library_asks_before_creating_one() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let src = tmp.path().join("src");
    source_tree(&src);

    let out = rasterlab(&[
        "library",
        "import",
        root.to_str().unwrap(),
        src.to_str().unwrap(),
    ]);
    assert!(!out.status.success(), "imported into a non-library");
    assert!(!root.exists(), "created a library at the rejected path");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("--create"),
        "unhelpful: {out:?}"
    );

    let out = rasterlab(&[
        "library",
        "import",
        root.to_str().unwrap(),
        src.to_str().unwrap(),
        "--create",
    ]);
    assert!(out.status.success(), "--create import failed: {out:?}");
    assert_eq!(photo_count(&root), 2);
}

/// Files and folders in one run, and the tally that reports them.
#[test]
fn a_run_takes_loose_files_alongside_folders_and_counts_them_once() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let src = tmp.path().join("src");
    source_tree(&src);
    let loose = src.join("shoot-a").join("meta_test.jpg");

    let out = rasterlab(&[
        "library",
        "import",
        root.to_str().unwrap(),
        src.to_str().unwrap(),
        loose.to_str().unwrap(),
        "--create",
    ]);
    assert!(out.status.success(), "import failed: {out:?}");
    // The loose file is inside the folder: named twice, imported once, and not
    // counted twice in the total either.
    assert!(
        stdout_of(&out).contains("2 of 2 imported"),
        "unexpected tally: {}",
        stdout_of(&out)
    );
    assert_eq!(photo_count(&root), 2);
}

/// Re-running the same import is the documented way to finish an interrupted
/// one, so it has to be a no-op that says so rather than a second copy.
#[test]
fn re_importing_skips_what_is_already_there() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let src = tmp.path().join("src");
    source_tree(&src);
    let args = [
        "library",
        "import",
        root.to_str().unwrap(),
        src.to_str().unwrap(),
        "--create",
    ];
    assert!(rasterlab(&args).status.success());

    let out = rasterlab(&args);
    assert!(out.status.success(), "re-import failed: {out:?}");
    assert!(
        stdout_of(&out).contains("0 of 2 imported, 2 already in the library"),
        "unexpected tally: {}",
        stdout_of(&out)
    );
    assert_eq!(photo_count(&root), 2);
}

#[test]
fn photos_can_be_filed_into_collections_as_they_arrive() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    source_tree(&src);

    let named = tmp.path().join("named");
    let out = rasterlab(&[
        "library",
        "import",
        named.to_str().unwrap(),
        src.to_str().unwrap(),
        "--create",
        "--collection",
        "Iceland",
    ]);
    assert!(out.status.success(), "named import failed: {out:?}");
    let lib = Library::open_or_create(&named).unwrap();
    let collections = lib.all_collections().unwrap();
    assert_eq!(
        collections
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["Iceland"]
    );
    assert_eq!(lib.collection_photos(collections[0].id).unwrap().len(), 2);
    drop(lib);

    let per_folder = tmp.path().join("per-folder");
    let out = rasterlab(&[
        "library",
        "import",
        per_folder.to_str().unwrap(),
        src.to_str().unwrap(),
        "--create",
        "--collection-per-folder",
    ]);
    assert!(out.status.success(), "per-folder import failed: {out:?}");
    let lib = Library::open_or_create(&per_folder).unwrap();
    let mut names: Vec<String> = lib
        .all_collections()
        .unwrap()
        .into_iter()
        .map(|c| c.name)
        .collect();
    names.sort();
    assert_eq!(names, ["shoot-a", "shoot-b"]);
}

/// `--quiet` drops the running progress, not the answer.
#[test]
fn quiet_keeps_the_tally() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("library");
    let src = tmp.path().join("src");
    source_tree(&src);

    let out = rasterlab(&[
        "library",
        "import",
        root.to_str().unwrap(),
        src.to_str().unwrap(),
        "--create",
        "--quiet",
    ]);
    assert!(out.status.success(), "import failed: {out:?}");
    let stdout = stdout_of(&out);
    assert!(stdout.contains("2 of 2 imported"), "no tally: {stdout}");
    assert!(!stdout.contains("scanned"), "progress survived: {stdout}");
}
