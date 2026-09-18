/// Measures wall-clock time for each stage of the editor's Open path:
/// decode thread (file read, JPEG decode, RGB→RGBA, EXIF, orientation),
/// render thread (histogram), and main thread (downsample + texture upload).
///
/// Usage:
///   cargo run --release --example load_timing -- <image_path> [oriented_image]
///
/// The optional second argument should be the same photo with an EXIF
/// orientation of 6 (rotate 90°); the decode-time difference between the two
/// files isolates the cost of the orientation transform.
use std::{env, path::PathBuf, time::Instant};

use rayon::prelude::*;

use rasterlab_core::{
    formats::{FormatRegistry, exif_util},
    ops::{histogram::HistogramData, resize::reduce_pow2},
};

const RUNS: u32 = 5;

fn time_ms<R: std::any::Any, F: FnMut() -> R>(label: &str, mut f: F) -> f64 {
    f(); // warm up
    let t = Instant::now();
    for _ in 0..RUNS {
        std::hint::black_box(f());
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / RUNS as f64;
    println!("  {label:<48} {:>9.2} ms", ms);
    ms
}

/// Stand-in for the canvas RGBA8->Color32 conversion, serial as egui's own
/// `ColorImage::from_rgba_unmultiplied` is.  The real premultiply is a little
/// more work per pixel; this measures the memory traffic that dominates it.
fn egui_convert(data: &[u8]) -> Vec<[u8; 4]> {
    data.as_chunks::<4>()
        .0
        .iter()
        .map(|p| [p[0], p[1], p[2], p[3]])
        .collect()
}

/// The same conversion across the rayon pool, as the canvas now does it.
fn egui_convert_par(data: &[u8]) -> Vec<[u8; 4]> {
    data.par_chunks_exact(4)
        .map(|p| [p[0], p[1], p[2], p[3]])
        .collect()
}

fn main() {
    // The GUI's render thread runs with a 32 MiB stack (main.rs); match it so
    // the histogram fold cannot overflow the default 512 KiB secondary stack.
    rayon::ThreadPoolBuilder::new()
        .stack_size(32 * 1024 * 1024)
        .build_global()
        .unwrap();

    let path: PathBuf = env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        eprintln!("Usage: load_timing <image_path> [oriented_image]");
        std::process::exit(1);
    });
    let oriented_path: Option<PathBuf> = env::args().nth(2).map(PathBuf::from);
    let data = std::fs::read(&path).expect("failed to read image");

    let is_jpeg = matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("jpg" | "jpeg")
    );
    let registry = FormatRegistry::with_builtins();
    let decoded = registry.decode_file(&path).expect("failed to decode image");
    println!(
        "Image: {}  ({}×{}, {} MiB RGBA, {} MiB on disk)",
        path.display(),
        decoded.width,
        decoded.height,
        decoded.data.len() / (1024 * 1024),
        data.len() / (1024 * 1024),
    );

    println!("\n── Decode thread (rasterlab-load) ─────────────────────────────");
    time_ms("std::fs::read (whole file)", || {
        std::fs::read(&path).unwrap()
    });

    let decode_ms = time_ms("registry.decode_file (TOTAL)", || {
        registry.decode_file(&path).unwrap()
    });
    if let Some(opath) = &oriented_path {
        time_ms(
            "decode_file of oriented copy (total, incl. rotation)",
            || registry.decode_file(opath).unwrap(),
        );
    }

    if is_jpeg {
        let dimg =
            image::load_from_memory_with_format(&data, image::ImageFormat::Jpeg).expect("decode");
        time_ms("image::load  (zune-jpeg decode → Rgb8)", || {
            image::load_from_memory_with_format(&data, image::ImageFormat::Jpeg).unwrap()
        });
        if let image::DynamicImage::ImageRgb8(rgb) = dimg {
            let raw = rgb.into_raw();
            time_ms("rgb8_to_rgba8 (parallel 3→4)", || {
                exif_util::rgb8_to_rgba8(&raw)
            });
        }
        time_ms("exif_util::read_exif_from_bytes", || {
            exif_util::read_exif_from_bytes(&data)
        });
    } else {
        let _dimg =
            image::load_from_memory_with_format(&data, image::ImageFormat::Png).expect("decode");
        time_ms("image::load  (png decode)", || {
            image::load_from_memory_with_format(&data, image::ImageFormat::Png).unwrap()
        });
    }

    println!("\n── Render thread (rasterlab-render) ───────────────────────────");
    let hist = time_ms("HistogramData::compute", || {
        HistogramData::compute(&decoded)
    });

    println!("\n── Main thread (canvas texture upload) ────────────────────────");
    let level = 2;
    let reduce = time_ms(&format!("reduce_pow2 (level {level})"), || {
        reduce_pow2(&decoded, level)
    });
    let reduced = reduce_pow2(&decoded, level);
    println!(
        "    reduced to {}×{} ({} MiB)",
        reduced.width,
        reduced.height,
        reduced.data.len() / (1024 * 1024)
    );
    let upload_small = time_ms("RGBA8→Color32 (reduced buffer)", || {
        egui_convert(&reduced.data)
    });
    time_ms("RGBA8→Color32 (full-res, serial)", || {
        egui_convert(&decoded.data)
    });
    time_ms("RGBA8→Color32 (full-res, rayon)", || {
        egui_convert_par(&decoded.data)
    });

    println!("\n── Totals ─────────────────────────────────────────────────────");
    // For a fresh Open with no ops the render is a no-op on pixels: the decode
    // thread, the histogram, and the first texture upload run back to back.
    println!(
        "  decode + histogram + upload ≈ {:>9.2} ms  (→ first paint, fit-to-window)",
        decode_ms + hist + reduce + upload_small,
    );
    if let Some(opath) = &oriented_path {
        let oriented = registry
            .decode_file(opath)
            .expect("failed to decode oriented image");
        println!(
            "  oriented file decodes as {}×{} ({} MiB RGBA)",
            oriented.width,
            oriented.height,
            oriented.data.len() / (1024 * 1024),
        );
        let oriented_total = time_ms("decode_file of oriented copy (again)", || {
            registry.decode_file(opath).unwrap()
        });
        println!(
            "  orientation transform costs ≈ {:>9.2} ms  (oriented − plain decode)",
            oriented_total - decode_ms,
        );
    }
}
