use std::sync::OnceLock;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use rasterlab_core::{
    image::Image,
    ops::{
        BlackAndWhiteOp, CropOp, HealOp, HealSpot, NoiseReductionOp, NrMethod, RotateOp, SepiaOp,
        SharpenOp, WhiteBalanceOp, clarity_texture::ClarityTextureOp, histogram::HistogramData,
        split_tone::SplitToneOp,
    },
    traits::operation::Operation,
};
use rayon::prelude::*;

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

fn lut_invert() -> [u8; 256] {
    let mut lut = [0u8; 256];
    for (i, entry) in lut.iter_mut().enumerate() {
        *entry = 255 - i as u8;
    }
    lut
}

fn apply_lut_pixel_tasks(data: &mut [u8], lut: &[u8; 256]) {
    data.par_chunks_mut(4).for_each(|p| {
        p[0] = lut[p[0] as usize];
        p[1] = lut[p[1] as usize];
        p[2] = lut[p[2] as usize];
    });
}

fn apply_lut_row_tasks(data: &mut [u8], width: usize, lut: &[u8; 256]) {
    data.par_chunks_mut(width * 4).for_each(|row| {
        for p in row.chunks_exact_mut(4) {
            p[0] = lut[p[0] as usize];
            p[1] = lut[p[1] as usize];
            p[2] = lut[p[2] as usize];
        }
    });
}

fn bench_crop(c: &mut Criterion) {
    init_rayon();
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
    init_rayon();
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
    init_rayon();
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
    init_rayon();
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
    init_rayon();
    let img = make_image(4000, 3000);
    c.bench_function("bw_luminance 4000x3000", |b| {
        b.iter_batched(
            || img.deep_clone(),
            |i| BlackAndWhiteOp::luminance().apply(i).unwrap(),
            BatchSize::LargeInput,
        )
    });
}

fn bench_split_tone(c: &mut Criterion) {
    init_rayon();
    let img = make_image(4000, 3000);
    c.bench_function("split_tone 4000x3000", |b| {
        b.iter_batched(
            || img.deep_clone(),
            |i| SplitToneOp::default().apply(i).unwrap(),
            BatchSize::LargeInput,
        )
    });
}

fn bench_clarity_texture(c: &mut Criterion) {
    init_rayon();
    let img = make_image(4000, 3000);
    for (clarity, texture) in [(1.0_f32, 0.0_f32), (0.0, 1.0), (1.0, 1.0)] {
        c.bench_with_input(
            BenchmarkId::new(
                "clarity_texture 4000x3000",
                format!("clarity={clarity} texture={texture}"),
            ),
            &(clarity, texture),
            |b, &(cl, tx)| {
                b.iter_batched(
                    || img.deep_clone(),
                    |i| ClarityTextureOp::new(cl, tx).apply(i).unwrap(),
                    BatchSize::LargeInput,
                )
            },
        );
    }
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
    init_rayon();
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

fn bench_heal(c: &mut Criterion) {
    init_rayon();
    let img = make_image(2000, 1500);
    let spots = vec![
        HealSpot {
            dest_x: 500,
            dest_y: 400,
            src_x: 600,
            src_y: 400,
            radius: 30,
        },
        HealSpot {
            dest_x: 1000,
            dest_y: 700,
            src_x: 1100,
            src_y: 700,
            radius: 40,
        },
        HealSpot {
            dest_x: 1500,
            dest_y: 1100,
            src_x: 1600,
            src_y: 1100,
            radius: 25,
        },
    ];
    let op = HealOp::new(spots);
    c.bench_function("heal 3 spots r=25-40 on 2000x1500", |b| {
        b.iter_batched(
            || img.deep_clone(),
            |i| op.apply(i).unwrap(),
            BatchSize::LargeInput,
        )
    });
}

fn bench_noise_reduction(c: &mut Criterion) {
    init_rayon();
    let img = make_image(2000, 1500);

    let wavelet_op = NoiseReductionOp {
        method: NrMethod::Wavelet,
        luma_strength: 0.4,
        color_strength: 0.6,
        detail_preservation: 0.5,
    };
    c.bench_function("nr_wavelet 2000x1500", |b| {
        b.iter_batched(
            || img.deep_clone(),
            |i| wavelet_op.apply(i).unwrap(),
            BatchSize::LargeInput,
        )
    });

    // NLM is slower — use a smaller image for the bench
    let small = make_image(800, 600);
    let nlm_op = NoiseReductionOp {
        method: NrMethod::NonLocalMeans,
        luma_strength: 0.3,
        color_strength: 0.4,
        detail_preservation: 0.5,
    };
    c.bench_function("nr_nlm 800x600", |b| {
        b.iter_batched(
            || small.deep_clone(),
            |i| nlm_op.apply(i).unwrap(),
            BatchSize::SmallInput,
        )
    });
}

fn bench_row_parallel_granularity(c: &mut Criterion) {
    init_rayon();
    let img = make_image(4000, 3000);
    let lut = lut_invert();
    let mut group = c.benchmark_group("row_parallel_granularity 4000x3000 lut");

    group.bench_function("old_pixel_tasks", |b| {
        b.iter_batched(
            || img.data.clone(),
            |mut data| apply_lut_pixel_tasks(&mut data, &lut),
            BatchSize::LargeInput,
        )
    });

    group.bench_function("new_row_tasks", |b| {
        b.iter_batched(
            || img.data.clone(),
            |mut data| apply_lut_row_tasks(&mut data, img.width as usize, &lut),
            BatchSize::LargeInput,
        )
    });

    group.finish();
}

fn bench_white_balance(c: &mut Criterion) {
    init_rayon();
    let img = make_image(4000, 3000);
    c.bench_function("white_balance 4000x3000 temp=0.5 tint=0.2", |b| {
        b.iter_batched(
            || img.deep_clone(),
            |i| WhiteBalanceOp::new(0.5, 0.2).apply(i).unwrap(),
            BatchSize::LargeInput,
        )
    });
}

fn bench_sepia(c: &mut Criterion) {
    init_rayon();
    let img = make_image(4000, 3000);
    c.bench_function("sepia 4000x3000 strength=1.0", |b| {
        b.iter_batched(
            || img.deep_clone(),
            |i| SepiaOp::new(1.0).apply(i).unwrap(),
            BatchSize::LargeInput,
        )
    });
}

criterion_group!(
    benches,
    bench_crop,
    bench_rotate_90,
    bench_rotate_arbitrary,
    bench_sharpen,
    bench_bw,
    bench_row_parallel_granularity,
    bench_split_tone,
    bench_clarity_texture,
    bench_histogram,
    bench_image_to_egui,
    bench_heal,
    bench_noise_reduction,
    bench_white_balance,
    bench_sepia,
);
criterion_main!(benches);
