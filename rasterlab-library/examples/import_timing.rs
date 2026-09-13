//! Disposable synthetic import baseline; never accepts an existing library.
//!
//! Build: cargo build --release --offline -p rasterlab-library --example import_timing
//! Run: TMPDIR="$PWD/target/import-bench-tmp" /usr/bin/time -v \
//!      target/release/examples/import_timing jpeg 100 3 256 256
//! Arguments: workload (jpeg|png|project|collection), batch size, repetitions,
//! width, height. Defaults: jpeg 10 3 256 256. Run each batch/workload in a
//! separate process; JSON lines report individual runs and min/median/max.
//! Record git revision, dirty diff, rustc version, storage and command alongside
//! results. /usr/bin/time peak RSS includes fixture generation and validation.
//! Sources are deterministic generated RGB noise, no EXIF, fixed mtime, and
//! warm OS cache (generated before timing); this is not a cold-cache or RAW
//! benchmark. Project fixtures have two unedited copies and embedded thumbnails.
//! Project imports use an explicit file list (folder discovery excludes .rlab),
//! prepared outside timing. Other workloads include folder traversal.
//! Each repeat imports into a fresh disk library, then repeats the same inputs
//! to measure unchanged duplicates. Fixture creation, opening the library,
//! validation, and cleanup are outside timing. Scan time includes directory
//! enumeration/capture-date grouping until the first non-scanning callback;
//! scan also includes sorting, stack detection and initial session lookup.
//! Remaining time includes decode, storage, DB and progress callbacks together.
//! Add `--features import-timing` to the build command for internal phase times.
//! `phase_seconds` are exclusive calling-thread wall times, not worker CPU time.
//! Import prepares on worker threads, so only the committing thread's phases —
//! thumbnail and project writes, directory creation and database work — are
//! snapshotted; overlapped reads, decoding, thumbnails and serialisation land in
//! `unattributed_seconds`. Compare `seconds`, not the phase sum.
//! Historically `phase_seconds` covered the whole serial import;
//! nested parity/writes are subtracted from serialization/project preparation.
//! Capture scanning includes prefix reads and timestamp parsing; decode includes
//! codec-internal reads/EXIF and any RAW staging. Project parsing includes original
//! extraction/copy. Thumbnail writes include directory creation and verification;
//! `verified_write` covers only project writes. `unattributed_seconds` includes
//! sorting/grouping, metadata assembly, progress, allocation teardown and timing
//! overhead. The old callback scan boundary overlaps phases and is not additive.
//! Counters reset before import and snapshot before validation on each run.

use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use rasterlab_core::{
    formats::FormatRegistry,
    pipeline::PipelineState,
    project::{RlabFile, RlabMeta, SavedCopy},
};
use rasterlab_library::{ImportCollection, Library, SortOrder, thumbnail::generate_thumbnail};
use serde_json::json;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() <= 5,
        "expected workload count repeats width height"
    );
    let workload = args.first().map(String::as_str).unwrap_or("jpeg");
    ensure!(
        matches!(workload, "jpeg" | "png" | "project" | "collection"),
        "workload must be jpeg, png, project or collection"
    );
    let number = |index: usize, default: usize| -> Result<usize> {
        args.get(index)
            .map(|s| s.parse().context("invalid positive integer"))
            .unwrap_or(Ok(default))
    };
    let count = number(1, 10)?;
    let repeats = number(2, 3)?;
    let width = u32::try_from(number(3, 256)?)?;
    let height = u32::try_from(number(4, 256)?)?;
    ensure!(
        count > 0 && repeats > 0 && width > 0 && height > 0,
        "arguments must be positive"
    );
    // Large fold accumulators used by dependent image operations need this on macOS.
    rayon::ThreadPoolBuilder::new()
        .stack_size(16 * 1024 * 1024)
        .build_global()?;
    let scratch = tempfile::Builder::new()
        .prefix("import-timing-")
        .tempdir()?;
    let source = scratch.path().join("source");
    std::fs::create_dir(&source)?;
    let fixture_start = Instant::now();
    let source_bytes = generate_fixtures(&source, workload, count, width, height)?;
    println!(
        "{}",
        json!({"type":"fixtures", "workload":workload,
        "count":count, "width":width, "height":height, "source_bytes":source_bytes,
        "fixture_seconds":fixture_start.elapsed().as_secs_f64(),
        "scratch":scratch.path(), "cache":"warm/generated", "raw":"unavailable",
        "api":if workload == "project" { "import_paths_explicit" } else { "import_folder_into_collection" },
        "debug_assertions":cfg!(debug_assertions), "phase_timing":cfg!(feature = "import-timing"), "repetitions":repeats})
    );

    let project_paths: Vec<_> = (0..count)
        .map(|index| source.join(format!("fixture-{index:06}.rlab")))
        .collect();
    let mut fresh_times = Vec::new();
    let mut duplicate_times = Vec::new();
    for repeat in 1..=repeats {
        let destination = tempfile::Builder::new()
            .prefix("library-")
            .tempdir_in(scratch.path())?;
        let library = Library::open_or_create(destination.path())?;
        for duplicate in [false, true] {
            #[cfg(feature = "import-timing")]
            rasterlab_core::import_timing::reset();
            let start = Instant::now();
            let scan_seconds = Arc::new(Mutex::new((None, 0, 0)));
            let scan_callback = Arc::clone(&scan_seconds);
            let collection = if workload == "collection" {
                ImportCollection::Named("Synthetic collection".into())
            } else {
                ImportCollection::None
            };
            let callback = move |p: rasterlab_library::ImportProgress| {
                if !p.scanning {
                    let mut progress = scan_callback.lock().unwrap();
                    progress
                        .0
                        .get_or_insert_with(|| start.elapsed().as_secs_f64());
                    progress.1 = p.skipped_duplicates;
                    progress.2 = p.done;
                }
            };
            let sessions = if workload == "project" {
                library.import_paths(
                    &project_paths,
                    collection,
                    Arc::new(AtomicBool::new(false)),
                    callback,
                )?
            } else {
                library.import_folder_into_collection(&source, collection, callback)?
            };
            let elapsed = start.elapsed().as_secs_f64();
            #[cfg(feature = "import-timing")]
            let phase_seconds = Some(rasterlab_core::import_timing::snapshot_seconds());
            #[cfg(not(feature = "import-timing"))]
            let phase_seconds: Option<std::collections::BTreeMap<&str, f64>> = None;
            let unattributed_seconds = phase_seconds
                .as_ref()
                .map(|phases| elapsed - phases.values().sum::<f64>());
            let (scan, skipped, done) = *scan_seconds.lock().unwrap();
            let scan = scan.context("no import progress callback")?;
            ensure!(
                done == count && skipped == if duplicate { count } else { 0 },
                "unexpected final progress"
            );
            for session in &sessions {
                ensure!(
                    session.errors.is_empty(),
                    "import errors: {:?}",
                    session.errors
                );
            }
            ensure!(
                sessions.iter().map(|s| s.photo_count).sum::<usize>()
                    == if duplicate { 0 } else { count },
                "unexpected imported count"
            );
            validate(&library, &source, workload, count, width, height)?;
            println!(
                "{}",
                json!({"type":"run", "workload":workload,
                "count":count, "repeat":repeat, "duplicate":duplicate,
                "seconds":elapsed, "scan_seconds":scan,
                "phase_seconds":phase_seconds, "unattributed_seconds":unattributed_seconds,
                "remaining_seconds":elapsed-scan, "files_per_second":count as f64/elapsed,
                "source_mib_per_second":source_bytes as f64/1048576.0/elapsed})
            );
            if duplicate {
                duplicate_times.push(elapsed);
            } else {
                fresh_times.push(elapsed);
            }
        }
    }
    for (duplicate, mut times) in [(false, fresh_times), (true, duplicate_times)] {
        times.sort_by(f64::total_cmp);
        let median = (times[(times.len() - 1) / 2] + times[times.len() / 2]) / 2.0;
        println!(
            "{}",
            json!({"type":"summary", "workload":workload, "count":count,
            "duplicate":duplicate, "repetitions":repeats, "min_seconds":times[0],
            "median_seconds":median, "max_seconds":times[times.len()-1]})
        );
    }
    Ok(())
}

fn generate_fixtures(
    source: &Path,
    workload: &str,
    count: usize,
    width: u32,
    height: u32,
) -> Result<u64> {
    let registry = FormatRegistry::with_builtins();
    let mut total_bytes = 0;
    let mut hashes = HashSet::new();
    for index in 0..count {
        let mut state = (index as u64).wrapping_add(1);
        let pixels = image::RgbImage::from_fn(width, height, |_, _| {
            let mut rgb = [0; 3];
            for component in &mut rgb {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *component = (state >> 32) as u8;
            }
            image::Rgb(rgb)
        });
        let mut encoded = Vec::new();
        if workload == "png" {
            image::DynamicImage::ImageRgb8(pixels).write_to(
                &mut std::io::Cursor::new(&mut encoded),
                image::ImageFormat::Png,
            )?;
        } else {
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut encoded, 90)
                .encode_image(&pixels)?;
        }
        ensure!(
            hashes.insert(blake3::hash(&encoded)),
            "fixture collision; increase dimensions"
        );
        let extension = match workload {
            "png" => "png",
            "project" => "rlab",
            _ => "jpg",
        };
        let path = source.join(format!("fixture-{index:06}.{extension}"));
        if workload == "project" {
            let decoded = registry.decode_bytes(&encoded, Some(Path::new("source.jpg")))?;
            let thumbnail = generate_thumbnail(&decoded, 512)?;
            let copies = ["Original", "Virtual copy"]
                .map(|name| SavedCopy {
                    name: name.into(),
                    pipeline_state: PipelineState {
                        entries: Vec::new(),
                        cursor: 0,
                    },
                })
                .to_vec();
            RlabFile::new(
                RlabMeta::new(
                    "import-timing",
                    Some(format!("fixture-{index:06}.jpg")),
                    width,
                    height,
                ),
                encoded,
                copies,
                1,
                Some(thumbnail),
            )
            .write_v5(&path)?;
        } else {
            std::fs::write(&path, encoded)?;
        }
        filetime::set_file_mtime(&path, filetime::FileTime::from_unix_time(1_700_000_000, 0))?;
        total_bytes += std::fs::metadata(path)?.len();
    }
    Ok(total_bytes)
}

fn validate(
    library: &Library,
    source: &Path,
    workload: &str,
    count: usize,
    width: u32,
    height: u32,
) -> Result<()> {
    let photos = library.all_photos(SortOrder::default())?;
    ensure!(photos.len() == count, "unexpected database photo count");
    let memberships = library.collection_memberships()?;
    ensure!(
        memberships.len() == if workload == "collection" { count } else { 0 },
        "incorrect collection membership count"
    );
    let unique: HashSet<_> = memberships.iter().collect();
    ensure!(unique.len() == memberships.len(), "duplicate memberships");
    let collections = library.all_collections()?;
    ensure!(
        collections.len() == usize::from(workload == "collection"),
        "unexpected collections"
    );
    for photo in photos {
        ensure!(
            photo.width == width && photo.height == height,
            "dimensions changed"
        );
        let project = RlabFile::read(&library.rlab_path(&photo.hash))?;
        ensure!(
            blake3::hash(&project.original_bytes).to_hex().as_str() == photo.hash,
            "original hash changed"
        );
        ensure!(
            library.thumb_path(&photo.hash).exists(),
            "missing thumbnail"
        );
        if workload != "project" {
            let filename = photo
                .original_filename
                .as_deref()
                .context("missing original filename")?;
            ensure!(
                project.original_bytes == std::fs::read(source.join(filename))?,
                "original bytes changed"
            );
        }
        if workload == "project" {
            let filename = photo
                .original_filename
                .as_deref()
                .context("missing original filename")?;
            let original = RlabFile::read(&source.join(filename).with_extension("rlab"))?;
            ensure!(
                project.original_bytes == original.original_bytes,
                "original bytes changed"
            );
            ensure!(
                serde_json::to_value(&project.copies)? == serde_json::to_value(&original.copies)?,
                "copies changed"
            );
            ensure!(
                project.active_copy_index == original.active_copy_index,
                "active copy changed"
            );
            ensure!(
                project.thumbnail == original.thumbnail,
                "project thumbnail changed"
            );
        }
    }
    Ok(())
}
