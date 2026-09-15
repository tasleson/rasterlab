use std::{
    path::PathBuf,
    sync::{
        Arc, Barrier, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use rasterlab_library::{
    ImportCollection, Library, MembershipChange, NotALibrary,
    db_trait::{PhotoId, PhotoRow, SortOrder},
    search::{Resolution, SearchFilter},
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

fn no_cancel() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
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

/// A file already in the library is skipped for writing, but it must still
/// count toward its capture day: dropping duplicates from the dated list would
/// move session boundaries on every reimport.  Here the two duplicates bridge
/// the new photo back into the run they already belong to.
#[test]
fn grouped_reimport_keeps_duplicates_in_clusters() {
    const DAY: i64 = 86_400;
    const EXIF_CAPTURE: i64 = 1_718_447_400; // 2024-06-15 10:30 UTC
    const WRONG_MTIME: i64 = 1_262_304_000;

    let tmp_src = tempfile::tempdir().unwrap();
    let jpeg = tmp_src.path().join("shot.jpg");
    let prior_png = tmp_src.path().join("prior.png");
    std::fs::copy(jpeg_path(), &jpeg).unwrap();
    // Deliberately misleading: the scan must date this by EXIF, not by mtime.
    filetime::set_file_mtime(&jpeg, filetime::FileTime::from_unix_time(WRONG_MTIME, 0)).unwrap();
    write_png_with_mtime(&prior_png, 1, EXIF_CAPTURE + DAY);

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());
    let first = lib.import_folder(tmp_src.path(), |_| {}).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].started_at, EXIF_CAPTURE as u64);
    assert_eq!(first[0].photo_count, 2);

    write_png_with_mtime(&tmp_src.path().join("new.png"), 2, EXIF_CAPTURE + 2 * DAY);
    let second = lib.import_folder(tmp_src.path(), |_| {}).unwrap();

    assert_eq!(second.len(), 1, "all three dated paths remain one run");
    assert_eq!(
        second[0].started_at, EXIF_CAPTURE as u64,
        "the duplicates still anchor the run to the original shoot day"
    );
    assert_eq!(second[0].photo_count, 1);
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 3);
}

// ── Import collections ──────────────────────────────────────────────────────

/// A folder of shoot directories, each holding one distinct photo, plus one
/// photo loose at the top.  Returns the source directory (kept alive by the
/// caller) so the tests below share one shape.
fn shoot_tree() -> tempfile::TempDir {
    const BASE: i64 = 1_600_000_000;
    let src = tempfile::tempdir().unwrap();
    for (dir, tag) in [("Sunrise", 1u8), ("Harbour", 2)] {
        let sub = src.path().join(dir);
        std::fs::create_dir_all(&sub).unwrap();
        write_png_with_mtime(&sub.join("shot.png"), tag, BASE);
    }
    write_png_with_mtime(&src.path().join("loose.png"), 3, BASE);
    src
}

/// Collection names in the library, sorted, so assertions do not depend on
/// index row order.
fn collection_names(lib: &Library) -> Vec<String> {
    let mut names: Vec<String> = lib
        .all_collections()
        .unwrap()
        .into_iter()
        .map(|row| row.name)
        .collect();
    names.sort();
    names
}

#[test]
fn per_folder_import_makes_one_collection_per_directory() {
    let src = shoot_tree();
    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    lib.import_folder_into_collection(src.path(), ImportCollection::PerFolder, |_| {})
        .unwrap();

    let root_name = src
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let mut expected = vec!["Harbour".to_owned(), "Sunrise".to_owned(), root_name];
    expected.sort();
    assert_eq!(
        collection_names(&lib),
        expected,
        "each directory that directly holds photos gets its own collection"
    );

    for row in lib.all_collections().unwrap() {
        assert_eq!(
            lib.collection_photos(row.id).unwrap().len(),
            1,
            "collection “{}” should hold only its own directory's photo",
            row.name
        );
    }
}

/// A cancel flag that is already set has to stop the run before it writes
/// anything: the CLI arms it from SIGINT and a "stopped" tally that had
/// quietly imported half the folder anyway would be a lie.
#[test]
fn a_cancelled_import_stops_without_importing() {
    let src = shoot_tree();
    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    let cancel = Arc::new(AtomicBool::new(true));
    let sessions = lib
        .import_paths(
            &[src.path().to_path_buf()],
            ImportCollection::None,
            cancel,
            |_| {},
        )
        .unwrap();

    assert!(sessions.is_empty(), "a cancelled run made a session");
    assert!(lib.all_photos(SortOrder::default()).unwrap().is_empty());
}

/// Cancellation is polled between photos. Each completed photo must already
/// have both its durable project and its matching collection row, rather than
/// waiting for a batch-final membership pass that cancellation could skip.
#[test]
fn cancelling_a_collection_import_keeps_completed_memberships() {
    let src = shoot_tree();
    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());
    let cancel = no_cancel();
    let cancel_from_callback = Arc::clone(&cancel);

    let sessions = lib
        .import_paths(
            &[src.path().to_path_buf()],
            ImportCollection::Named("Cancelled collection".into()),
            cancel,
            move |progress| {
                if !progress.scanning && progress.done == 0 {
                    cancel_from_callback.store(true, Ordering::Relaxed);
                }
            },
        )
        .unwrap();

    assert_eq!(
        sessions
            .iter()
            .map(|session| session.photo_count)
            .sum::<usize>(),
        1
    );
    let collection = lib.all_collections().unwrap().remove(0);
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 1);
    assert_eq!(lib.collection_photos(collection.id).unwrap().len(), 1);
}

/// Naming a folder and a file inside it is an easy thing to type, and has to
/// cost one import rather than two attempts at the same photo.
#[test]
fn import_paths_takes_each_file_once() {
    let src = shoot_tree();
    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    let inside = src.path().join("Sunrise").join("shot.png");
    assert!(inside.is_file(), "shoot tree changed shape");
    let paths = vec![src.path().to_path_buf(), inside];

    let totals = std::cell::RefCell::new(Vec::new());
    let sessions = lib
        .import_paths(&paths, ImportCollection::None, no_cancel(), |p| {
            totals.borrow_mut().push(p.total)
        })
        .unwrap();

    let totals = totals.into_inner();
    assert!(totals.iter().all(|&t| t == 3), "double-counted: {totals:?}");
    let imported: usize = sessions.iter().map(|s| s.photo_count).sum();
    assert_eq!(imported, 3);
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 3);
}

#[test]
fn named_import_files_the_whole_run_into_one_collection() {
    let src = shoot_tree();
    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    lib.import_folder_into_collection(
        src.path(),
        ImportCollection::Named("  Iceland 2024  ".to_owned()),
        |_| {},
    )
    .unwrap();

    assert_eq!(
        collection_names(&lib),
        ["Iceland 2024"],
        "the name is trimmed and used once for the whole import"
    );
    let collection = lib.all_collections().unwrap().remove(0);
    assert_eq!(
        lib.collection_photos(collection.id).unwrap().len(),
        3,
        "every photo in the tree joins it, whichever directory it came from"
    );
}

#[test]
fn plain_folder_import_creates_no_collections() {
    let src = shoot_tree();
    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    lib.import_folder(src.path(), |_| {}).unwrap();

    assert!(
        lib.all_collections().unwrap().is_empty(),
        "an import that was not asked to file anything must not"
    );
}

/// Re-importing a folder the user has added to must extend the collection the
/// first import made, not stand up a second one beside it.
#[test]
fn reimporting_a_folder_reuses_its_collection() {
    const BASE: i64 = 1_600_000_000;
    let src = tempfile::tempdir().unwrap();
    let sub = src.path().join("Harbour");
    std::fs::create_dir_all(&sub).unwrap();
    write_png_with_mtime(&sub.join("one.png"), 1, BASE);

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    lib.import_folder_into_collection(src.path(), ImportCollection::PerFolder, |_| {})
        .unwrap();
    let first = lib.all_collections().unwrap();
    assert_eq!(first.len(), 1);
    let collection_id = first[0].id;

    write_png_with_mtime(&sub.join("two.png"), 2, BASE);
    lib.import_folder_into_collection(src.path(), ImportCollection::PerFolder, |_| {})
        .unwrap();

    let after = lib.all_collections().unwrap();
    assert_eq!(after.len(), 1, "the second run must reuse the collection");
    assert_eq!(after[0].id, collection_id);
    assert_eq!(
        lib.collection_photos(collection_id).unwrap().len(),
        2,
        "the newly added photo joins; the duplicate is skipped whole"
    );
}

/// The user's own "Harbour" is the collection the import should join, even
/// though the directory is spelt differently — the UI refuses to let two
/// collections differ only by case, so an import must not create the pair.
#[test]
fn import_joins_an_existing_collection_ignoring_case() {
    const BASE: i64 = 1_600_000_000;
    let src = tempfile::tempdir().unwrap();
    let sub = src.path().join("HARBOUR");
    std::fs::create_dir_all(&sub).unwrap();
    write_png_with_mtime(&sub.join("one.png"), 1, BASE);

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());
    let existing = lib.create_collection("Harbour").unwrap();

    lib.import_folder_into_collection(src.path(), ImportCollection::PerFolder, |_| {})
        .unwrap();

    assert_eq!(collection_names(&lib), ["Harbour"]);
    assert_eq!(lib.collection_photos(existing.id).unwrap().len(), 1);
}

/// A folder whose photographs are all already in the library must not leave an
/// empty collection named after it: the collection is created only once a file
/// turns out to be a genuinely new photograph.
#[test]
fn a_folder_of_only_duplicates_creates_no_collection() {
    const BASE: i64 = 1_600_000_000;
    let src = tempfile::tempdir().unwrap();
    let sub = src.path().join("Harbour");
    std::fs::create_dir_all(&sub).unwrap();
    write_png_with_mtime(&sub.join("one.png"), 1, BASE);

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    lib.import_folder(src.path(), |_| {}).unwrap();
    assert!(lib.all_collections().unwrap().is_empty());

    lib.import_folder_into_collection(src.path(), ImportCollection::PerFolder, |_| {})
        .unwrap();

    assert!(
        lib.all_collections().unwrap().is_empty(),
        "nothing was imported, so there is nothing to file and no collection to make"
    );
}

/// Several files of one batch can hold identical bytes.  Preparation runs them
/// concurrently, so none of them can find the others in the index; only the
/// commit-time check stops the same photograph landing more than once, and it
/// has to leave the same tally a strictly serial import would have.
#[test]
fn identical_files_in_one_batch_are_imported_once() {
    const BASE: i64 = 1_600_000_000;
    const COPIES: [&str; 6] = ["a.png", "b.png", "c.png", "d.png", "e.png", "f.png"];
    let src = tempfile::tempdir().unwrap();
    let sub = src.path().join("Harbour");
    std::fs::create_dir_all(&sub).unwrap();
    for name in COPIES {
        write_png_with_mtime(&sub.join(name), 1, BASE);
    }
    write_png_with_mtime(&sub.join("other.png"), 2, BASE);

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    let progress = Arc::new(Mutex::new(Vec::new()));
    let progress_sink = progress.clone();
    let sessions = lib
        .import_folder_into_collection(src.path(), ImportCollection::PerFolder, move |p| {
            progress_sink.lock().unwrap().push(p)
        })
        .unwrap();

    let imported: usize = sessions.iter().map(|s| s.photo_count).sum();
    assert_eq!(imported, 2, "six identical files are one photograph");
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 2);
    assert!(
        sessions.iter().all(|s| s.errors.is_empty()),
        "an in-batch duplicate is a skip, not a failure: {:?}",
        sessions.iter().flat_map(|s| &s.errors).collect::<Vec<_>>()
    );

    let progress = progress.lock().unwrap();
    let last = progress
        .iter()
        .rev()
        .find(|p| !p.scanning)
        .expect("final import progress");
    assert_eq!(last.done, COPIES.len() + 1);
    assert_eq!(last.imported, 2);
    assert_eq!(last.skipped_duplicates, COPIES.len() - 1);

    assert_eq!(collection_names(&lib), ["Harbour"]);
    let collection = lib.all_collections().unwrap()[0].id;
    assert_eq!(
        lib.collection_photos(collection).unwrap().len(),
        2,
        "a skipped duplicate must not add a second membership row"
    );
}

/// Membership is written into the `.rlab` at import time, so it is a property
/// of the photograph rather than of the index — a rebuild after total index
/// loss has to bring it back.
#[test]
fn import_collection_membership_survives_index_loss() {
    const BASE: i64 = 1_600_000_000;
    let src = tempfile::tempdir().unwrap();
    let sub = src.path().join("Harbour");
    std::fs::create_dir_all(&sub).unwrap();
    write_png_with_mtime(&sub.join("one.png"), 1, BASE);

    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());
    lib.import_folder_into_collection(src.path(), ImportCollection::PerFolder, |_| {})
        .unwrap();
    drop(lib);

    let db_path = tmp_lib.path().join("library.db");
    if db_path.is_dir() {
        std::fs::remove_dir_all(&db_path).unwrap();
    } else if db_path.exists() {
        std::fs::remove_file(&db_path).unwrap();
    }

    let lib = open_library(tmp_lib.path());
    assert!(lib.all_collections().unwrap().is_empty());
    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    assert_eq!(collection_names(&lib), ["Harbour"]);
    let collection = lib.all_collections().unwrap().remove(0);
    assert_eq!(lib.collection_photos(collection.id).unwrap().len(), 1);
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

/// The detail panel reads a selected photo's `.rlab` for its collections and
/// the rest of its metadata.  A photo in Recently Deleted has been moved out of
/// `files/`, so looking for it where an active photo's file lives reported the
/// photo as unreadable instead.
#[test]
fn a_deleted_photos_metadata_is_still_read_and_written_where_the_file_now_is() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let collection = lib.create_collection("Portfolio").unwrap();
    lib.add_to_collection(collection.id, &[photo.id]).unwrap();
    lib.delete_photo(photo.id)
        .expect("move to Recently Deleted");

    let path = lib.photo_rlab_path(&photo.hash);
    assert_eq!(path, lib.recently_deleted_path(&photo.hash));
    let summary = rasterlab_core::project::read_library_summary(&path)
        .expect("read a deleted photo's metadata");
    let lmta = summary.lmta.expect("lmta");
    assert_eq!(
        lmta.collection_refs
            .iter()
            .map(|held| held.name.as_str())
            .collect::<Vec<_>>(),
        ["Portfolio"],
        "the photo keeps its collections while it waits in Recently Deleted"
    );

    // And an edit made from that panel reaches the file, rather than the index
    // alone — a restore followed by a rebuild would otherwise lose it.
    let mut edited = lmta.clone();
    edited.rating = 4;
    lib.update_metadata(photo.id, edited)
        .expect("update_metadata");
    let written = rasterlab_core::project::read_library_summary(&path)
        .unwrap()
        .lmta
        .unwrap();
    assert_eq!(written.rating, 4);

    lib.restore_photo(photo.id).expect("restore photo");
    assert_eq!(
        lib.photo_rlab_path(&photo.hash),
        lib.rlab_path(&photo.hash),
        "a restored photo is read from files/ again"
    );
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

/// A rebuild reads the files to decide what the library holds, and Recently
/// Deleted is part of it.  Walking `files/` alone left a library whose index
/// was lost with no trace of the photos waiting there: no row to restore, to
/// empty, or even to show, while their files stayed on disk for good.
#[test]
fn rebuild_index_keeps_and_recovers_recently_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let kept = photos[0].clone();
    let deleted = photos[1].clone();
    let collection = lib.create_collection("Portfolio").unwrap();
    lib.add_to_collection(collection.id, &[deleted.id]).unwrap();
    lib.delete_photo(deleted.id)
        .expect("move to Recently Deleted");
    let deleted_at = lib.recently_deleted().unwrap()[0].deleted_at;

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");
    assert_eq!(
        lib.recently_deleted().unwrap()[0].deleted_at,
        deleted_at,
        "a rebuild restarted the photo's stay in Recently Deleted"
    );

    // Now the case a rebuild is really for: the index is gone, and everything
    // the library holds has to come back from the files themselves.
    drop(lib);
    std::fs::remove_dir_all(tmp.path().join("library.db")).unwrap();
    let lib = open_library(tmp.path());
    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    let recovered = lib.recently_deleted().unwrap();
    assert_eq!(recovered.len(), 1, "the deleted photo was not recovered");
    assert_eq!(recovered[0].photo.hash, deleted.hash);
    assert!(recovered[0].deleted_at > 0);
    let active = lib.all_photos(SortOrder::default()).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].hash, kept.hash);
    assert!(
        lib.collection_photos(lib.all_collections().unwrap()[0].id)
            .unwrap()
            .is_empty(),
        "a deleted photo is not shown among a collection's photos"
    );

    // The point of getting the row back: the photo can still be restored, to
    // the collection its own file remembers it belongs to.
    lib.restore_photo(recovered[0].photo.id)
        .expect("restore photo");
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 2);
    let collections = lib.all_collections().unwrap();
    assert_eq!(collections.len(), 1);
    assert_eq!(collections[0].name, "Portfolio");
    assert_eq!(
        lib.collection_photos(collections[0].id).unwrap()[0].hash,
        deleted.hash
    );
}

/// The files are the record on this too: a photo whose file is back in
/// `files/` is an active one, however the index came to think otherwise.
#[test]
fn rebuild_index_reactivates_a_photo_whose_file_came_back() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    lib.delete_photo(photo.id).unwrap();

    // A restore that moved the file and then died before the index caught up.
    std::fs::rename(
        lib.recently_deleted_path(&photo.hash),
        lib.rlab_path(&photo.hash),
    )
    .unwrap();

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");

    assert!(lib.recently_deleted().unwrap().is_empty());
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 1);
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

/// Filing a selection into a collection is a `.rlab` rewrite per photo, so it
/// has to be stoppable — and what it got through before stopping has to be in
/// the index, or the files and the index disagree about who is a member.
#[test]
fn a_stopped_collection_change_keeps_what_it_already_filed() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(
        &[
            jpeg_path(),
            png_path(),
            test_images_dir().join("hue_wheel.png"),
        ],
        |_| {},
    )
    .unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let ids: Vec<PhotoId> = photos.iter().map(|row| row.id).collect();
    let coll = lib.create_collection("Portfolio").unwrap();

    // Ask to stop once the first photo is filed. The report that carries that
    // count comes after the second photo has been claimed, so the run gets as
    // far as two and the third is never attempted.
    let cancel = no_cancel();
    let watcher = cancel.clone();
    let outcome = lib
        .change_collection_membership(
            coll.id,
            &ids,
            MembershipChange::Add,
            cancel,
            move |progress| {
                if progress.done >= 1 {
                    watcher.store(true, Ordering::Relaxed);
                }
            },
        )
        .expect("the run itself must not fail");

    assert!(outcome.cancelled, "the run should report that it stopped");
    assert_eq!(outcome.done, 2, "what it got to before stopping");
    let mut members: Vec<String> = lib
        .collection_photos(coll.id)
        .unwrap()
        .into_iter()
        .map(|row| row.hash)
        .collect();
    members.sort();
    let mut filed: Vec<String> = photos[..2].iter().map(|row| row.hash.clone()).collect();
    filed.sort();
    assert_eq!(
        members, filed,
        "the index must list exactly the photos whose files were written"
    );
}

/// Photos the index no longer lists are dropped before the run starts rather
/// than counted and skipped, so the progress bar counts down real work.
#[test]
fn a_collection_change_counts_only_photos_it_can_write() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].id;
    let coll = lib.create_collection("Portfolio").unwrap();

    let totals = Arc::new(Mutex::new(Vec::new()));
    let seen = totals.clone();
    let outcome = lib
        .change_collection_membership(
            coll.id,
            &[photo, 9999],
            MembershipChange::Add,
            no_cancel(),
            move |p| {
                seen.lock().unwrap().push(p.total);
            },
        )
        .unwrap();

    assert_eq!(outcome.done, 1);
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert!(
        totals.lock().unwrap().iter().all(|&total| total == 1),
        "a photo that cannot be written must not be counted: {:?}",
        totals.lock().unwrap()
    );
    assert_eq!(lib.collection_photos(coll.id).unwrap().len(), 1);
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

/// Deleting a collection has to reach the files of members waiting in Recently
/// Deleted as well.  Skipping them left the collection recorded in a file that
/// a rebuild reads, so the collection came back — and a restored photo turned
/// up in a collection the user had thrown away.
#[test]
fn deleting_a_collection_reaches_members_in_recently_deleted() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let (kept, deleted) = (photos[0].clone(), photos[1].clone());
    let collection = lib.create_collection("Portfolio").unwrap();
    lib.add_to_collection(collection.id, &[kept.id, deleted.id])
        .unwrap();
    lib.delete_photo(deleted.id)
        .expect("move to Recently Deleted");

    lib.delete_collection(collection.id)
        .expect("delete_collection");

    assert!(
        rasterlab_core::project::RlabFile::read(&lib.recently_deleted_path(&deleted.hash))
            .unwrap()
            .lmta
            .unwrap()
            .collection_refs
            .is_empty(),
        "a photo in Recently Deleted still claims the deleted collection"
    );

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");
    assert!(
        lib.all_collections().unwrap().is_empty(),
        "a deleted collection came back from a deleted photo's file"
    );

    let restored = lib.recently_deleted().unwrap()[0].photo.id;
    lib.restore_photo(restored).expect("restore photo");
    assert!(lib.all_collections().unwrap().is_empty());
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

#[test]
fn search_by_resolution_bounds_is_orientation_agnostic() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    // meta_test.jpg is 640x480, color_patches.png is 576x384; add a portrait
    // 400x600 so the long/short-edge comparison has something to get wrong.
    let portrait = tmp.path().join("portrait.png");
    image::RgbImage::from_pixel(400, 600, image::Rgb([9, 9, 9]))
        .save(&portrait)
        .expect("write portrait png");
    lib.import_files(&[jpeg_path(), png_path(), portrait], |_| {})
        .unwrap();

    struct Case {
        desc: &'static str,
        min: Option<Resolution>,
        max: Option<Resolution>,
        expected: &'static [&'static str],
    }
    let case = |desc, min, max, expected| Case {
        desc,
        min,
        max,
        expected,
    };
    let cases = [
        case(
            "at most 576x400 keeps only the smallest",
            None,
            Some(Resolution::new(576, 400)),
            &["color_patches.png"],
        ),
        case(
            "the same limit written in portrait order means the same thing",
            None,
            Some(Resolution::new(400, 576)),
            &["color_patches.png"],
        ),
        case(
            "at most 600x400 also admits the portrait, whose long edge fits",
            None,
            Some(Resolution::new(600, 400)),
            &["color_patches.png", "portrait.png"],
        ),
        case(
            "at least 600x400 drops the smallest, keeps both orientations",
            Some(Resolution::new(600, 400)),
            None,
            &["meta_test.jpg", "portrait.png"],
        ),
        case(
            "a min and a max together bracket a single photo",
            Some(Resolution::new(600, 400)),
            Some(Resolution::new(600, 400)),
            &["portrait.png"],
        ),
    ];

    for Case {
        desc,
        min,
        max,
        expected,
    } in cases
    {
        let filter = SearchFilter {
            resolution_min: min,
            resolution_max: max,
            ..Default::default()
        };
        let mut got: Vec<String> = lib
            .search(&filter, SortOrder::default())
            .unwrap()
            .into_iter()
            .filter_map(|r| r.original_filename)
            .collect();
        got.sort();
        assert_eq!(got, expected, "{desc}");
    }
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

/// A batch of collections goes in one run, reporting as it goes and stopping
/// when asked.  The GUI drives this from a worker thread, so the progress and
/// the cancel flag are what the user sees and what they can do about it.
#[test]
fn delete_collections_reports_progress_and_can_be_stopped() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo_id = lib.all_photos(SortOrder::default()).unwrap()[0].id;

    let ids: Vec<_> = ["Portfolio", "Prints", "Archive"]
        .iter()
        .map(|name| lib.create_collection(name).unwrap().id)
        .collect();
    // The photo is in one of them, so at least one delete has to reach a
    // member file rather than the index alone.
    lib.add_to_collection(ids[0], &[photo_id]).unwrap();

    let seen = std::sync::Mutex::new(Vec::new());
    let outcome = lib
        .delete_collections(&ids[..2], no_cancel(), |p| {
            seen.lock().unwrap().push((p.done, p.total))
        })
        .unwrap();

    assert_eq!(outcome.done, 2);
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert!(!outcome.cancelled);
    assert_eq!(
        seen.lock().unwrap().last().copied(),
        Some((2, 2)),
        "the last report is the finished tally"
    );
    let left: Vec<String> = lib
        .all_collections()
        .unwrap()
        .into_iter()
        .map(|row| row.name)
        .collect();
    assert_eq!(left, ["Archive"], "only the untouched collection is left");
    assert_eq!(
        lib.all_photos(SortOrder::default()).unwrap().len(),
        1,
        "the photo stays in the library"
    );

    // A flag already raised stops the run before it deletes anything.
    let outcome = lib
        .delete_collections(&ids[2..], Arc::new(AtomicBool::new(true)), |_| {})
        .unwrap();
    assert!(outcome.cancelled && outcome.done == 0);
    assert_eq!(lib.all_collections().unwrap().len(), 1);
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

/// The point of the move: one pass over each `.rlab` leaves the photo in the
/// collection it was sent to and out of the ones it was in, in the file and in
/// the index alike.
#[test]
fn moving_photos_takes_them_out_of_their_other_collections() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path(), png_path()], |_| {})
        .unwrap();
    let photos = lib.all_photos(SortOrder::default()).unwrap();
    let ids: Vec<_> = photos.iter().map(|row| row.id).collect();
    let portfolio = lib.create_collection("Portfolio").unwrap();
    let prints = lib.create_collection("Prints").unwrap();
    let archive = lib.create_collection("Archive").unwrap();
    lib.add_to_collection(portfolio.id, &ids).unwrap();
    lib.add_to_collection(prints.id, &ids[..1]).unwrap();

    lib.move_to_collection(archive.id, &ids).unwrap();

    assert_eq!(lib.collection_photos(archive.id).unwrap().len(), 2);
    assert!(
        lib.collection_photos(portfolio.id).unwrap().is_empty()
            && lib.collection_photos(prints.id).unwrap().is_empty(),
        "a moved photo is still listed where it came from"
    );
    for photo in &photos {
        let lmta = rasterlab_core::project::RlabFile::read(&lib.rlab_path(&photo.hash))
            .unwrap()
            .lmta
            .unwrap();
        assert_eq!(
            lmta.collection_refs
                .iter()
                .map(|held| held.name.as_str())
                .collect::<Vec<_>>(),
            ["Archive"],
            "the file disagrees with the index about where the photo now lives"
        );
    }
}

/// A move to where the photo already exclusively lives is no work at all: the
/// file must not be rewritten, or re-filing an overlapping selection would
/// churn the mtime of every photo that was already in the right place.
#[test]
fn moving_a_photo_that_is_already_only_there_rewrites_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let archive = lib.create_collection("Archive").unwrap();
    lib.move_to_collection(archive.id, &[photo.id]).unwrap();
    let written = std::fs::metadata(lib.rlab_path(&photo.hash))
        .unwrap()
        .modified()
        .unwrap();

    lib.move_to_collection(archive.id, &[photo.id]).unwrap();

    assert_eq!(lib.collection_photos(archive.id).unwrap().len(), 1);
    assert_eq!(
        std::fs::metadata(lib.rlab_path(&photo.hash))
            .unwrap()
            .modified()
            .unwrap(),
        written,
        "a file that already listed only the target was rewritten"
    );
}

/// Pre-uuid membership is a collection the photo is in as far as the file is
/// concerned, so a move has to take it out of that one too.  Left behind, the
/// name would put the photo back in a collection it was moved out of the next
/// time the index was rebuilt from the files.
#[test]
fn moving_a_photo_clears_the_collection_names_it_predates_uuids_with() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let path = lib.rlab_path(&photo.hash);

    // Both shapes at once: a name the index knows, and one it does not.
    let mut rlab = rasterlab_core::project::RlabFile::read(&path).unwrap();
    let mut lmta = rlab.lmta.clone().unwrap();
    lmta.legacy_collections = vec!["Portfolio".to_owned(), "Forgotten".to_owned()];
    rlab.set_lmta(Some(lmta));
    rlab.write_v5(&path).unwrap();
    let portfolio = lib.create_collection("Portfolio").unwrap();
    let archive = lib.create_collection("Archive").unwrap();

    lib.move_to_collection(archive.id, &[photo.id]).unwrap();

    let lmta = rasterlab_core::project::RlabFile::read(&path)
        .unwrap()
        .lmta
        .unwrap();
    assert_eq!(
        lmta.collection_refs
            .iter()
            .map(|held| held.name.as_str())
            .collect::<Vec<_>>(),
        ["Archive"]
    );
    assert!(
        lmta.legacy_collections.is_empty(),
        "a moved photo still names a collection it was moved out of"
    );

    assert!(
        lib.collection_photos(portfolio.id).unwrap().is_empty(),
        "migrating the name on the way through must not file the photo there"
    );

    lib.rebuild_index(Arc::new(AtomicBool::new(false)), |_| {})
        .expect("rebuild_index");
    let names: Vec<String> = lib
        .all_collections()
        .unwrap()
        .into_iter()
        .filter(|row| !lib.collection_photos(row.id).unwrap().is_empty())
        .map(|row| row.name)
        .collect();
    assert_eq!(
        names,
        ["Archive"],
        "a rebuild put the photo back where it had been moved out of"
    );
}

/// A stopped move is the case where the index has the most to keep straight:
/// unlike an add, it touches collections the user never named.  What it filed
/// before it stopped must be filed everywhere, and what it did not reach must
/// be left exactly as it was.
#[test]
fn a_stopped_move_leaves_the_photos_it_did_not_reach_alone() {
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
    let portfolio = lib.create_collection("Portfolio").unwrap();
    let archive = lib.create_collection("Archive").unwrap();
    lib.add_to_collection(portfolio.id, &ids).unwrap();

    // Raised as the first photo is reported, so the second is never written.
    let cancel = no_cancel();
    let flag = cancel.clone();
    let outcome = lib
        .change_collection_membership(
            archive.id,
            &ids,
            MembershipChange::Move,
            cancel,
            move |_| {
                flag.store(true, Ordering::Relaxed);
            },
        )
        .unwrap();

    assert!(outcome.cancelled);
    assert_eq!(outcome.done, 1);
    assert_eq!(
        lib.collection_photos(archive.id).unwrap().len(),
        1,
        "the photo that was written is not in the collection it was moved to"
    );
    assert_eq!(
        lib.collection_photos(portfolio.id).unwrap().len(),
        1,
        "the photo the run never reached must keep the collection it was in"
    );
}

/// A photo waiting in Recently Deleted keeps its membership rows so a restore
/// can put it back where it was.  A move has to clear them anyway: its file no
/// longer records the old collections, so a restore would otherwise bring back
/// a membership nothing on disk agrees with.
#[test]
fn moving_a_deleted_photo_clears_the_memberships_a_restore_would_use() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());

    lib.import_files(&[jpeg_path()], |_| {}).unwrap();
    let photo = lib.all_photos(SortOrder::default()).unwrap()[0].clone();
    let portfolio = lib.create_collection("Portfolio").unwrap();
    let archive = lib.create_collection("Archive").unwrap();
    lib.add_to_collection(portfolio.id, &[photo.id]).unwrap();
    lib.delete_photo(photo.id)
        .expect("move to Recently Deleted");

    lib.move_to_collection(archive.id, &[photo.id]).unwrap();

    // The file has to be the one in Recently Deleted: a move that quietly
    // skipped it would still look right in the index until the restore.
    assert_eq!(
        rasterlab_core::project::RlabFile::read(&lib.recently_deleted_path(&photo.hash))
            .unwrap()
            .lmta
            .unwrap()
            .collection_refs
            .iter()
            .map(|held| held.name.as_str())
            .collect::<Vec<_>>(),
        ["Archive"],
        "the deleted photo's file was not moved with it"
    );

    lib.restore_photo(photo.id).expect("restore");

    assert_eq!(lib.collection_photos(archive.id).unwrap().len(), 1);
    assert!(
        lib.collection_photos(portfolio.id).unwrap().is_empty(),
        "a restored photo came back to a collection it had been moved out of"
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

// ── Bulk Recently Deleted operations ──────────────────────────────────────────
//
// The single-photo entry points above go through the same code, so what these
// cover is what only the bulk form has: that one bad photo does not decide the
// fate of the rest, that progress is reported per photo, and that a run stopped
// partway leaves a library in a state it is happy in.

/// Import three distinct photos and hand back their rows in import order.
fn import_three(lib: &Library) -> Vec<PhotoRow> {
    let sources = [
        jpeg_path(),
        png_path(),
        test_images_dir().join("hue_wheel.png"),
    ];
    let session = lib.import_files(&sources, |_| {}).expect("import_files");
    assert_eq!(session.photo_count, 3, "{:?}", session.errors);
    let mut rows = lib.all_photos(SortOrder::default()).unwrap();
    rows.sort_by_key(|row| row.id);
    rows
}

#[test]
fn a_bulk_move_leaves_protected_photos_and_moves_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    let rows = import_three(&lib);
    lib.set_protected(rows[1].id, true).unwrap();
    let ids: Vec<PhotoId> = rows.iter().map(|row| row.id).collect();

    let outcome = lib
        .move_to_recently_deleted(&ids, no_cancel(), |_| {})
        .expect("move_to_recently_deleted");

    assert_eq!(outcome.done, 2);
    assert_eq!(outcome.protected.len(), 1, "{:?}", outcome.protected);
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert!(!outcome.cancelled);
    assert_eq!(lib.recently_deleted().unwrap().len(), 2);
    assert!(
        lib.rlab_path(&rows[1].hash).exists(),
        "the protected photo must still be where an active photo lives"
    );
}

#[test]
fn a_photo_the_index_does_not_know_is_reported_without_stopping_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    let rows = import_three(&lib);
    let ids = vec![rows[0].id, PhotoId::MAX, rows[2].id];

    let outcome = lib
        .move_to_recently_deleted(&ids, no_cancel(), |_| {})
        .expect("move_to_recently_deleted");

    assert_eq!(outcome.done, 2, "the two real photos must still move");
    assert_eq!(outcome.errors.len(), 1, "{:?}", outcome.errors);
    assert!(
        outcome.errors[0].1.contains("not in the library index"),
        "unhelpful report: {:?}",
        outcome.errors[0]
    );
}

#[test]
fn a_bulk_move_reports_progress_before_every_photo_and_once_at_the_end() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    let ids: Vec<PhotoId> = import_three(&lib).iter().map(|row| row.id).collect();

    let seen = Mutex::new(Vec::new());
    lib.move_to_recently_deleted(&ids, no_cancel(), |progress| {
        seen.lock().unwrap().push((progress.done, progress.total));
    })
    .expect("move_to_recently_deleted");

    assert_eq!(
        seen.into_inner().unwrap(),
        vec![(0, 3), (1, 3), (2, 3), (3, 3)]
    );
}

#[test]
fn a_cancelled_bulk_move_keeps_what_it_had_already_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    let ids: Vec<PhotoId> = import_three(&lib).iter().map(|row| row.id).collect();

    // Raised on the report that precedes the first photo, so exactly one photo
    // is moved and the check before the second one stops the run.
    let cancel = no_cancel();
    let raise = cancel.clone();
    let outcome = lib
        .move_to_recently_deleted(&ids, cancel, move |_| raise.store(true, Ordering::Relaxed))
        .expect("move_to_recently_deleted");

    assert!(outcome.cancelled);
    assert_eq!(outcome.done, 1);
    assert_eq!(lib.recently_deleted().unwrap().len(), 1);
    assert_eq!(
        lib.all_photos(SortOrder::default()).unwrap().len(),
        2,
        "the photos it never reached must still be active"
    );
}

#[test]
fn a_bulk_restore_brings_every_photo_back() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    let rows = import_three(&lib);
    let ids: Vec<PhotoId> = rows.iter().map(|row| row.id).collect();
    lib.move_to_recently_deleted(&ids, no_cancel(), |_| {})
        .unwrap();

    let outcome = lib
        .restore_photos(&ids, no_cancel(), |_| {})
        .expect("restore_photos");

    assert_eq!(outcome.done, 3);
    assert!(outcome.errors.is_empty(), "{:?}", outcome.errors);
    assert!(lib.recently_deleted().unwrap().is_empty());
    for row in &rows {
        assert!(lib.rlab_path(&row.hash).exists());
    }
}

#[test]
fn purging_recently_deleted_names_the_hashes_it_erased() {
    let tmp = tempfile::tempdir().unwrap();
    let lib = open_library(tmp.path());
    let rows = import_three(&lib);
    let ids: Vec<PhotoId> = rows.iter().map(|row| row.id).collect();
    lib.move_to_recently_deleted(&ids, no_cancel(), |_| {})
        .unwrap();

    let outcome = lib
        .purge_recently_deleted(None, no_cancel(), |_| {})
        .expect("purge_recently_deleted");

    assert_eq!(outcome.done, 3);
    let mut purged = outcome.purged.clone();
    purged.sort();
    let mut expected: Vec<String> = rows.iter().map(|row| row.hash.clone()).collect();
    expected.sort();
    assert_eq!(
        purged, expected,
        "the caller drops its cached thumbnails from this list"
    );

    assert!(lib.recently_deleted().unwrap().is_empty());
    assert!(lib.all_sessions().unwrap().is_empty());
    for row in &rows {
        assert!(!lib.recently_deleted_path(&row.hash).exists());
        assert!(!lib.thumb_path(&row.hash).exists());
    }
}

/// A path that is not a library must fail rather than quietly become one.
///
/// The GUI opens recent libraries and the last session's library by path, and
/// a drive that is not plugged in looks exactly like a directory that is not
/// there.  Creating one on the spot hides that, and on a mount point it writes
/// the new library into the directory the real one mounts over.
#[test]
fn open_existing_refuses_a_path_that_is_no_longer_a_library() {
    let tmp = tempfile::tempdir().unwrap();
    let gone = tmp.path().join("Main Library");

    // `Library` is not Debug, so the failures are matched rather than unwrapped.
    let Err(error) = Library::open_existing(&gone) else {
        panic!("a missing library must not open");
    };
    assert!(
        error.downcast_ref::<NotALibrary>().is_some(),
        "callers tell this apart from a broken library by its type: {error}"
    );
    assert!(!gone.exists(), "the failed open left a library behind");

    // An empty directory where a library used to be — an unmounted volume's
    // mount point — is the same answer.
    std::fs::create_dir(&gone).unwrap();
    let Err(error) = Library::open_existing(&gone) else {
        panic!("an empty directory is not a library");
    };
    assert!(error.downcast_ref::<NotALibrary>().is_some(), "{error}");
    assert!(
        gone.read_dir().unwrap().next().is_none(),
        "the failed open wrote into the directory"
    );

    // The real thing still opens. Dropped first: the index holds an flock.
    drop(open_library(&gone));
    assert!(
        Library::open_existing(&gone).is_ok(),
        "a real library must open by the same path"
    );
}

// ── Deleting sources ─────────────────────────────────────────────────────────

/// The source files still under `dir`, as paths relative to it and sorted, so
/// assertions do not depend on directory order.
fn remaining_sources(dir: &std::path::Path) -> Vec<String> {
    fn walk(dir: &std::path::Path, root: &std::path::Path, found: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).expect("read source dir") {
            let path = entry.expect("source entry").path();
            if path.is_dir() {
                walk(&path, root, found);
            } else {
                found.push(path.strip_prefix(root).unwrap().display().to_string());
            }
        }
    }
    let mut found = Vec::new();
    walk(dir, dir, &mut found);
    found.sort();
    found
}

fn delete_sources() -> rasterlab_library::ImportOptions {
    rasterlab_library::ImportOptions {
        collection: ImportCollection::None,
        delete_sources: true,
    }
}

/// Emptying a card is the whole point of the option: what the library took in
/// is gone from the source, and the library holds every one of them.
#[test]
fn deleting_sources_empties_what_it_imported() {
    let src = shoot_tree();
    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    let deleted = std::cell::Cell::new(0);
    lib.import_paths(
        &[src.path().to_path_buf()],
        delete_sources(),
        no_cancel(),
        |p| deleted.set(p.deleted_sources),
    )
    .unwrap();

    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 3);
    assert_eq!(deleted.get(), 3);
    assert!(
        remaining_sources(src.path()).is_empty(),
        "sources left behind: {:?}",
        remaining_sources(src.path())
    );
}

/// A file the library already had is no less imported for having arrived on an
/// earlier run, so a second pass over a half-emptied card finishes emptying it
/// rather than leaving every duplicate where it is.
#[test]
fn a_delete_run_takes_the_duplicates_too() {
    let src = shoot_tree();
    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    lib.import_paths(
        &[src.path().to_path_buf()],
        ImportCollection::None,
        no_cancel(),
        |_| {},
    )
    .unwrap();

    let last = std::cell::RefCell::new(rasterlab_library::ImportProgress::default());
    lib.import_paths(
        &[src.path().to_path_buf()],
        delete_sources(),
        no_cancel(),
        |p| {
            if !p.scanning {
                *last.borrow_mut() = p;
            }
        },
    )
    .unwrap();

    let last = last.into_inner();
    assert_eq!(last.imported, 0, "the second run imported something new");
    assert_eq!(last.skipped_duplicates, 3);
    assert_eq!(last.deleted_sources, 3);
    assert!(last.errors.is_empty(), "{:?}", last.errors);
    assert!(remaining_sources(src.path()).is_empty());
    assert_eq!(lib.all_photos(SortOrder::default()).unwrap().len(), 3);
}

/// A photo can be in the index while its file is not — a library restored
/// without its `files/`, a mount that dropped out. The source in front of us is
/// then the only copy left, and deleting it on the index's word alone would
/// lose the photograph.
#[test]
fn a_source_is_kept_when_the_library_has_no_file_for_it() {
    let src = shoot_tree();
    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    lib.import_paths(
        &[src.path().to_path_buf()],
        ImportCollection::None,
        no_cancel(),
        |_| {},
    )
    .unwrap();
    for row in lib.all_photos(SortOrder::default()).unwrap() {
        std::fs::remove_file(lib.rlab_path(&row.hash)).unwrap();
    }

    let last = std::cell::RefCell::new(rasterlab_library::ImportProgress::default());
    lib.import_paths(
        &[src.path().to_path_buf()],
        delete_sources(),
        no_cancel(),
        |p| {
            if !p.scanning {
                *last.borrow_mut() = p;
            }
        },
    )
    .unwrap();

    let last = last.into_inner();
    assert_eq!(last.deleted_sources, 0);
    assert_eq!(last.errors.len(), 3, "kept sources go unreported");
    assert_eq!(
        remaining_sources(src.path()),
        vec![
            "Harbour/shot.png".to_string(),
            "Sunrise/shot.png".into(),
            "loose.png".into(),
        ]
    );
}

/// A file that failed to import is not in the library, whatever the run was
/// asked to do with the files that are.
#[test]
fn a_failed_import_keeps_its_source() {
    let src = shoot_tree();
    let broken = src.path().join("broken.png");
    std::fs::write(&broken, b"not a png").unwrap();
    let tmp_lib = tempfile::tempdir().unwrap();
    let lib = open_library(tmp_lib.path());

    let last = std::cell::RefCell::new(rasterlab_library::ImportProgress::default());
    lib.import_paths(
        &[src.path().to_path_buf()],
        delete_sources(),
        no_cancel(),
        |p| {
            if !p.scanning {
                *last.borrow_mut() = p;
            }
        },
    )
    .unwrap();

    let last = last.into_inner();
    assert_eq!(last.imported, 3);
    assert_eq!(last.deleted_sources, 3);
    assert_eq!(last.errors.len(), 1);
    assert_eq!(
        remaining_sources(src.path()),
        vec!["broken.png".to_string()]
    );
}
