use std::path::PathBuf;

use rasterlab_library::{
    Library,
    db_trait::{PhotoId, SortOrder},
    search::SearchFilter,
};

fn test_images_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("test_images")
}

fn jpeg_path() -> PathBuf {
    test_images_dir().join("meta_test.jpg")
}

fn png_path() -> PathBuf {
    test_images_dir().join("color_patches.png")
}

fn open_library(dir: &std::path::Path) -> Library {
    Library::open_or_create(dir).expect("open_or_create")
}

// ── Import ────────────────────────────────────────────────────────────────────

#[test]
fn import_single_jpeg_creates_rlab_and_thumb() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    let session = lib
        .import_files(&[jpeg_path()], |_| {})
        .expect("import_files");
    if !session.errors.is_empty() {
        for (path, e) in &session.errors {
            eprintln!("import error for {}: {:#}", path.display(), e);
        }
        panic!("import errors: {:?}", session.errors);
    }
    assert_eq!(
        session.photo_count, 1,
        "expected 1 imported, got {:?}",
        session
    );

    let photos = lib.all_photos(SortOrder::default()).unwrap();
    assert_eq!(photos.len(), 1);
    let row = &photos[0];

    // .rlab on disk
    assert!(lib.rlab_path(&row.hash).exists(), "rlab missing");
    // thumbnail on disk
    assert!(lib.thumb_path(&row.hash).exists(), "thumb missing");

    // DB row fields are sane
    assert!(!row.hash.is_empty());
    assert!(row.width > 0 && row.height > 0);
    assert_eq!(row.import_session, session.id);
}

#[test]
fn import_lmta_round_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let row = &photos[0];

    let rlab_path = lib.rlab_path(&row.hash);
    let rlab = rasterlab_core::project::RlabFile::read(&rlab_path).unwrap();
    let lmta = rlab.lmta.expect("LMTA chunk missing");

    // Original filename preserved
    assert_eq!(lmta.original_filename.as_deref(), Some("meta_test.jpg"));
    // Session ID round-trips
    assert_eq!(lmta.import_session_id, row.import_session);
    // EXIF snapshot present
    assert!(lmta.exif.is_some());
}

#[test]
fn duplicate_import_is_skipped() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let session2 = lib.import_files(&[jpeg_path()], |_| {}).unwrap();

    // Second import: 0 new photos, 1 skipped duplicate
    assert_eq!(session2.photo_count, 0);
    // DB still has only 1 row
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 1);
}

#[test]
fn imports_on_same_day_share_one_session() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    // Import the two files in separate batches on the same day.
    let s1 = lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let s2 = lib.import_files(&[png_path()], |_| {}).unwrap();

    assert_eq!(
        s1.id, s2.id,
        "both imports on the same day should reuse the session id"
    );
    assert_eq!(s1.name, s2.name, "session names should match");

    let sessions = lib.all_sessions().unwrap();
    assert_eq!(sessions.len(), 1, "only one session should exist");
    assert_eq!(sessions[0].photo_count, 2, "count should aggregate");
}

#[test]
fn folder_import_finds_all_supported_formats() {
    let tmp_src = tempfile::tempdir().unwrap();
    // Copy two images into a subdirectory
    let sub = tmp_src.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::copy(jpeg_path(), sub.join("a.jpg")).unwrap();
    std::fs::copy(png_path(), tmp_src.path().join("b.png")).unwrap();

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    let session = lib.import_folder(tmp_src.path(), |_| {}).unwrap();
    assert_eq!(session.photo_count, 2, "should have imported both images");
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 2);
}

// ── Delete ────────────────────────────────────────────────────────────────────

#[test]
fn delete_photo_removes_thumb_and_db_row() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let row = &photos[0];
    let thumb = lib.thumb_path(&row.hash);
    let photo_id: PhotoId = row.id;

    lib.delete_photo(photo_id).expect("delete_photo");

    assert!(!thumb.exists(), "thumbnail should be removed");
    assert!(
        lib.all_photos(SortOrder::default()).unwrap().is_empty(),
        "DB row should be gone"
    );
}

// ── Rebuild ───────────────────────────────────────────────────────────────────

#[test]
fn rebuild_index_restores_rows_after_db_delete() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 2);

    // Simulate DB loss: delete the db directory and reopen the library.
    drop(lib);
    let db_path = tmp.path().join("library.db");
    if db_path.exists() {
        if db_path.is_dir() {
            std::fs::remove_dir_all(&db_path).unwrap();
        } else {
            std::fs::remove_file(&db_path).unwrap();
        }
    }

    let lib2 = open_library(tmp.path());
    // Before rebuild the DB is empty
    assert_eq!(lib2.all_photos(SortOrder::default()).unwrap().len(), 0);

    lib2.rebuild_index(|_| {}).expect("rebuild_index");
    let photos = lib2.all_photos(SortOrder::default()).unwrap();
    assert_eq!(photos.len(), 2, "should have 2 photos after rebuild");
}

// ── Search (EXIF-based) ───────────────────────────────────────────────────────

#[test]
fn search_by_iso_excludes_no_exif_photos() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    // jpeg has ISO 400; png has no EXIF
    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();

    let filter = SearchFilter {
        iso: Some(400..=400),
        ..Default::default()
    };
    let results = lib.search(&filter, SortOrder::default()).unwrap();
    assert_eq!(results.len(), 1, "only the JPEG with ISO 400 should match");
    assert_eq!(
        results[0].original_filename.as_deref(),
        Some("meta_test.jpg")
    );
}

#[test]
fn search_by_shutter_finds_matching_photo() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    // jpeg has shutter 1/200 s (shutter_sec ≈ 0.005)
    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();

    // 1/200 = 0.005; use ±0.5% tolerance
    let eps = 0.005 * 0.005_f64;
    let filter = SearchFilter {
        shutter_min_sec: Some(0.005 - eps),
        shutter_max_sec: Some(0.005 + eps),
        ..Default::default()
    };
    let results = lib.search(&filter, SortOrder::default()).unwrap();
    assert_eq!(
        results.len(),
        1,
        "only the JPEG with 1/200 shutter should match"
    );
    assert_eq!(
        results[0].original_filename.as_deref(),
        Some("meta_test.jpg")
    );
}

// ── Search ────────────────────────────────────────────────────────────────────

#[test]
fn search_by_text_returns_matching_subset() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();

    // Set a keyword on the JPEG photo only
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let jpeg_row = photos
        .iter()
        .find(|r| r.original_filename.as_deref() == Some("meta_test.jpg"))
        .expect("jpeg photo");

    let lmta = rasterlab_library::LibraryMeta {
        keywords: vec!["searchable_kw".to_owned()],
        ..Default::default()
    };
    lib.update_metadata(jpeg_row.id, lmta).unwrap();

    let filter = SearchFilter {
        text: Some("searchable_kw".to_owned()),
        ..Default::default()
    };
    let results = lib.search(&filter, SortOrder::default()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].original_filename.as_deref(),
        Some("meta_test.jpg")
    );
}

#[test]
fn search_by_rating_min_filters_correctly() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();

    let photos = lib.all_photos(SortOrder::default()).unwrap();
    // Give JPEG a 4-star rating
    let jpeg_row = photos
        .iter()
        .find(|r| r.original_filename.as_deref() == Some("meta_test.jpg"))
        .unwrap();
    let lmta = rasterlab_library::LibraryMeta {
        rating: 4,
        ..Default::default()
    };
    lib.update_metadata(jpeg_row.id, lmta).unwrap();

    let filter = SearchFilter {
        rating_min: Some(3),
        ..Default::default()
    };
    let results = lib.search(&filter, SortOrder::default()).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0].original_filename.as_deref(),
        Some("meta_test.jpg")
    );
}

// ── Collections ───────────────────────────────────────────────────────────────

#[test]
fn create_add_rename_delete_collection() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo_id = lib.all_photos(SortOrder::default()).unwrap()[0].id;

    let coll = lib.create_collection("Favorites").unwrap();
    lib.add_to_collection(coll.id, &[photo_id]).unwrap();

    let members = lib.collection_photos(coll.id).unwrap();
    assert_eq!(members.len(), 1);

    // Rename rewrites LMTA
    lib.rename_collection(coll.id, "Best Of").unwrap();
    let rlab_path = lib.rlab_path(&members[0].hash);
    let rlab = rasterlab_core::project::RlabFile::read(&rlab_path).unwrap();
    let lmta = rlab.lmta.unwrap();
    assert!(
        lmta.collections.contains(&"Best Of".to_owned()),
        "LMTA should have new collection name"
    );
    assert!(
        !lmta.collections.contains(&"Favorites".to_owned()),
        "LMTA should not have old name"
    );

    // Delete collection — photo is unaffected
    lib.delete_collection(coll.id).unwrap();
    assert!(lib.all_collections().unwrap().is_empty());
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 1);
}

#[test]
fn remove_from_collection_updates_lmta() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo_id = lib.all_photos(SortOrder::default()).unwrap()[0].id;
    let hash = lib.all_photos(SortOrder::default()).unwrap()[0]
        .hash
        .clone();

    let coll = lib.create_collection("ToRemove").unwrap();
    lib.add_to_collection(coll.id, &[photo_id]).unwrap();
    lib.remove_from_collection(coll.id, &[photo_id]).unwrap();

    let rlab = rasterlab_core::project::RlabFile::read(&lib.rlab_path(&hash)).unwrap();
    let lmta = rlab.lmta.unwrap();
    assert!(
        !lmta.collections.contains(&"ToRemove".to_owned()),
        "collection name should be removed from LMTA"
    );
}

// ── Batch metadata ────────────────────────────────────────────────────────────

#[test]
fn batch_metadata_update_applies_to_all() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();

    let updates: Vec<(PhotoId, rasterlab_library::LibraryMeta)> = photos
        .iter()
        .map(|r| {
            let lmta = rasterlab_library::LibraryMeta {
                rating: 5,
                caption: Some("batch caption".to_owned()),
                ..Default::default()
            };
            (r.id, lmta)
        })
        .collect();

    lib.update_metadata_batch(&updates).unwrap();

    // Verify via DB search
    let filter = SearchFilter {
        rating_min: Some(5),
        ..Default::default()
    };
    let results = lib.search(&filter, SortOrder::default()).unwrap();
    assert_eq!(results.len(), 2, "both photos should have rating 5");
}
