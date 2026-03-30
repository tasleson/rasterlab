/// Measures wall-clock time for each measurable step of the GUI render
/// pipeline.  The GPU load_texture call cannot be timed here (it needs an
/// egui/wgpu context), so the output shows how much of the ~400 ms is
/// accounted for by CPU work and how much remains unexplained (= GPU upload).
///
/// Usage:
///   cargo run --release --example render_timing -- <image_path>
use std::{env, path::PathBuf, time::Instant};

use rasterlab_core::{
    formats::FormatRegistry,
    ops::{histogram::HistogramData, sepia::SepiaOp},
    traits::operation::Operation,
};

const RUNS: u32 = 5;

fn time_ms<R, F: FnMut() -> R>(label: &str, mut f: F) -> f64 {
    f(); // warm up
    let t = Instant::now();
    for _ in 0..RUNS {
        std::hint::black_box(f());
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / RUNS as f64;
    println!("  {label:<40} {:>8.2} ms", ms);
    ms
}

/// Mirrors compute_hash from canvas.rs.
fn compute_hash(data: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.len().hash(&mut h);
    for byte in data.iter().step_by(128) {
        byte.hash(&mut h);
    }
    h.finish()
}

/// Mirrors image_to_egui (serial, before fix).
/// Returns Vec<[u8;4]> — same layout as Vec<Color32>.
fn conv_serial(data: &[u8]) -> Vec<[u8; 4]> {
    data.chunks_exact(4)
        .map(|p| [p[0], p[1], p[2], 255])
        .collect()
}

/// Mirrors image_to_egui (parallel + opaque fast-path, after fix).
fn conv_parallel(data: &[u8]) -> Vec<[u8; 4]> {
    use rayon::prelude::*;
    let all_opaque = data.iter().skip(3).step_by(4).all(|&a| a == 255);
    if all_opaque {
        data.par_chunks_exact(4)
            .map(|p| [p[0], p[1], p[2], p[3]])
            .collect()
    } else {
        data.par_chunks_exact(4)
            .map(|p| [p[0], p[1], p[2], 255])
            .collect()
    }
}

fn main() {
    // Increase rayon global worker stack size before the pool is initialised.
    // The histogram fold uses 4×[u64;256] = 8 KiB accumulators per rayon
    // reduction level; the macOS default secondary-thread stack of 512 KB can
    // overflow on large images.  The real GUI render thread has 32 MiB stack
    // (app_state.rs) which keeps workers alive long enough.
    rayon::ThreadPoolBuilder::new()
        .stack_size(32 * 1024 * 1024)
        .build_global()
        .unwrap();

    let path: PathBuf = env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        eprintln!("Usage: render_timing <image_path>");
        std::process::exit(1);
    });

    let registry = FormatRegistry::with_builtins();
    let base = registry.decode_file(&path).expect("failed to load image");

    println!(
        "Image: {} ({}×{}, {} MiB RGBA)\nRuns per step: {RUNS}\n",
        path.display(),
        base.width,
        base.height,
        base.data.len() / (1024 * 1024),
    );

    let op = SepiaOp::new(1.0);

    println!("── Render thread (background) ──────────────────────────────────");

    let clone_ms = time_ms("deep_clone  136 MiB", || base.deep_clone());

    let sepia_total_ms = time_ms("SepiaOp::apply  (includes clone)", || {
        op.apply(base.deep_clone()).unwrap()
    });
    println!(
        "  {:<40} {:>8.2} ms  <- kernel only",
        "(net sepia)",
        sepia_total_ms - clone_ms
    );

    let hist_ms = time_ms("HistogramData::compute  (rayon f32)", || {
        HistogramData::compute(&base)
    });

    println!("\n── Main thread (egui frame) ────────────────────────────────────");

    let hash_ms = time_ms("compute_hash  stride-128", || compute_hash(&base.data));

    let serial_ms = time_ms("image_to_egui  SERIAL  (before fix)", || {
        conv_serial(&base.data)
    });

    let par_ms = time_ms("image_to_egui  PARALLEL+opaque  (after)", || {
        conv_parallel(&base.data)
    });

    println!("\n── Totals ──────────────────────────────────────────────────────");
    let render_thread_ms = clone_ms + (sepia_total_ms - clone_ms) + hist_ms;
    let main_cpu_ms = hash_ms + par_ms;
    let cpu_total = render_thread_ms + main_cpu_ms;
    println!(
        "  Render thread CPU subtotal     {:>8.2} ms",
        render_thread_ms
    );
    println!("  Main thread CPU (excl. GPU)    {:>8.2} ms", main_cpu_ms);
    println!(
        "  image_to_egui speedup          {:>8.2}x",
        serial_ms / par_ms
    );
    println!("  Total measured CPU             {:>8.2} ms", cpu_total);
    println!(
        "  Gap to observed 400 ms         {:>8.2} ms  <- GPU load_texture",
        400.0_f64 - cpu_total
    );
}
