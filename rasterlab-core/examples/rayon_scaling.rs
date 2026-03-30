/// Benchmark: rayon thread-count scaling for the sepia operation.
///
/// Usage:
///   cargo run --release --example rayon_scaling -- <image_path>
///
/// For each thread count 1..=16 the sepia operation is applied to the loaded
/// image inside a rayon `ThreadPool` built with exactly that many threads.
/// The total wall-clock time and per-iteration average are printed.
use std::{
    env,
    path::PathBuf,
    time::{Duration, Instant},
};

use rasterlab_core::{formats::FormatRegistry, ops::sepia::SepiaOp, traits::operation::Operation};
use rayon::ThreadPoolBuilder;

const ITERATIONS: u32 = 20;
const STRENGTH: f32 = 1.0;

fn main() {
    let path: PathBuf = env::args().nth(1).map(PathBuf::from).unwrap_or_else(|| {
        eprintln!("Usage: rayon_scaling <image_path>");
        std::process::exit(1);
    });

    let registry = FormatRegistry::with_builtins();
    let base_image = registry.decode_file(&path).expect("failed to load image");

    println!(
        "Image: {}  ({}×{}, {} pixels, {} MiB RGBA)",
        path.display(),
        base_image.width,
        base_image.height,
        base_image.width as u64 * base_image.height as u64,
        base_image.data.len() / (1024 * 1024),
    );
    println!("Sepia strength: {STRENGTH:.1}   Iterations per thread count: {ITERATIONS}\n");

    println!(
        "{:>7}  {:>12}  {:>12}  {:>10}",
        "threads", "total (ms)", "avg (ms)", "speedup"
    );
    println!("{}", "-".repeat(50));

    let op = SepiaOp::new(STRENGTH);

    let mut baseline_ms: Option<f64> = None;

    for num_threads in 1u32..=16 {
        let pool = ThreadPoolBuilder::new()
            .num_threads(num_threads as usize)
            .build()
            .expect("failed to build thread pool");

        // Warm up once outside the timer.
        pool.install(|| {
            let img = base_image.deep_clone();
            let _ = op.apply(img);
        });

        let start = Instant::now();
        pool.install(|| {
            for _ in 0..ITERATIONS {
                let img = base_image.deep_clone();
                let _ = op.apply(img);
            }
        });
        let elapsed: Duration = start.elapsed();

        let total_ms = elapsed.as_secs_f64() * 1000.0;
        let avg_ms = total_ms / ITERATIONS as f64;

        let speedup = match baseline_ms {
            None => {
                baseline_ms = Some(total_ms);
                1.00
            }
            Some(b) => b / total_ms,
        };

        println!(
            "{:>7}  {:>12.1}  {:>12.2}  {:>10.2}x",
            num_threads, total_ms, avg_ms, speedup
        );
    }
}
