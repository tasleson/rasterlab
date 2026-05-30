use std::time::{Duration, Instant};

use rasterlab_core::{image::Image, ops::BrightnessContrastOp, traits::operation::Operation};
use rayon::prelude::*;

fn make_pixels(width: usize, height: usize) -> Vec<u8> {
    (0..width * height * 4).map(|i| (i % 251) as u8).collect()
}

fn apply_pixel_tasks(data: &mut [u8], lut: &[u8; 256]) {
    data.par_chunks_mut(4).for_each(|p| {
        p[0] = lut[p[0] as usize];
        p[1] = lut[p[1] as usize];
        p[2] = lut[p[2] as usize];
    });
}

fn apply_row_tasks(data: &mut [u8], width: usize, lut: &[u8; 256]) {
    data.par_chunks_mut(width * 4).for_each(|row| {
        for p in row.chunks_exact_mut(4) {
            p[0] = lut[p[0] as usize];
            p[1] = lut[p[1] as usize];
            p[2] = lut[p[2] as usize];
        }
    });
}

fn invert_lut() -> [u8; 256] {
    let mut lut = [0u8; 256];
    for (i, entry) in lut.iter_mut().enumerate() {
        *entry = 255 - i as u8;
    }
    lut
}

#[test]
fn row_parallel_transform_matches_pixel_parallel_reference() {
    let width = 257;
    let height = 131;
    let lut = invert_lut();
    let mut pixel_tasks = make_pixels(width, height);
    let mut row_tasks = pixel_tasks.clone();

    apply_pixel_tasks(&mut pixel_tasks, &lut);
    apply_row_tasks(&mut row_tasks, width, &lut);

    assert_eq!(row_tasks, pixel_tasks);
}

#[test]
fn row_parallel_ops_tolerate_empty_rows() {
    let out = BrightnessContrastOp::new(0.1, 0.2)
        .apply(Image::new(0, 8))
        .unwrap();

    assert_eq!(out.width, 0);
    assert_eq!(out.height, 8);
    assert!(out.data.is_empty());
}

#[test]
#[ignore = "wall-clock performance check; run explicitly on target hardware"]
fn row_parallel_transform_is_faster_than_pixel_parallel_reference() {
    let width = 3000;
    let height = 2000;
    let lut = invert_lut();

    let source = make_pixels(width, height);
    let mut pixel_tasks = source.clone();
    let mut row_tasks = source;

    let pixel_time = time_once(|| apply_pixel_tasks(&mut pixel_tasks, &lut));
    let row_time = time_once(|| apply_row_tasks(&mut row_tasks, width, &lut));

    eprintln!("row-level tasks: {row_time:?}; per-pixel tasks: {pixel_time:?}");

    assert_eq!(row_tasks, pixel_tasks);
    assert!(
        row_time < pixel_time,
        "row-level parallelism should beat per-pixel tasks: row={row_time:?}, pixel={pixel_time:?}"
    );
}

fn time_once(f: impl FnOnce()) -> Duration {
    let start = Instant::now();
    f();
    start.elapsed()
}
