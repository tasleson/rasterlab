//! `compare` over two libraries built from the same photographs.
//!
//! The point of the command is that two libraries filled the same way match
//! even though every id, uuid and timestamp in them differs, so most of what
//! these tests do is fill two libraries and insist on silence — then change one
//! thing and insist that exactly that thing is reported.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};

use rasterlab_library::{
    CompareOptions, CompareOutcome, Difference, Library, Scope,
    compare::compare,
    db_trait::{PhotoRow, SortOrder},
};

fn test_images_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test_images")
}

fn sources() -> Vec<PathBuf> {
    let dir = test_images_dir();
    vec![dir.join("meta_test.jpg"), dir.join("color_patches.png")]
}

/// Fill a library at `root` with the same two photographs, and let `after`
/// change it before the handle is dropped — the index holds an exclusive lock
/// while it is open, and `compare` needs to take that lock itself.
fn library_with_sources(root: &Path, after: impl FnOnce(&Library)) {
    let library = Library::open_or_create(root).expect("open_or_create");
    library.import_files(&sources(), |_| {}).expect("import");
    after(&library);
}

fn compared(left: &Path, right: &Path, options: CompareOptions) -> CompareOutcome {
    compare(
        left,
        right,
        options,
        Arc::new(AtomicBool::new(false)),
        &|_| {},
    )
    .expect("compare")
}

/// Every difference, as `subject: detail`, for assertions that care what was
/// actually said rather than only how many lines there were.
fn lines(outcome: &CompareOutcome) -> Vec<String> {
    outcome
        .differences
        .iter()
        .map(|d| format!("{}: {}", d.subject, d.detail))
        .collect()
}

fn photo(library: &Library, filename: &str) -> PhotoRow {
    library
        .all_photos(SortOrder::default())
        .expect("all_photos")
        .into_iter()
        .find(|row| row.original_filename.as_deref() == Some(filename))
        .unwrap_or_else(|| panic!("{filename} was not imported"))
}

fn only(outcome: &CompareOutcome) -> &Difference {
    assert_eq!(
        outcome.differences.len(),
        1,
        "expected one difference, got {:?}",
        lines(outcome)
    );
    &outcome.differences[0]
}

/// Two libraries filled from the same files match, despite sharing no id,
/// uuid, or import timestamp.
#[test]
fn the_same_import_twice_compares_equal() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    library_with_sources(left.path(), |_| {});
    library_with_sources(right.path(), |_| {});

    let outcome = compared(left.path(), right.path(), CompareOptions::default());
    assert!(
        outcome.is_match(),
        "libraries should match: {:?}",
        lines(&outcome)
    );
    assert_eq!(outcome.photos_left, 2);
    assert_eq!(outcome.photos_right, 2);
}

/// A photo in one library and not the other is named once, not reported field
/// by field.
#[test]
fn a_photo_only_one_library_has_is_reported_as_such() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    library_with_sources(left.path(), |_| {});
    let library = Library::open_or_create(right.path()).unwrap();
    library.import_files(&sources()[..1], |_| {}).unwrap();
    drop(library);

    let outcome = compared(left.path(), right.path(), CompareOptions::default());
    assert!(!outcome.is_match());
    let photos: Vec<&Difference> = outcome
        .differences
        .iter()
        .filter(|d| d.scope == Scope::Photo)
        .collect();
    assert_eq!(photos.len(), 1, "{:?}", lines(&outcome));
    assert!(
        photos[0].subject.contains("color_patches.png"),
        "{:?}",
        photos[0]
    );
    assert_eq!(photos[0].detail, "only in left");
    // The session it was imported into is short a photo, and says which.
    let sessions: Vec<&Difference> = outcome
        .differences
        .iter()
        .filter(|d| d.scope == Scope::Session)
        .collect();
    assert_eq!(sessions.len(), 1, "{:?}", lines(&outcome));
    assert!(
        sessions[0]
            .detail
            .starts_with("1 photo only in left: color_patches.png"),
        "{:?}",
        sessions[0]
    );
}

/// Metadata lives in the `.rlab` rather than in any column the index shows, so
/// this is the difference `--index-only` is not meant to see.
#[test]
fn a_changed_rating_is_reported_only_by_the_full_comparison() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    library_with_sources(left.path(), |_| {});
    library_with_sources(right.path(), |library| {
        let row = photo(library, "meta_test.jpg");
        let mut lmta = rasterlab_core::project::RlabFile::read(&library.rlab_path(&row.hash))
            .unwrap()
            .lmta
            .unwrap();
        lmta.rating = 4;
        library.update_metadata(row.id, lmta).unwrap();
    });

    let outcome = compared(left.path(), right.path(), CompareOptions::default());
    let difference = only(&outcome);
    assert!(
        difference.subject.contains("meta_test.jpg"),
        "{difference:?}"
    );
    assert_eq!(
        difference.detail, "file.lmta.rating: 0 (left) vs 4 (right)",
        "{difference:?}"
    );

    let index_only = compared(
        left.path(),
        right.path(),
        CompareOptions { index_only: true },
    );
    assert!(
        index_only.is_match(),
        "the index knows nothing about ratings: {:?}",
        lines(&index_only)
    );
}

/// Collections are compared by name and membership, because the uuid they are
/// really keyed by is minted per run.
#[test]
fn collections_are_compared_by_name_and_membership() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let file_one_photo = |library: &Library, name: &str| {
        let collection = library.create_collection(name).unwrap();
        let row = photo(library, "meta_test.jpg");
        library.add_to_collection(collection.id, &[row.id]).unwrap();
    };
    library_with_sources(left.path(), |library| file_one_photo(library, "Iceland"));
    library_with_sources(right.path(), |library| file_one_photo(library, "Iceland"));

    let outcome = compared(left.path(), right.path(), CompareOptions::default());
    assert!(
        outcome.is_match(),
        "two libraries with the same collection should match: {:?}",
        lines(&outcome)
    );

    // Same photos, different collection: two one-sided reports rather than a
    // membership difference, since neither name is in both libraries — and the
    // photo itself disagrees in both stores.
    let renamed = tempfile::tempdir().unwrap();
    library_with_sources(renamed.path(), |library| file_one_photo(library, "Norway"));
    let outcome = compared(left.path(), renamed.path(), CompareOptions::default());
    let mut reported = lines(&outcome);
    reported.sort();
    assert_eq!(reported.len(), 4, "{reported:?}");
    assert_eq!(reported[0], "collection \"Iceland\": only in left");
    assert_eq!(reported[1], "collection \"Norway\": only in right");
    for (line, field) in reported[2..]
        .iter()
        .zip(["file.lmta.collection_names", "index.collections"])
    {
        assert!(line.contains("photo meta_test.jpg"), "{line}");
        assert!(
            line.ends_with(&format!("{field}: Iceland (left) vs Norway (right)")),
            "{line}"
        );
    }
}

/// Membership differences within one collection name the photos that moved.
#[test]
fn a_photo_missing_from_a_collection_is_named() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    let file = |library: &Library, names: &[&str]| {
        let collection = library.create_collection("Shoot").unwrap();
        let ids: Vec<_> = names.iter().map(|name| photo(library, name).id).collect();
        library.add_to_collection(collection.id, &ids).unwrap();
    };
    library_with_sources(left.path(), |library| {
        file(library, &["meta_test.jpg", "color_patches.png"])
    });
    library_with_sources(right.path(), |library| file(library, &["meta_test.jpg"]));

    let outcome = compared(left.path(), right.path(), CompareOptions::default());
    let collection: Vec<&Difference> = outcome
        .differences
        .iter()
        .filter(|d| d.scope == Scope::Collection)
        .collect();
    assert_eq!(collection.len(), 1, "{:?}", lines(&outcome));
    assert!(
        collection[0]
            .detail
            .starts_with("1 photo only in left: color_patches.png"),
        "{:?}",
        collection[0]
    );
}

/// A `.rlab` on disk that no row mentions — what an import killed between the
/// file write and the index write leaves — is a difference the index alone
/// cannot see.
#[test]
fn a_file_the_index_never_learned_about_is_reported() {
    let left = tempfile::tempdir().unwrap();
    let right = tempfile::tempdir().unwrap();
    library_with_sources(left.path(), |_| {});
    library_with_sources(right.path(), |_| {});

    // Copy one photo's file into the right library under a hash it has no row
    // for, the way a half-finished import would leave it.
    let library = Library::open_existing(right.path()).unwrap();
    let row = photo(&library, "meta_test.jpg");
    let source = library.rlab_path(&row.hash);
    let stray = format!("{}f", &row.hash[..row.hash.len() - 1]);
    let destination = library.rlab_path(&stray);
    drop(library);
    std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
    std::fs::copy(&source, &destination).unwrap();

    let outcome = compared(left.path(), right.path(), CompareOptions::default());
    let difference = only(&outcome);
    assert_eq!(difference.detail, "only in right");
}
