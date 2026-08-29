use std::{
    path::PathBuf,
    sync::{Arc, Barrier, atomic::AtomicBool},
};

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

/// A saved pipeline holding one real operation, for the tests about which
/// photos count as edited.  What makes a photo edited is the undo cursor, so
/// the state has to come from a pipeline that actually ran an op rather than
/// from a hand-built struct.
fn edited_pipeline_state(image_path: &std::path::Path) -> rasterlab_core::pipeline::PipelineState {
    use rasterlab_core::{formats::FormatRegistry, ops::SaturationOp, pipeline::EditPipeline};

    let bytes = std::fs::read(image_path).unwrap();
    let image = FormatRegistry::with_builtins()
        .decode_bytes(&bytes, Some(image_path))
        .unwrap();
    let mut pipeline = EditPipeline::new(image);
    pipeline.push_op(Box::new(SaturationOp::new(0.4)));
    pipeline.save_state().unwrap()
}

/// Give an already-imported photo an edited virtual copy, the way saving from
/// the editor would, and hand back its hash.
fn give_the_photo_an_edit(lib: &Library, hash: &str, source: &std::path::Path) {
    let path = lib.rlab_path(hash);
    let mut project = rasterlab_core::project::RlabFile::read(&path).unwrap();
    project.copies[0].pipeline_state = edited_pipeline_state(source);
    project.write_v5(&path).unwrap();
    lib.regenerate_thumbnail(hash).unwrap();
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
fn import_single_rlab_preserves_project_and_indexes_original_photo() {
    use rasterlab_core::{
        formats::FormatRegistry,
        ops::SaturationOp,
        pipeline::EditPipeline,
        project::{RlabFile, RlabMeta, SavedCopy},
    };

    let project_dir = tempfile::tempdir().unwrap();
    let project_path = project_dir.path().join("edited-photo.rlab");
    let original_bytes = std::fs::read(png_path()).unwrap();
    let image = FormatRegistry::with_builtins()
        .decode_bytes(&original_bytes, Some(&png_path()))
        .unwrap();
    let (width, height) = (image.width, image.height);
    let mut pipeline = EditPipeline::new(image);
    pipeline.push_op(Box::new(SaturationOp::new(0.4)));
    let saved_state = pipeline.save_state().unwrap();
    let source_path = png_path().to_string_lossy().into_owned();
    let project = RlabFile::new(
        RlabMeta::new("test", Some(source_path.clone()), width, height),
        original_bytes.clone(),
        vec![SavedCopy {
            name: "Edited copy".into(),
            pipeline_state: saved_state,
        }],
        0,
        None,
    );
    project.write_v5(&project_path).unwrap();

    let library_dir = tempfile::tempdir().unwrap();
    let lib = open_library(library_dir.path());
    let session = lib.import_files(&[project_path], |_| {}).unwrap();

    assert!(session.errors.is_empty(), "{:?}", session.errors);
    assert_eq!(session.photo_count, 1);
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    assert_eq!(photos.len(), 1);
    assert_eq!(photos[0].width, width);
    assert_eq!(photos[0].height, height);
    assert_eq!(
        photos[0].hash,
        blake3::hash(&original_bytes).to_hex().to_string()
    );

    let imported = RlabFile::read(&lib.rlab_path(&photos[0].hash)).unwrap();
    assert_eq!(imported.original_bytes, original_bytes);
    assert_eq!(imported.copies.len(), 1);
    assert_eq!(imported.copies[0].name, "Edited copy");
    assert_eq!(imported.copies[0].pipeline_state.entries.len(), 1);
    assert!(imported.thumbnail.is_some());
    let lmta = imported.lmta.unwrap();
    assert_eq!(lmta.original_filename.as_deref(), Some("color_patches.png"));
    assert_eq!(lmta.import_session_id, session.id);
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
    let source_path = jpeg_path().to_string_lossy().into_owned();

    // Original filename preserved
    assert_eq!(lmta.original_filename.as_deref(), Some("meta_test.jpg"));
    // Original source path preserved in both library and project metadata
    assert_eq!(lmta.source_path.as_deref(), Some(source_path.as_str()));
    assert_eq!(rlab.meta.source_path.as_deref(), Some(source_path.as_str()));
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

    let sessions = lib.import_folder(tmp_src.path(), |_| {}).unwrap();
    let imported: usize = sessions.iter().map(|s| s.photo_count).sum();
    assert_eq!(imported, 2, "should have imported both images");
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 2);
}

#[test]
fn folder_reimport_progress_counts_processed_duplicates() {
    let tmp_src = tempfile::tempdir().unwrap();
    std::fs::copy(jpeg_path(), tmp_src.path().join("a.jpg")).unwrap();
    std::fs::copy(png_path(), tmp_src.path().join("b.png")).unwrap();

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    lib.import_folder(tmp_src.path(), |_| {}).unwrap();

    let progress = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let progress_sink = progress.clone();
    let sessions = lib
        .import_folder(tmp_src.path(), move |p| {
            progress_sink.lock().unwrap().push(p);
        })
        .unwrap();

    let imported: usize = sessions.iter().map(|s| s.photo_count).sum();
    assert_eq!(imported, 0, "re-import should only skip duplicates");

    let progress = progress.lock().unwrap();
    let final_import = progress
        .iter()
        .rev()
        .find(|p| !p.scanning)
        .expect("final import progress");
    assert_eq!(final_import.done, 2, "processed count should advance");
    assert_eq!(final_import.imported, 0);
    assert_eq!(final_import.skipped_duplicates, 2);
}

/// Resuming an interrupted import must skip already-imported files by their
/// source fingerprint (path + size + mtime) *without* re-reading and re-hashing
/// their bytes — that is the whole point of the fast-resume path. We prove the
/// bytes are not read by overwriting the source file with different content of
/// the same length and restoring its mtime: a fingerprint-only check still skips
/// it, whereas a hash-based check would see new bytes and re-import.
#[test]
fn folder_reimport_skips_by_fingerprint_without_hashing() {
    let tmp_src = tempfile::tempdir().unwrap();
    let src = tmp_src.path().join("a.png");
    std::fs::copy(png_path(), &src).unwrap();

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    let sessions = lib.import_folder(tmp_src.path(), |_| {}).unwrap();
    assert_eq!(sessions.iter().map(|s| s.photo_count).sum::<usize>(), 1);

    // Corrupt the file's *content* while preserving its byte length and mtime,
    // so only the fingerprint — not the bytes — can match on re-import.
    let meta = std::fs::metadata(&src).unwrap();
    let mtime = filetime::FileTime::from_last_modification_time(&meta);
    std::fs::write(&src, vec![0xABu8; meta.len() as usize]).unwrap();
    filetime::set_file_mtime(&src, mtime).unwrap();

    let progress = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = progress.clone();
    let sessions = lib
        .import_folder(tmp_src.path(), move |p| sink.lock().unwrap().push(p))
        .unwrap();

    assert_eq!(
        sessions.iter().map(|s| s.photo_count).sum::<usize>(),
        0,
        "fingerprint match must skip the file without hashing its (now different) bytes"
    );
    let progress = progress.lock().unwrap();
    let final_import = progress
        .iter()
        .rev()
        .find(|p| !p.scanning)
        .expect("final import progress");
    assert_eq!(final_import.skipped_duplicates, 1);
}

// ── Grouped folder import ───────────────────────────────────────────────────

/// Write a distinct (so non-deduplicating) tiny PNG and stamp its mtime.
fn write_png_with_mtime(path: &std::path::Path, tag: u8, mtime_secs: i64) {
    let img = image::RgbImage::from_pixel(2, 2, image::Rgb([tag, tag, tag]));
    img.save(path).expect("write png");
    filetime::set_file_mtime(path, filetime::FileTime::from_unix_time(mtime_secs, 0))
        .expect("set mtime");
}

#[test]
fn folder_import_groups_by_capture_day_and_back_dates() {
    const DAY: i64 = 86_400;
    // A fixed past base so grouping is deterministic regardless of "now".
    const BASE: i64 = 1_600_000_000; // 2020-09-13 UTC

    let tmp_src = tempfile::tempdir().unwrap();
    // Shoot A: three consecutive days. Shoot B: a single day after a gap.
    write_png_with_mtime(&tmp_src.path().join("a0.png"), 1, BASE);
    write_png_with_mtime(&tmp_src.path().join("a1.png"), 2, BASE + DAY);
    write_png_with_mtime(&tmp_src.path().join("a2.png"), 3, BASE + 2 * DAY);
    write_png_with_mtime(&tmp_src.path().join("b0.png"), 4, BASE + 5 * DAY);

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    let sessions = lib.import_folder(tmp_src.path(), |_| {}).unwrap();
    assert_eq!(sessions.len(), 2, "consecutive days group; the gap splits");

    let mut by_start: Vec<_> = sessions
        .iter()
        .map(|s| (s.started_at, s.photo_count))
        .collect();
    by_start.sort();
    assert_eq!(
        by_start[0],
        (BASE as u64, 3),
        "shoot A: 3 photos, dated day 0"
    );
    assert_eq!(
        by_start[1],
        ((BASE + 5 * DAY) as u64, 1),
        "shoot B: 1 photo, dated day 5"
    );

    // Per-photo import_date is back-dated to each file's own capture time.
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    assert_eq!(photos.len(), 4);
    let mut import_dates: Vec<u64> = photos.iter().map(|p| p.import_date).collect();
    import_dates.sort();
    assert_eq!(
        import_dates,
        vec![
            BASE as u64,
            (BASE + DAY) as u64,
            (BASE + 2 * DAY) as u64,
            (BASE + 5 * DAY) as u64,
        ]
    );
}

#[test]
fn folder_import_stamps_the_wall_clock_import_time() {
    const DAY: i64 = 86_400;
    const BASE: i64 = 1_600_000_000; // 2020-09-13 UTC

    let tmp_src = tempfile::tempdir().unwrap();
    write_png_with_mtime(&tmp_src.path().join("old0.png"), 1, BASE);
    write_png_with_mtime(&tmp_src.path().join("old1.png"), 2, BASE + 5 * DAY);

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    lib.import_folder(tmp_src.path(), |_| {}).unwrap();

    let sessions = lib.all_sessions().unwrap();
    assert_eq!(sessions.len(), 2);
    for s in &sessions {
        assert!(
            s.started_at < BASE as u64 + 6 * DAY as u64,
            "session stays dated to the 2020 shoot"
        );
        let imported_at = s.imported_at.expect("import time recorded");
        assert!(
            imported_at >= before,
            "imported_at must be the wall clock, not the back-dated capture time"
        );
        assert_eq!(s.last_import_at(), imported_at);
    }
}

#[test]
fn folder_import_gives_a_heavy_day_its_own_session() {
    use rasterlab_library::import::HEAVY_DAY_PHOTOS;

    const DAY: i64 = 86_400;
    const BASE: i64 = 1_600_000_000; // 2020-09-13 UTC

    let tmp_src = tempfile::tempdir().unwrap();
    // One quiet day, a heavy shoot day, then another quiet day.  Without the
    // heavy-day rule all three would merge into a single consecutive-day run.
    let mut tag = 0u8;
    let mut write = |name: String, mtime: i64| {
        tag += 1;
        write_png_with_mtime(&tmp_src.path().join(name), tag, mtime);
    };
    write("quiet0.png".into(), BASE);
    for i in 0..=HEAVY_DAY_PHOTOS {
        write(format!("shoot{i}.png"), BASE + DAY + i as i64);
    }
    write("quiet1.png".into(), BASE + 2 * DAY);

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    let sessions = lib.import_folder(tmp_src.path(), |_| {}).unwrap();
    let mut by_start: Vec<_> = sessions
        .iter()
        .map(|s| (s.started_at, s.photo_count))
        .collect();
    by_start.sort();
    assert_eq!(
        by_start,
        vec![
            (BASE as u64, 1),
            ((BASE + DAY) as u64, HEAVY_DAY_PHOTOS + 1),
            ((BASE + 2 * DAY) as u64, 1),
        ],
        "the heavy day stands alone and does not bridge the quiet days"
    );
}

#[test]
fn folder_import_groups_jpeg_by_exif_capture_date_not_mtime() {
    // meta_test.jpg carries EXIF DateTimeOriginal 2024-06-15 10:30:00 UTC.
    const EXIF_CAPTURE: u64 = 1_718_447_400;
    // A wildly different mtime (2010-01-01 UTC) on a different calendar day, so
    // a regression that reads mtime instead of EXIF would back-date the session
    // to 2010 rather than 2024.
    const WRONG_MTIME: i64 = 1_262_304_000;

    let tmp_src = tempfile::tempdir().unwrap();
    let jpeg = tmp_src.path().join("shot.jpg");
    std::fs::copy(jpeg_path(), &jpeg).unwrap();
    filetime::set_file_mtime(&jpeg, filetime::FileTime::from_unix_time(WRONG_MTIME, 0)).unwrap();

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    let sessions = lib.import_folder(tmp_src.path(), |_| {}).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].started_at, EXIF_CAPTURE,
        "session must be back-dated to the JPEG's EXIF capture date, not its mtime"
    );

    let photos = lib.all_photos(SortOrder::default()).unwrap();
    assert_eq!(photos.len(), 1);
    assert_eq!(
        photos[0].import_date, EXIF_CAPTURE,
        "import_date must come from EXIF, not the filesystem mtime"
    );
}

// ── Delete ────────────────────────────────────────────────────────────────────

#[test]
fn recently_deleted_photo_can_be_restored_with_its_session() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let session = lib.all_sessions().unwrap()[0].clone();
    lib.rename_session(&session.id, "Keep this name").unwrap();
    let collection = lib.create_collection("Restorable").unwrap();
    lib.add_to_collection(collection.id, &[photo.id]).unwrap();
    let active_path = lib.rlab_path(&photo.hash);
    let deleted_path = lib.recently_deleted_path(&photo.hash);
    let thumb_path = lib.thumb_path(&photo.hash);

    lib.delete_photo(photo.id)
        .expect("move to Recently Deleted");

    assert!(!active_path.exists());
    assert!(deleted_path.exists());
    assert!(thumb_path.exists(), "thumbnail is retained for recovery UI");
    assert!(lib.all_photos(SortOrder::default()).unwrap().is_empty());
    assert!(lib.collection_photos(collection.id).unwrap().is_empty());
    assert!(
        lib.all_sessions().unwrap().is_empty(),
        "an all-deleted session should be hidden"
    );
    let deleted = lib.recently_deleted().unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].photo.id, photo.id);

    lib.restore_photo(photo.id).expect("restore photo");

    assert!(active_path.exists());
    assert!(!deleted_path.exists());
    assert!(lib.recently_deleted().unwrap().is_empty());
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 1);
    assert_eq!(lib.collection_photos(collection.id).unwrap().len(), 1);
    let restored_session = &lib.all_sessions().unwrap()[0];
    assert_eq!(restored_session.photo_count, 1);
    assert_eq!(restored_session.name, "Keep this name");
}

#[test]
fn opening_library_finishes_an_interrupted_recently_deleted_move() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let active_path = lib.rlab_path(&photo.hash);
    let deleted_path = lib.recently_deleted_path(&photo.hash);
    std::fs::create_dir_all(deleted_path.parent().unwrap()).unwrap();
    std::fs::rename(&active_path, &deleted_path).unwrap();
    drop(lib);

    let reopened = open_library(tmp.path());

    assert!(
        reopened
            .all_photos(SortOrder::default())
            .unwrap()
            .is_empty()
    );
    let deleted = reopened.recently_deleted().unwrap();
    assert_eq!(deleted.len(), 1);
    assert_eq!(deleted[0].photo.hash, photo.hash);
}

#[test]
fn empty_recently_deleted_permanently_removes_files_and_rows() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let paths: Vec<_> = photos
        .iter()
        .map(|photo| {
            (
                lib.recently_deleted_path(&photo.hash),
                lib.thumb_path(&photo.hash),
            )
        })
        .collect();
    lib.delete_photo(photos[0].id).unwrap();
    assert_eq!(
        lib.all_sessions().unwrap()[0].photo_count,
        1,
        "moving one photo must decrement the active session count"
    );
    lib.delete_photo(photos[1].id).unwrap();

    assert_eq!(lib.recently_deleted().unwrap().len(), 2);
    assert_eq!(lib.empty_recently_deleted().unwrap(), 2);

    assert!(lib.recently_deleted().unwrap().is_empty());
    assert!(lib.all_sessions().unwrap().is_empty());
    for (rlab, thumb) in paths {
        assert!(!rlab.exists());
        assert!(!thumb.exists());
    }
}

#[test]
fn delete_photo_permanently_removes_files_and_db_row() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let row = &photos[0];
    let thumb = lib.thumb_path(&row.hash);
    let photo_id: PhotoId = row.id;

    let rlab = lib.rlab_path(&row.hash);

    lib.delete_photo_permanently(photo_id)
        .expect("delete_photo_permanently");

    assert!(!rlab.exists(), "rlab should be removed");
    assert!(!thumb.exists(), "thumbnail should be removed");
    assert!(
        lib.all_photos(SortOrder::default()).unwrap().is_empty(),
        "DB row should be gone"
    );
    assert!(
        lib.all_sessions().unwrap().is_empty(),
        "the empty import session should not remain in the sidebar"
    );
}

// ── Protection ──────────────────────────────────────────────────────────────

#[test]
fn protected_photo_cannot_be_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let row = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let rlab = lib.rlab_path(&row.hash);

    lib.set_protected(row.id, true).expect("set_protected true");

    // DB mirrors the flag.
    let row = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    assert!(row.protected, "DB row should be protected");

    // Both delete modes refuse a protected photo.
    assert!(
        lib.delete_photo(row.id).is_err(),
        "trash delete must be refused"
    );
    assert!(
        lib.delete_photo_permanently(row.id).is_err(),
        "permanent delete must be refused"
    );
    assert!(rlab.exists(), "rlab must still exist");
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 1);

    // Unprotect: deletion is allowed again.
    lib.set_protected(row.id, false)
        .expect("set_protected false");
    let row = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    assert!(!row.protected);
    lib.delete_photo_permanently(row.id)
        .expect("delete after unprotect");
    assert!(lib.all_photos(SortOrder::default()).unwrap().is_empty());
}

#[test]
fn metadata_edit_works_while_protected() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let row = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let rlab_path = lib.rlab_path(&row.hash);

    lib.set_protected(row.id, true).unwrap();

    // Protection guards against deletion, not metadata edits.
    let lmta = rasterlab_library::LibraryMeta {
        rating: 5,
        ..Default::default()
    };
    lib.update_metadata(row.id, lmta)
        .expect("update protected metadata");

    let rlab = rasterlab_core::project::RlabFile::read(&rlab_path).unwrap();
    assert_eq!(rlab.lmta.unwrap().rating, 5);

    lib.set_protected(row.id, false).unwrap();
}

#[test]
fn failed_unprotect_keeps_the_db_state() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let row = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let rlab_path = lib.rlab_path(&row.hash);
    lib.set_protected(row.id, true).unwrap();

    // Make the protected file unreadable as a project so set_protected fails
    // while rewriting the LMTA chunk.
    std::fs::write(&rlab_path, b"not an rlab file").unwrap();

    assert!(lib.set_protected(row.id, false).is_err());
    assert!(
        lib.all_photos(SortOrder::default()).unwrap()[0].protected,
        "the DB must retain the old protection state"
    );
}

#[test]
fn protection_survives_rebuild_index() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let row = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    lib.set_protected(row.id, true).unwrap();

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");
    let row = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    assert!(
        row.protected,
        "protected flag should survive a rebuild (it lives in the LMTA chunk)"
    );

    lib.set_protected(row.id, false).ok();
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

    lib2.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");
    let photos = lib2.all_photos(SortOrder::default()).unwrap();
    assert_eq!(photos.len(), 2, "should have 2 photos after rebuild");
}

#[test]
fn rebuild_index_restores_session_counts_and_names() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let before = lib.all_sessions().unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].photo_count, 2);

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    let after = lib.all_sessions().unwrap();
    assert_eq!(after.len(), 1, "session row should survive a rebuild");
    assert_eq!(after[0].id, before[0].id);
    assert_eq!(
        after[0].photo_count, 2,
        "session photo count should be recomputed, not left at 0"
    );
    assert_eq!(
        after[0].name, before[0].name,
        "date-based session name should be regenerated"
    );
    assert_eq!(
        after[0].started_at, before[0].started_at,
        "started_at should be rebuilt from the photos' import dates"
    );

    // The photos are still reachable through the session.
    let photos = lib.photos_in_session(&after[0].id).unwrap();
    assert_eq!(photos.len(), 2);
}

/// Collection membership is keyed by photo id, so a rebuild that reassigned
/// ids would break every collection in the library.  Refreshing rows in place
/// is what keeps them stable.
#[test]
fn rebuild_index_keeps_photo_ids() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let before: Vec<(String, PhotoId)> = lib
        .all_photos(SortOrder::default())
        .unwrap()
        .into_iter()
        .map(|row| (row.hash, row.id))
        .collect();

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    let after: Vec<(String, PhotoId)> = lib
        .all_photos(SortOrder::default())
        .unwrap()
        .into_iter()
        .map(|row| (row.hash, row.id))
        .collect();
    assert_eq!(before, after, "a rebuild must not renumber the photo rows");
}

/// The pass that restores collections runs over memberships that a rebuild no
/// longer clears, so it has to be idempotent: two rebuilds must not leave a
/// photo listed twice.
#[test]
fn repeated_rebuilds_leave_one_collection_membership() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let ids: Vec<_> = lib
        .all_photos(SortOrder::default())
        .unwrap()
        .iter()
        .map(|row| row.id)
        .collect();
    let collection = lib.create_collection("Trip").unwrap();
    lib.add_to_collection(collection.id, &ids).unwrap();

    for _ in 0..2 {
        lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
            .expect("rebuild_index");
    }

    assert_eq!(
        lib.collection_photos(collection.id).unwrap().len(),
        2,
        "each photo should be in the collection exactly once"
    );
    assert_eq!(
        lib.all_collections().unwrap().len(),
        1,
        "the collection should not be recreated alongside itself"
    );
}

// ── Recovery: bringing the index back in line with the files ──────────────────

/// A delete that stopped after trashing the file — or a photo removed from
/// `files/` by hand — leaves a row describing something that is not there.
#[test]
fn rebuild_index_drops_rows_whose_file_is_gone() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    assert_eq!(photos.len(), 2);

    let doomed = photos[0].clone();
    std::fs::remove_file(lib.rlab_path(&doomed.hash)).unwrap();

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    let after = lib.all_photos(SortOrder::default()).unwrap();
    assert_eq!(
        after.len(),
        1,
        "the row for the missing file should be gone"
    );
    assert_ne!(after[0].hash, doomed.hash);
    assert_eq!(
        lib.all_sessions().unwrap()[0].photo_count,
        1,
        "the session count should follow the surviving photo"
    );
}

/// A rebuild the user stops has walked only part of the library, so the rows it
/// never reached are not evidence of missing files.  Pruning them would delete
/// photos that are still on disk.
#[test]
fn cancelled_rebuild_keeps_rows_it_never_reached() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 2);

    // Cancelled before the first file, so nothing is re-indexed at all — the
    // worst case for a pass that decides what to drop from what it saw.
    let outcome = lib
        .rebuild_index(Arc::new(AtomicBool::new(true)), |_| {})
        .expect("rebuild_index");

    assert!(outcome.cancelled, "the outcome should report the stop");
    assert_eq!(outcome.done, 0, "no file should have been re-indexed");
    assert_eq!(outcome.total, 2, "the walk still counted both files");
    assert_eq!(
        lib.all_photos(SortOrder::default()).unwrap().len(),
        2,
        "a stopped rebuild must not drop rows it never looked at"
    );
}

/// Membership lives in a join table keyed by photo id, and re-indexing replaces
/// the row (and its id).  A stop between the walk and the restore pass would
/// therefore empty the collection, so the restore has to run either way.
#[test]
fn cancelled_rebuild_keeps_collection_membership() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let ids: Vec<_> = lib
        .all_photos(SortOrder::default())
        .unwrap()
        .iter()
        .map(|row| row.id)
        .collect();
    let collection = lib.create_collection("Trip").unwrap();
    lib.add_to_collection(collection.id, &ids).unwrap();

    // Stop the walk once it is under way, so it re-indexes the first file and
    // never reaches the second.
    let cancel = Arc::new(AtomicBool::new(false));
    let outcome = lib
        .rebuild_index(cancel.clone(), |_| {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        })
        .expect("rebuild_index");

    assert!(outcome.cancelled, "the outcome should report the stop");
    assert_eq!(outcome.done, 1, "one file re-indexed before the stop");
    let members = lib.collection_photos(collection.id).unwrap();
    assert_eq!(
        members.len(),
        2,
        "a stopped rebuild must not strip the collections it re-indexed"
    );
}

/// An empty `files/` is far more often an unmounted volume than a library the
/// user emptied, so a rebuild that finds nothing must not wipe the index.
#[test]
fn rebuild_index_does_not_prune_when_it_finds_no_files() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    std::fs::remove_dir_all(tmp.path().join("files")).unwrap();
    std::fs::create_dir_all(tmp.path().join("files")).unwrap();

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    assert_eq!(
        lib.all_photos(SortOrder::default()).unwrap().len(),
        2,
        "an empty files/ must not be read as a deleted library"
    );
}

/// The generated date name is only a fallback for a session the index has
/// lost; a name the user chose has to survive the pass that rebuilds it.
#[test]
fn rebuild_index_keeps_a_renamed_session() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let session = lib.all_sessions().unwrap()[0].clone();
    lib.rename_session(&session.id, "Kate's wedding").unwrap();

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    let after = lib.all_sessions().unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].name, "Kate's wedding");
    assert_eq!(after[0].photo_count, 1);
}

/// The session count is a cache of the rows, not a running total: a photo
/// deleted out from under it is corrected by the next import, where the old
/// add-this-run's-tally arithmetic would have drifted further with each one.
#[test]
fn session_count_is_recomputed_rather_than_incremented() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    let src = tempfile::tempdir().unwrap();

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    assert_eq!(lib.all_sessions().unwrap()[0].photo_count, 2);

    // Deleting a photo does not touch the cached count …
    let doomed = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    lib.delete_photo_permanently(doomed.id).unwrap();

    // … importing into the same session recomputes it from what is there.
    let extra = src.path().join("extra.png");
    write_png_with_mtime(&extra, 7, 1_600_000_000);
    lib.import_files(&[extra], |_| {}).unwrap();

    let sessions = lib.all_sessions().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].photo_count, 2,
        "one survivor plus one new photo, not the pre-delete total plus one"
    );
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 2);
}

/// Collection membership is recorded in each `.rlab` before the index, so the
/// files alone are enough to put a collection back together.
#[test]
fn collection_membership_survives_a_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let coll = lib.create_collection("Portfolio").unwrap();
    lib.add_to_collection(coll.id, &[photos[0].id]).unwrap();

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    let coll = lib
        .all_collections()
        .unwrap()
        .into_iter()
        .find(|c| c.name == "Portfolio")
        .expect("collection should be rebuilt from the LMTA chunks");
    let members = lib.collection_photos(coll.id).unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].hash, photos[0].hash);
}

/// The point of keeping the name in the index: a rename must not be undone by
/// the stale hints every member file still carries.
#[test]
fn a_rebuild_keeps_the_renamed_collection_name() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let coll = lib.create_collection("Portfolio").unwrap();
    lib.add_to_collection(coll.id, &[photo.id]).unwrap();
    lib.rename_collection(coll.id, "Best Of").unwrap();

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    let collections = lib.all_collections().unwrap();
    assert_eq!(
        collections.len(),
        1,
        "the stale hint started a second collection: {collections:?}"
    );
    assert_eq!(collections[0].name, "Best Of");
    assert_eq!(collections[0].uuid, coll.uuid, "identity must survive too");
    assert_eq!(lib.collection_photos(collections[0].id).unwrap().len(), 1);
}

/// Losing the index is the case the name hints exist for. Member files
/// disagree about the name after a rename, so the most recently written one
/// decides.
#[test]
fn a_lost_index_rebuilds_collections_from_the_newest_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let uuid;
    let hashes: Vec<String>;
    {
        let lib = open_library(tmp.path());
        lib.import_files(&[jpeg_path(), png_path()], |_| {})
            .unwrap();
        let photos = lib.all_photos(SortOrder::default()).unwrap();
        hashes = photos.iter().map(|row| row.hash.clone()).collect();
        let coll = lib.create_collection("Portfolio").unwrap();
        uuid = coll.uuid.clone();
        lib.add_to_collection(coll.id, &[photos[0].id, photos[1].id])
            .unwrap();

        // Stand in for a rename that only one member file has caught up with,
        // and stamp them so the newer hint is unambiguous.
        set_hint(&lib.rlab_path(&hashes[0]), &uuid, "Old Name", 1_000);
        set_hint(&lib.rlab_path(&hashes[1]), &uuid, "Current Name", 2_000);
    }

    // The index is gone entirely; only the files remain.
    let index = tmp.path().join("library.db");
    if index.is_dir() {
        std::fs::remove_dir_all(&index).unwrap();
    } else {
        std::fs::remove_file(&index).unwrap();
    }
    let lib = open_library(tmp.path());
    assert!(lib.all_collections().unwrap().is_empty());

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    let collections = lib.all_collections().unwrap();
    assert_eq!(collections.len(), 1, "one id is one collection");
    assert_eq!(collections[0].uuid, uuid);
    assert_eq!(
        collections[0].name, "Current Name",
        "the most recently written file names the collection"
    );
    assert_eq!(lib.collection_photos(collections[0].id).unwrap().len(), 2);
}

/// Clicking a collection in the sidebar is a search scoped to it, so the
/// filter has to actually return its photos.
#[test]
fn searching_by_collection_returns_its_photos() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let coll = lib.create_collection("Portfolio").unwrap();
    lib.add_to_collection(coll.id, &[photos[0].id]).unwrap();

    let filter = SearchFilter {
        collection_id: Some(coll.id),
        ..Default::default()
    };
    let found = lib.search(&filter, SortOrder::default()).unwrap();

    assert_eq!(found.len(), 1, "collection scope returned {found:?}");
    assert_eq!(found[0].hash, photos[0].hash);

    // The sidebar's filters apply on top of the collection, so the scope has
    // to combine with the rest rather than replace them. Rating the photo
    // outside the collection proves the scope is still doing its job.
    let rate = |photo: &rasterlab_library::PhotoRow, rating: u8| {
        let path = lib.rlab_path(&photo.hash);
        let mut lmta = rasterlab_core::project::RlabFile::read(&path)
            .unwrap()
            .lmta
            .unwrap();
        lmta.rating = rating;
        lib.update_metadata(photo.id, lmta).unwrap();
    };
    rate(&photos[0], 3);
    rate(&photos[1], 5);

    let scoped = SearchFilter {
        collection_id: Some(coll.id),
        rating_min: Some(5),
        ..Default::default()
    };
    assert!(
        lib.search(&scoped, SortOrder::default())
            .unwrap()
            .is_empty(),
        "the five-star photo is not in the collection"
    );

    let scoped = SearchFilter {
        rating_min: Some(3),
        ..scoped
    };
    let found = lib.search(&scoped, SortOrder::default()).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].hash, photos[0].hash);
}

/// A deleted collection must not come back: the files are what a rebuild
/// believes, so they have to stop claiming membership before the index rows
/// go.
#[test]
fn a_deleted_collection_does_not_return_with_a_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let coll = lib.create_collection("Portfolio").unwrap();
    lib.add_to_collection(coll.id, &[photo.id]).unwrap();

    lib.delete_collection(coll.id).unwrap();
    assert!(lib.all_collections().unwrap().is_empty());
    assert!(
        rasterlab_core::project::RlabFile::read(&lib.rlab_path(&photo.hash))
            .unwrap()
            .lmta
            .unwrap()
            .collection_refs
            .is_empty(),
        "the photo still claims to be in the deleted collection"
    );

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");
    assert!(
        lib.all_collections().unwrap().is_empty(),
        "a deleted collection came back from the files"
    );
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 1);
}

/// Files written before collections had ids list them by name alone, and have
/// to keep their memberships until something rewrites them.
#[test]
fn pre_uuid_files_still_rebuild_into_named_collections() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let path = lib.rlab_path(&photo.hash);

    // What such a file looks like: a bare name, no ref.
    let mut rlab = rasterlab_core::project::RlabFile::read(&path).unwrap();
    let mut lmta = rlab.lmta.clone().unwrap();
    lmta.legacy_collections = vec!["Portfolio".to_owned()];
    rlab.set_lmta(Some(lmta));
    rlab.write_v5(&path).unwrap();

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    let collections = lib.all_collections().unwrap();
    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].name, "Portfolio");
    assert!(
        !collections[0].uuid.is_empty(),
        "a rebuilt collection needs an id of its own"
    );
    assert_eq!(lib.collection_photos(collections[0].id).unwrap().len(), 1);

    // And a membership change migrates the file to the new shape in passing.
    // The rebuild reassigned row ids, so the photo has to be looked up again —
    // which is exactly why a file records a collection's uuid and not its id.
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let other = lib.create_collection("Prints").unwrap();
    lib.add_to_collection(other.id, &[photo.id]).unwrap();
    let lmta = rasterlab_core::project::RlabFile::read(&path)
        .unwrap()
        .lmta
        .unwrap();
    assert!(
        lmta.legacy_collections.is_empty(),
        "the legacy list should have been migrated away"
    );
    let mut names: Vec<&str> = lmta
        .collection_refs
        .iter()
        .map(|held| held.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["Portfolio", "Prints"]);
}

/// Overwrite a file's collection hint and write stamp, standing in for a
/// member file that has not caught up with a rename.
fn set_hint(path: &std::path::Path, uuid: &str, name: &str, modified_at: u64) {
    let mut rlab = rasterlab_core::project::RlabFile::read(path).unwrap();
    let mut lmta = rlab.lmta.clone().unwrap();
    lmta.collection_refs = vec![rasterlab_library::CollectionRef {
        id: uuid.to_owned(),
        name: name.to_owned(),
    }];
    rlab.set_lmta(Some(lmta));
    rlab.meta.modified_at = modified_at;
    rlab.write_v5(path).unwrap();
}

#[test]
fn selecting_a_copy_updates_the_project_and_thumbnail_together() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let project_path = lib.rlab_path(&photo.hash);

    let mut project = rasterlab_core::project::RlabFile::read(&project_path).unwrap();
    let mut second = project.copies[0].clone();
    second.name = "Copy 2".into();
    project.copies.push(second);
    project.write_v5(&project_path).unwrap();

    let thumbnail = lib
        .set_active_copy_and_regenerate_thumbnail(&photo.hash, 1)
        .unwrap();

    let saved = rasterlab_core::project::RlabFile::read(&project_path).unwrap();
    assert_eq!(saved.active_copy_index, 1);
    assert_eq!(saved.thumbnail.as_deref(), Some(thumbnail.as_slice()));
    assert_eq!(
        std::fs::read(lib.thumb_path(&photo.hash)).unwrap(),
        thumbnail
    );
}

#[test]
fn metadata_and_active_copy_writes_do_not_overwrite_each_other() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let project_path = lib.rlab_path(&photo.hash);

    let mut project = rasterlab_core::project::RlabFile::read(&project_path).unwrap();
    let mut second = project.copies[0].clone();
    second.name = "Copy 2".into();
    project.copies.push(second);
    let mut lmta = project.lmta.clone().unwrap();
    lmta.rating = 5;
    project.write_v5(&project_path).unwrap();

    let lib = Arc::new(lib);
    let barrier = Arc::new(Barrier::new(3));
    let metadata_worker = {
        let lib = Arc::clone(&lib);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            lib.update_metadata(photo.id, lmta).unwrap();
        })
    };
    let copy_worker = {
        let lib = Arc::clone(&lib);
        let barrier = Arc::clone(&barrier);
        let hash = photo.hash.clone();
        std::thread::spawn(move || {
            barrier.wait();
            lib.set_active_copy_and_regenerate_thumbnail(&hash, 1)
                .unwrap();
        })
    };
    barrier.wait();
    metadata_worker.join().unwrap();
    copy_worker.join().unwrap();

    let saved = rasterlab_core::project::RlabFile::read(&project_path).unwrap();
    assert_eq!(saved.active_copy_index, 1);
    assert_eq!(saved.lmta.unwrap().rating, 5);
}

#[test]
fn metadata_write_cannot_recreate_a_deleted_project() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let active_path = lib.rlab_path(&photo.hash);
    let deleted_path = lib.recently_deleted_path(&photo.hash);
    let mut lmta = rasterlab_core::project::RlabFile::read(&active_path)
        .unwrap()
        .lmta
        .unwrap();
    lmta.rating = 4;

    let lib = Arc::new(lib);
    let barrier = Arc::new(Barrier::new(3));
    let metadata_worker = {
        let lib = Arc::clone(&lib);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            lib.update_metadata(photo.id, lmta).unwrap();
        })
    };
    let delete_worker = {
        let lib = Arc::clone(&lib);
        let barrier = Arc::clone(&barrier);
        std::thread::spawn(move || {
            barrier.wait();
            lib.delete_photo(photo.id).unwrap();
        })
    };
    barrier.wait();
    metadata_worker.join().unwrap();
    delete_worker.join().unwrap();

    assert!(!active_path.exists());
    assert!(deleted_path.exists());
    assert_eq!(lib.recently_deleted().unwrap().len(), 1);
}

/// Adding to a collection that is not there used to update the index and
/// silently skip the files; it has to fail before touching either.
#[test]
fn adding_to_an_unknown_collection_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();

    let err = lib
        .add_to_collection(9999, &[photo.id])
        .expect_err("unknown collection must be rejected");
    assert!(err.to_string().contains("9999"), "{err}");
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
fn search_by_text_matches_original_import_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();

    let filter = SearchFilter {
        text: Some("meta_test".to_owned()),
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
fn search_by_text_matches_import_source_path() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();

    let filter = SearchFilter {
        text: Some("test_images".to_owned()),
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
fn search_text_is_case_insensitive() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();

    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let jpeg_row = photos
        .iter()
        .find(|r| r.original_filename.as_deref() == Some("meta_test.jpg"))
        .expect("jpeg photo");

    let lmta = rasterlab_library::LibraryMeta {
        keywords: vec!["Vacation".to_owned()],
        ..Default::default()
    };
    lib.update_metadata(jpeg_row.id, lmta).unwrap();

    // A keyword stored as "Vacation" must be found regardless of the
    // case the user types into the search box.
    for query in ["vacation", "VACATION", "VaCaTiOn"] {
        let filter = SearchFilter {
            text: Some(query.to_owned()),
            ..Default::default()
        };
        let results = lib.search(&filter, SortOrder::default()).unwrap();
        assert_eq!(results.len(), 1, "search {query:?} should match 'Vacation'");
        assert_eq!(
            results[0].original_filename.as_deref(),
            Some("meta_test.jpg")
        );
    }
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

    // The file records the collection's id, and the name only as a hint.
    let rlab_path = lib.rlab_path(&members[0].hash);
    let held = |path: &std::path::Path| {
        rasterlab_core::project::RlabFile::read(path)
            .unwrap()
            .lmta
            .unwrap()
            .collection_refs
    };
    assert_eq!(held(&rlab_path).len(), 1);
    assert_eq!(held(&rlab_path)[0].id, coll.uuid);
    assert_eq!(held(&rlab_path)[0].name, "Favorites");

    // A rename is the index's business alone: the member file is not even
    // opened, which is what keeps renaming a large collection instant.
    let untouched = std::fs::metadata(&rlab_path).unwrap().modified().unwrap();
    lib.rename_collection(coll.id, "Best Of").unwrap();
    assert_eq!(
        std::fs::metadata(&rlab_path).unwrap().modified().unwrap(),
        untouched,
        "renaming rewrote a member file"
    );
    assert_eq!(lib.all_collections().unwrap()[0].name, "Best Of");
    assert_eq!(
        lib.collection_photos(coll.id).unwrap().len(),
        1,
        "membership follows the id, not the name"
    );
    assert_eq!(
        held(&rlab_path)[0].name,
        "Favorites",
        "the hint in the file is allowed to go stale"
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
        lmta.collection_refs.is_empty(),
        "collection should be removed from LMTA"
    );
}

/// The membership the app reads back is the whole point of the two-step write,
/// so a repeated add must leave one membership — and must not rewrite a file
/// that already says the right thing, which would churn every overlapping
/// photo's mtime for nothing.
#[test]
fn re_adding_a_photo_changes_neither_the_index_nor_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let (first, second) = (&photos[0], &photos[1]);

    let coll = lib.create_collection("Favorites").unwrap();
    lib.add_to_collection(coll.id, &[first.id]).unwrap();
    let written = std::fs::metadata(lib.rlab_path(&first.hash))
        .unwrap()
        .modified()
        .unwrap();

    // The ordinary case once the UI exists: a selection that is partly in the
    // collection already.
    lib.add_to_collection(coll.id, &[first.id, second.id])
        .unwrap();

    let members = lib.collection_photos(coll.id).unwrap();
    assert_eq!(members.len(), 2, "re-added photo listed twice");
    assert_eq!(
        std::fs::metadata(lib.rlab_path(&first.hash))
            .unwrap()
            .modified()
            .unwrap(),
        written,
        "a file that already lists the collection was rewritten"
    );
}

/// A membership row for a photo the index has never heard of would survive
/// every cleanup path, since they all key off the photo.
#[test]
fn adding_an_unknown_photo_adds_no_membership() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo_id = lib.all_photos(SortOrder::default()).unwrap()[0].id;
    let coll = lib.create_collection("Favorites").unwrap();

    lib.add_to_collection(coll.id, &[photo_id, 9999]).unwrap();

    let members = lib.collection_photos(coll.id).unwrap();
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].id, photo_id);
}

/// Membership belongs to the add/remove API, which writes the file and the
/// index together. Metadata editors hold an LMTA read when their photo was
/// selected, so one that was read before the photo joined a collection would
/// otherwise put the photo back out of it the moment the user touched a
/// rating — leaving the file and the index disagreeing, with the file winning
/// the next rebuild.
#[test]
fn a_metadata_write_leaves_collection_membership_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let read_lmta = || {
        rasterlab_core::project::RlabFile::read(&lib.rlab_path(&photo.hash))
            .unwrap()
            .lmta
            .unwrap()
    };

    // What a detail panel would have loaded when the photo was selected.
    let stale = read_lmta();
    assert!(stale.collection_refs.is_empty());

    let coll = lib.create_collection("Favorites").unwrap();
    lib.add_to_collection(coll.id, &[photo.id]).unwrap();

    // A rating edit, carrying the pre-collection list along with it.
    let mut edit = stale.clone();
    edit.rating = 4;
    lib.update_metadata(photo.id, edit).unwrap();

    let after = read_lmta();
    assert_eq!(after.rating, 4, "the edit itself must still land");
    assert_eq!(
        after
            .collection_refs
            .iter()
            .map(|held| held.name.as_str())
            .collect::<Vec<_>>(),
        ["Favorites"],
        "the file forgot a collection it had joined"
    );
    assert_eq!(lib.collection_photos(coll.id).unwrap().len(), 1);

    // And the mirror image: a stale list must not put a photo back into a
    // collection it has left.
    lib.remove_from_collection(coll.id, &[photo.id]).unwrap();
    lib.update_metadata(photo.id, after).unwrap();
    assert!(
        read_lmta().collection_refs.is_empty(),
        "the file rejoined a collection it had left"
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

// ── Library photos as multi-frame op sources ──────────────────────────────────

/// A 64×64 frame that is sharp (checkerboard) inside `sharp_x` and flat grey
/// everywhere else — the focus measure has to prefer this frame in that band.
fn focus_frame(sharp_x: std::ops::Range<usize>) -> rasterlab_core::Image {
    const SIDE: u32 = 64;
    let mut img = rasterlab_core::Image::new(SIDE, SIDE);
    for y in 0..SIDE as usize {
        for x in 0..SIDE as usize {
            let v = if sharp_x.contains(&x) && (x + y) % 2 == 0 {
                30
            } else if sharp_x.contains(&x) {
                220
            } else {
                120
            };
            let px = &mut img.data[(y * SIDE as usize + x) * 4..][..4];
            px.copy_from_slice(&[v, v, v, 255]);
        }
    }
    img
}

fn write_png(path: &std::path::Path, image: &rasterlab_core::Image) {
    use rasterlab_core::{formats::FormatRegistry, traits::format_handler::EncodeOptions};
    let bytes = FormatRegistry::with_builtins()
        .encode_file(image, path, &EncodeOptions::default())
        .expect("encode png");
    std::fs::write(path, bytes).expect("write png");
}

/// A library photo is a `.rlab` container, not an image file, so starting a
/// focus stack from the library grid hands the op paths that no image decoder
/// understands. It has to fuse them from their embedded originals.
#[test]
fn focus_stack_fuses_imported_library_photos() {
    use rasterlab_core::{Image, ops::FocusStackOp, traits::operation::Operation};

    let tmp = tempfile::tempdir().unwrap();
    let left = tmp.path().join("left.png");
    let right = tmp.path().join("right.png");
    write_png(&left, &focus_frame(8..24));
    write_png(&right, &focus_frame(40..56));

    let lib = open_library(&tmp.path().join("lib"));
    let session = lib.import_files(&[left, right], |_| {}).unwrap();
    assert!(session.errors.is_empty(), "{:?}", session.errors);

    let frames: Vec<String> = lib
        .all_photos(SortOrder::default())
        .unwrap()
        .iter()
        .map(|row| lib.rlab_path(&row.hash).to_string_lossy().into_owned())
        .collect();
    assert_eq!(frames.len(), 2);

    let fused = FocusStackOp::new(frames)
        .apply(Image::new(1, 1))
        .expect("focus stack over library photos");

    assert_eq!((fused.width, fused.height), (64, 64));
    // Both sharp bands survive the fusion: each was in focus in one frame only,
    // so a fused pixel there must keep the checker's contrast rather than the
    // flat 120 grey the other frame contributed.
    for x in [16usize, 48] {
        let row = 32 * 64;
        let a = fused.data[(row + x) * 4] as i16;
        let b = fused.data[(row + x + 1) * 4] as i16;
        assert!(
            (a - b).abs() > 100,
            "x={x} lost the in-focus detail: {a} vs {b}",
        );
    }
}

// ── Edited-only filter ────────────────────────────────────────────────────────

/// The edited-only filter reads a column of the index that no `LMTA` field
/// backs, so every path that writes a photo row has to derive it from the
/// file's virtual copies.  A rebuild that let the column default to zero took
/// every previously edited photo out of the filter.
#[test]
fn the_edited_flag_survives_a_rebuild() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let edited = lib
        .all_photos(SortOrder::default())
        .unwrap()
        .into_iter()
        .find(|row| row.original_filename.as_deref() == Some("meta_test.jpg"))
        .expect("imported jpeg");
    give_the_photo_an_edit(&lib, &edited.hash, &jpeg_path());
    assert!(edited_hashes(&lib) == vec![edited.hash.clone()]);

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    assert_eq!(
        edited_hashes(&lib),
        vec![edited.hash],
        "the rebuild lost the edited flag the .rlab still records"
    );
}

/// Importing a project that already carries edits — an editor `.rlab`, or a
/// photo moved between libraries — has to index it as edited straight away.
#[test]
fn an_imported_project_with_edits_is_indexed_as_edited() {
    use rasterlab_core::project::{RlabFile, RlabMeta, SavedCopy};

    let project_dir = tempfile::tempdir().unwrap();
    let project_path = project_dir.path().join("edited-photo.rlab");
    let original_bytes = std::fs::read(png_path()).unwrap();
    let project = RlabFile::new(
        RlabMeta::new(
            "test",
            Some(png_path().to_string_lossy().into_owned()),
            0,
            0,
        ),
        original_bytes,
        vec![SavedCopy {
            name: "Edited copy".into(),
            pipeline_state: edited_pipeline_state(&png_path()),
        }],
        0,
        None,
    );
    project.write_v5(&project_path).unwrap();

    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    let session = lib.import_files(&[project_path], |_| {}).unwrap();
    assert!(session.errors.is_empty(), "{:?}", session.errors);

    let photos = lib.all_photos(SortOrder::default()).unwrap();
    assert!(photos[0].has_edits, "an edited import was indexed as clean");
    assert_eq!(edited_hashes(&lib), vec![photos[0].hash.clone()]);
}

/// A photo is edited when *any* of its virtual copies is, so selecting the
/// untouched Copy 1 must not take it out of the filter.
#[test]
fn selecting_an_unedited_copy_keeps_the_photo_edited() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();

    let project_path = lib.rlab_path(&photo.hash);
    let mut project = rasterlab_core::project::RlabFile::read(&project_path).unwrap();
    let mut edited_copy = project.copies[0].clone();
    edited_copy.name = "Copy 2".into();
    edited_copy.pipeline_state = edited_pipeline_state(&jpeg_path());
    project.copies.push(edited_copy);
    project.write_v5(&project_path).unwrap();
    lib.set_active_copy_and_regenerate_thumbnail(&photo.hash, 1)
        .unwrap();
    assert_eq!(edited_hashes(&lib), vec![photo.hash.clone()]);

    lib.set_active_copy_and_regenerate_thumbnail(&photo.hash, 0)
        .unwrap();

    assert_eq!(
        edited_hashes(&lib),
        vec![photo.hash],
        "going back to the untouched copy dropped the photo's edits"
    );
}

/// Hashes the edited-only filter returns, scoped to the photos' own import
/// session the way the library sidebar scopes it.
fn edited_hashes(lib: &Library) -> Vec<String> {
    let session = lib.all_sessions().unwrap();
    let filter = SearchFilter {
        import_session: session.first().map(|row| row.id.clone()),
        has_edits_only: true,
        ..Default::default()
    };
    lib.search(&filter, SortOrder::default())
        .unwrap()
        .into_iter()
        .map(|row| row.hash)
        .collect()
}
