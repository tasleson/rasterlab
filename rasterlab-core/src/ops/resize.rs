use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Resampling algorithm used when resizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResampleMode {
    /// Nearest-neighbour — fast, pixelated at large upscales.
    NearestNeighbour,
    /// Bilinear — smooth, good for moderate scaling.
    Bilinear,
    /// Bicubic (Catmull-Rom) — sharper than bilinear, best for downscales.
    Bicubic,
}

/// Resize the image to an explicit pixel size.
///
/// * `width` / `height` — target dimensions in pixels (minimum 1).
/// * `mode`             — resampling filter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResizeOp {
    pub width: u32,
    pub height: u32,
    pub mode: ResampleMode,
}

impl ResizeOp {
    pub fn new(width: u32, height: u32, mode: ResampleMode) -> Self {
        Self {
            width: width.max(1),
            height: height.max(1),
            mode,
        }
    }
}

// ---------------------------------------------------------------------------
// Power-of-two area reduction
// ---------------------------------------------------------------------------

/// Dimensions of `width` × `height` after `level` halvings, floored at 1×1.
pub fn level_size(width: u32, height: u32, level: u32) -> (u32, u32) {
    let step = 1u32 << level.min(31);
    (width.div_ceil(step).max(1), height.div_ceil(step).max(1))
}

/// Area-average an image down by `2^level` in a single pass.
///
/// Each destination pixel is the mean of the exact block of source pixels it
/// covers, so no source pixel is skipped and no aliasing is introduced — the
/// ideal filter for minification, and much cheaper than a general resample
/// because the block bounds are fixed. Edge blocks are partial when a
/// dimension is not a multiple of the step.
///
/// `level == 0` returns a copy.
pub fn reduce_pow2(image: &Image, level: u32) -> Image {
    if level == 0 {
        return image.deep_clone();
    }

    let step = 1usize << level.min(31);
    let (src_w, src_h) = (image.width as usize, image.height as usize);
    let (dst_w, dst_h) = level_size(image.width, image.height, level);
    let mut out = Image::new(dst_w, dst_h);
    let dst_w = dst_w as usize;

    out.data
        .par_chunks_mut(dst_w * 4)
        .enumerate()
        .for_each(|(dst_y, row)| {
            let y0 = dst_y * step;
            let y1 = (y0 + step).min(src_h);
            for (dst_x, out_px) in row.chunks_exact_mut(4).enumerate() {
                let x0 = dst_x * step;
                let x1 = (x0 + step).min(src_w);
                let mut acc = [0u32; 4];
                for y in y0..y1 {
                    let start = (y * src_w + x0) * 4;
                    for src_px in image.data[start..start + (x1 - x0) * 4].chunks_exact(4) {
                        for (a, s) in acc.iter_mut().zip(src_px) {
                            *a += u32::from(*s);
                        }
                    }
                }
                let count = ((y1 - y0) * (x1 - x0)) as u32;
                for (o, a) in out_px.iter_mut().zip(acc) {
                    *o = ((a + count / 2) / count) as u8;
                }
            }
        });

    out
}

// ---------------------------------------------------------------------------
// Pixel sampling helpers
// ---------------------------------------------------------------------------

#[inline]
fn sample_nearest(src: &Image, sx: f32, sy: f32) -> [u8; 4] {
    let x = (sx as u32).min(src.width - 1);
    let y = (sy as u32).min(src.height - 1);
    let off = (y * src.width + x) as usize * 4;
    [
        src.data[off],
        src.data[off + 1],
        src.data[off + 2],
        src.data[off + 3],
    ]
}

#[inline]
fn sample_bilinear(src: &Image, sx: f32, sy: f32) -> [u8; 4] {
    let x0 = sx.floor() as i64;
    let y0 = sy.floor() as i64;
    let fx = sx - x0 as f32;
    let fy = sy - y0 as f32;

    let w = src.width as i64;
    let h = src.height as i64;

    let p00 = get_pixel(src, x0, y0, w, h);
    let p10 = get_pixel(src, x0 + 1, y0, w, h);
    let p01 = get_pixel(src, x0, y0 + 1, w, h);
    let p11 = get_pixel(src, x0 + 1, y0 + 1, w, h);

    let mut out = [0u8; 4];
    for i in 0..4 {
        let v = p00[i] * (1.0 - fx) * (1.0 - fy)
            + p10[i] * fx * (1.0 - fy)
            + p01[i] * (1.0 - fx) * fy
            + p11[i] * fx * fy;
        out[i] = v.clamp(0.0, 255.0) as u8;
    }
    out
}

/// Fetch a clamped pixel from `src` as `[f32; 4]`.
#[inline]
fn get_pixel(src: &Image, xi: i64, yi: i64, w: i64, h: i64) -> [f32; 4] {
    let xi = xi.clamp(0, w - 1) as usize;
    let yi = yi.clamp(0, h - 1) as usize;
    let off = (yi * w as usize + xi) * 4;
    [
        src.data[off] as f32,
        src.data[off + 1] as f32,
        src.data[off + 2] as f32,
        src.data[off + 3] as f32,
    ]
}

/// Catmull-Rom cubic weight.
#[inline]
fn cubic_weight(t: f32) -> f32 {
    let t = t.abs();
    if t < 1.0 {
        1.5 * t * t * t - 2.5 * t * t + 1.0
    } else if t < 2.0 {
        -0.5 * t * t * t + 2.5 * t * t - 4.0 * t + 2.0
    } else {
        0.0
    }
}

/// Catmull-Rom bicubic sample at float coordinates `(sx, sy)`, clamped to
/// border.  Shared with [`super::align`], which resamples for the same reason a
/// resize does and wants the same kernel.
#[inline]
pub(super) fn sample_bicubic(src: &Image, sx: f32, sy: f32) -> [u8; 4] {
    let x0 = sx.floor() as i64;
    let y0 = sy.floor() as i64;
    let fx = sx - x0 as f32;
    let fy = sy - y0 as f32;

    let w = src.width as i64;
    let h = src.height as i64;

    let mut acc = [0.0f32; 4];
    for ky in -1..=2i64 {
        let wy = cubic_weight(ky as f32 - fy);
        for kx in -1..=2i64 {
            let wx = cubic_weight(kx as f32 - fx);
            let p = get_pixel(src, x0 + kx, y0 + ky, w, h);
            for i in 0..4 {
                acc[i] += p[i] * wx * wy;
            }
        }
    }
    let mut out = [0u8; 4];
    for i in 0..4 {
        out[i] = acc[i].clamp(0.0, 255.0) as u8;
    }
    out
}

// ---------------------------------------------------------------------------
// Operation
// ---------------------------------------------------------------------------

#[typetag::serde]
impl Operation for ResizeOp {
    fn name(&self) -> &'static str {
        "resize"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn apply(&self, image: Image) -> RasterResult<Image> {
        if image.width == self.width && image.height == self.height {
            return Ok(image);
        }

        let dst_w = self.width as usize;
        let x_ratio = image.width as f32 / self.width as f32;
        let y_ratio = image.height as f32 / self.height as f32;
        let mode = self.mode;
        let metadata = image.metadata.clone();

        let mut out = Image::new(self.width, self.height);

        out.data
            .par_chunks_mut(dst_w * 4)
            .enumerate()
            .for_each(|(dy, row)| {
                // Sample at centre of destination pixel.
                let sy = (dy as f32 + 0.5) * y_ratio - 0.5;
                for dx in 0..dst_w {
                    let sx = (dx as f32 + 0.5) * x_ratio - 0.5;
                    let p = match mode {
                        ResampleMode::NearestNeighbour => {
                            sample_nearest(&image, sx.max(0.0), sy.max(0.0))
                        }
                        ResampleMode::Bilinear => sample_bilinear(&image, sx, sy),
                        ResampleMode::Bicubic => sample_bicubic(&image, sx, sy),
                    };
                    let off = dx * 4;
                    row[off] = p[0];
                    row[off + 1] = p[1];
                    row[off + 2] = p[2];
                    row[off + 3] = p[3];
                }
            });

        out.metadata = metadata;

        Ok(out)
    }

    fn describe(&self) -> String {
        let mode = match self.mode {
            ResampleMode::NearestNeighbour => "NN",
            ResampleMode::Bilinear => "BL",
            ResampleMode::Bicubic => "BC",
        };
        format!("Resize  {}×{}  ({})", self.width, self.height, mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(r: u8, g: u8, b: u8, w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = r;
            p[1] = g;
            p[2] = b;
            p[3] = 255;
        });
        img
    }

    #[test]
    fn same_size_is_identity() {
        let src = solid(100, 150, 200, 16, 16);
        for mode in [
            ResampleMode::NearestNeighbour,
            ResampleMode::Bilinear,
            ResampleMode::Bicubic,
        ] {
            let out = ResizeOp::new(16, 16, mode).apply(src.deep_clone()).unwrap();
            assert_eq!(out.data, src.data, "identity failed for {:?}", mode);
        }
    }

    #[test]
    fn resize_preserves_camera_metadata() {
        let mut src = solid(100, 150, 200, 16, 16);
        src.metadata.iso = Some(400);
        src.metadata.aperture = Some(2.8);
        src.metadata.focal_length = Some(50.0);
        src.metadata.shutter_speed = Some("1/250 s".into());

        let out = ResizeOp::new(8, 8, ResampleMode::Bicubic)
            .apply(src)
            .unwrap();

        assert_eq!(out.metadata.iso, Some(400));
        assert_eq!(out.metadata.aperture, Some(2.8));
        assert_eq!(out.metadata.focal_length, Some(50.0));
        assert_eq!(out.metadata.shutter_speed.as_deref(), Some("1/250 s"));
    }

    #[test]
    fn output_dimensions_correct() {
        let src = solid(128, 128, 128, 100, 80);
        let out = ResizeOp::new(50, 40, ResampleMode::Bilinear)
            .apply(src)
            .unwrap();
        assert_eq!(out.width, 50);
        assert_eq!(out.height, 40);
    }

    #[test]
    fn uniform_colour_preserved_after_resize() {
        // Resampling a constant-colour image should produce the same colour.
        for mode in [
            ResampleMode::NearestNeighbour,
            ResampleMode::Bilinear,
            ResampleMode::Bicubic,
        ] {
            let src = solid(200, 100, 50, 32, 32);
            let out = ResizeOp::new(64, 64, mode).apply(src).unwrap();
            out.data.chunks(4).for_each(|p| {
                assert!((p[0] as i16 - 200).abs() <= 2, "R off for {:?}", mode);
                assert!((p[1] as i16 - 100).abs() <= 2, "G off for {:?}", mode);
                assert!((p[2] as i16 - 50).abs() <= 2, "B off for {:?}", mode);
            });
        }
    }

    #[test]
    fn upscale_then_downscale_roundtrip_close() {
        // Upscaling then downscaling back should stay close to the original
        // for a smooth gradient image.
        let mut src = Image::new(8, 8);
        src.data.chunks_mut(4).enumerate().for_each(|(i, p)| {
            let v = ((i % 8) * 32) as u8;
            p[0] = v;
            p[1] = v;
            p[2] = v;
            p[3] = 255;
        });
        let src_data = src.data.clone();
        let up = ResizeOp::new(16, 16, ResampleMode::Bilinear)
            .apply(src)
            .unwrap();
        let down = ResizeOp::new(8, 8, ResampleMode::Bilinear)
            .apply(up)
            .unwrap();
        for (a, b) in src_data.chunks(4).zip(down.data.chunks(4)) {
            assert!((a[0] as i16 - b[0] as i16).abs() <= 10);
        }
    }

    #[test]
    fn reduce_pow2_dimensions_round_up() {
        // Odd sizes must not lose their last row/column.
        let cases = [
            ((8, 8), 1, (4, 4)),
            ((8, 8), 3, (1, 1)),
            ((7, 3), 1, (4, 2)),
            ((7, 3), 2, (2, 1)),
            ((4899, 3266), 2, (1225, 817)),
            // Reducing past a single pixel is floored, not zeroed.
            ((3, 3), 10, (1, 1)),
        ];
        for ((w, h), level, expected) in cases {
            assert_eq!(level_size(w, h, level), expected, "{w}x{h} @ {level}");
            let out = reduce_pow2(&solid(1, 2, 3, w, h), level);
            assert_eq!((out.width, out.height), expected, "{w}x{h} @ {level}");
            assert_eq!(out.data.len(), (expected.0 * expected.1 * 4) as usize);
        }
    }

    #[test]
    fn reduce_pow2_preserves_uniform_colour() {
        let src = solid(200, 100, 50, 64, 64);
        for level in 0..=6 {
            let out = reduce_pow2(&src, level);
            for p in out.data.chunks_exact(4) {
                assert_eq!(p, [200, 100, 50, 255], "level={level}");
            }
        }
    }

    /// Every source pixel must contribute — that is the whole reason the canvas
    /// reduces on the CPU instead of letting a bilinear tap skip most of them.
    #[test]
    fn reduce_pow2_averages_the_whole_footprint() {
        let mut src = Image::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                let v = if (x + y) % 2 == 0 { 0 } else { 255 };
                src.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        let out = reduce_pow2(&src, 2);
        assert_eq!(out.data[0], 128, "checkerboard should average to mid-grey");

        // A partial edge block averages only the pixels it actually covers.
        let mut edge = Image::new(3, 1);
        edge.set_pixel(0, 0, [0, 0, 0, 255]);
        edge.set_pixel(1, 0, [100, 100, 100, 255]);
        edge.set_pixel(2, 0, [60, 60, 60, 255]);
        let out = reduce_pow2(&edge, 1);
        assert_eq!((out.width, out.height), (2, 1));
        assert_eq!(out.data[0], 50, "first block covers two pixels");
        assert_eq!(out.data[4], 60, "last block covers only one");
    }

    #[test]
    fn reduce_pow2_level_zero_is_identity() {
        let src = solid(10, 20, 30, 5, 7);
        let out = reduce_pow2(&src, 0);
        assert_eq!((out.width, out.height), (5, 7));
        assert_eq!(out.data, src.data);
    }
}
