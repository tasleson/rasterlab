use std::sync::OnceLock;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use rasterlab_core::{
    image::Image,
    ops::{BlackAndWhiteOp, CropOp, RotateOp, SharpenOp, histogram::HistogramData},
    traits::operation::Operation,
};

/// Ensure the rayon global pool is initialised with enough stack space for
/// histogram fold accumulators (4 × [u64; 256] = 8 KiB) before any benchmark
/// runs.  macOS secondary threads default to 512 KiB which overflows under
/// deep rayon recursion.
fn init_rayon() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .stack_size(16 * 1024 * 1024)
            .build_global()
            .expect("rayon global pool already initialised");
    });
}

fn make_image(w: u32, h: u32) -> Image {
    let data: Vec<u8> = (0..(w * h * 4)).map(|i| (i % 256) as u8).collect();
    Image::from_rgba8(w, h, data).unwrap()
}

fn bench_crop(c: &mut Criterion) {
    let img = make_image(4000, 3000);
    c.bench_function("crop 4000x3000 → 2000x1500", |b| {
        b.iter_batched(
            || img.deep_clone(),
            |i| CropOp::new(500, 500, 2000, 1500).apply(i).unwrap(),
            BatchSize::LargeInput,
        )
    });
}

fn bench_rotate_90(c: &mut Criterion) {
    let img = make_image(4000, 3000);
    c.bench_function("rotate_cw90 4000x3000", |b| {
        b.iter_batched(
            || img.deep_clone(),
            |i| RotateOp::cw90().apply(i).unwrap(),
            BatchSize::LargeInput,
        )
    });
}

fn bench_rotate_arbitrary(c: &mut Criterion) {
    let img = make_image(1000, 1000);
    for deg in [15.0_f32, 45.0, 90.0] {
        c.bench_with_input(
            BenchmarkId::new("rotate_arbitrary 1000x1000", deg),
            &deg,
            |b, &d| {
                b.iter_batched(
                    || img.deep_clone(),
                    |i| RotateOp::arbitrary(d).apply(i).unwrap(),
                    BatchSize::LargeInput,
                )
            },
        );
    }
}

fn bench_sharpen(c: &mut Criterion) {
    let img = make_image(4000, 3000);
    c.bench_function("sharpen 4000x3000 strength=1.0", |b| {
        b.iter_batched(
            || img.deep_clone(),
            |i| SharpenOp::new(1.0).apply(i).unwrap(),
            BatchSize::LargeInput,
        )
    });
}

fn bench_bw(c: &mut Criterion) {
    let img = make_image(4000, 3000);
    c.bench_function("bw_luminance 4000x3000", |b| {
        b.iter_batched(
            || img.deep_clone(),
            |i| BlackAndWhiteOp::luminance().apply(i).unwrap(),
            BatchSize::LargeInput,
        )
    });
}

fn bench_histogram(c: &mut Criterion) {
    init_rayon();
    // Histogram compute is a full read of the pixel buffer with per-pixel
    // accumulation.  Any regression here (e.g. wrong chunk size in the rayon
    // fold) shows up immediately as a 10–100× slowdown.
    let img = make_image(4000, 3000);
    c.bench_function("histogram 4000x3000", |b| {
        b.iter(|| HistogramData::compute(&img))
    });
}

/// Simulates the image_to_egui conversion in the canvas panel.
/// This is memory-bandwidth-bound; parallelising it makes it slower on
/// Apple Silicon, so any "optimisation" that touches this code path must
/// show a regression here before it ships.
fn bench_image_to_egui(c: &mut Criterion) {
    use criterion::black_box;
    let img = make_image(4000, 3000);
    c.bench_function("image_to_egui 4000x3000", |b| {
        // Mirrors the serial conversion in canvas.rs.  black_box prevents the
        // compiler from eliminating the Vec allocation as dead code.
        b.iter(|| {
            let pixels: Vec<[u8; 4]> = black_box(&img.data)
                .chunks_exact(4)
                .map(|p| [p[0], p[1], p[2], p[3]])
                .collect();
            black_box(pixels)
        })
    });
}

criterion_group!(
    benches,
    bench_crop,
    bench_rotate_90,
    bench_rotate_arbitrary,
    bench_sharpen,
    bench_bw,
    bench_histogram,
    bench_image_to_egui,
);
criterion_main!(benches);
