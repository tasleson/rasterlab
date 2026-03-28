use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use rasterlab_core::{
    image::Image,
    ops::{BlackAndWhiteOp, CropOp, RotateOp, SharpenOp},
    traits::operation::Operation,
};

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

criterion_group!(
    benches,
    bench_crop,
    bench_rotate_90,
    bench_rotate_arbitrary,
    bench_sharpen,
    bench_bw
);
criterion_main!(benches);
