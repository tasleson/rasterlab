use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Add film-grain noise to the image.
///
/// Grain is applied as a luminance-only perturbation (same delta added to
/// R, G and B), weighted so it is strongest in the midtones and falls off
/// toward pure black and pure white — mimicking how silver-halide grain
/// distributes on real film.
///
/// The grain pattern is fully deterministic: given the same `strength`,
/// `size` and `seed` the result is always identical, which is important
/// for undo/redo and pipeline caching.
///
/// * `strength` — maximum grain amplitude as a fraction of 255.
///   `0.05` ≈ barely visible; `0.25` ≈ very heavy push-processed grain.
/// * `size`     — grain cell size in pixels (`1.0` = per-pixel noise,
///   `4.0` = coarse clumped grain).
/// * `seed`     — RNG seed; change to get a different pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrainOp {
    pub strength: f32,
    pub size: f32,
    pub seed: u64,
}

impl GrainOp {
    pub fn new(strength: f32, size: f32, seed: u64) -> Self {
        Self {
            strength: strength.clamp(0.0, 1.0),
            size: size.clamp(1.0, 32.0),
            seed,
        }
    }
}

/// Deterministic hash-based grain sample for cell `(gx, gy)`.
/// Returns a value in `[-1.0, 1.0]` with good statistical distribution.
#[inline]
fn grain_sample(gx: u32, gy: u32, seed: u64) -> f32 {
    let mut h: u64 = (gx as u64)
        .wrapping_mul(0x9e3779b97f4a7c15u64)
        .wrapping_add((gy as u64).wrapping_mul(0x6c62272e07bb0142u64))
        .wrapping_add(seed);
    // MurmurHash3 64-bit finalizer
    h ^= h >> 30;
    h = h.wrapping_mul(0xbf58476d1ce4e5b9u64);
    h ^= h >> 27;
    h = h.wrapping_mul(0x94d049bb133111ebu64);
    h ^= h >> 31;
    // Map to [-1, 1]
    (h >> 1) as f32 / (i64::MAX as f32)
}

#[typetag::serde]
impl Operation for GrainOp {
    fn name(&self) -> &'static str {
        "grain"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        let w = image.width as usize;
        let seed = self.seed;
        let strength = self.strength;
        let size = self.size;

        image
            .data
            .par_chunks_mut(w * 4)
            .enumerate()
            .for_each(|(y, row)| {
                let cell_y = (y as f32 / size) as u32;
                for x in 0..w {
                    let cell_x = (x as f32 / size) as u32;
                    let raw = grain_sample(cell_x, cell_y, seed);

                    let off = x * 4;
                    let r = row[off] as f32;
                    let g = row[off + 1] as f32;
                    let b = row[off + 2] as f32;

                    // BT.709 luma in [0, 1]
                    let luma = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0;
                    // Midtone weighting: peaks at luma=0.5, zero at 0 and 1.
                    let weight = 4.0 * luma * (1.0 - luma);

                    let delta = raw * strength * 255.0 * weight;
                    row[off] = (r + delta).clamp(0.0, 255.0) as u8;
                    row[off + 1] = (g + delta).clamp(0.0, 255.0) as u8;
                    row[off + 2] = (b + delta).clamp(0.0, 255.0) as u8;
                    // alpha unchanged
                }
            });

        Ok(image)
    }

    fn describe(&self) -> String {
        format!("Grain  {:.0}%  sz={:.1}", self.strength * 100.0, self.size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey_image(v: u8) -> Image {
        let mut img = Image::new(8, 8);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = v;
            p[1] = v;
            p[2] = v;
            p[3] = 255;
        });
        img
    }

    #[test]
    fn zero_strength_is_identity() {
        let src = grey_image(128);
        let out = GrainOp::new(0.0, 1.0, 0).apply(src.deep_clone()).unwrap();
        assert_eq!(out.data, src.data);
    }

    #[test]
    fn black_pixels_unaffected() {
        // Midtone weight = 0 at luma=0 → pure black stays black.
        let src = grey_image(0);
        let out = GrainOp::new(1.0, 1.0, 0).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| {
            assert_eq!(p[0], 0);
            assert_eq!(p[1], 0);
            assert_eq!(p[2], 0);
        });
    }

    #[test]
    fn white_pixels_unaffected() {
        // Midtone weight = 0 at luma=1 → pure white stays white.
        let src = grey_image(255);
        let out = GrainOp::new(1.0, 1.0, 0).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| {
            assert_eq!(p[0], 255);
            assert_eq!(p[1], 255);
            assert_eq!(p[2], 255);
        });
    }

    #[test]
    fn midtones_are_perturbed() {
        let src = grey_image(128);
        let src_data = src.data.clone();
        let out = GrainOp::new(0.5, 1.0, 0).apply(src).unwrap();
        // At least some pixels should differ from the source.
        assert!(src_data != out.data);
    }

    #[test]
    fn deterministic() {
        let src = grey_image(128);
        let a = GrainOp::new(0.2, 1.0, 42).apply(src.deep_clone()).unwrap();
        let b = GrainOp::new(0.2, 1.0, 42).apply(src).unwrap();
        assert_eq!(a.data, b.data);
    }

    #[test]
    fn different_seeds_differ() {
        let src = grey_image(128);
        let a = GrainOp::new(0.2, 1.0, 0).apply(src.deep_clone()).unwrap();
        let b = GrainOp::new(0.2, 1.0, 1).apply(src).unwrap();
        assert_ne!(a.data, b.data);
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 128;
            p[1] = 128;
            p[2] = 128;
            p[3] = 77;
        });
        let out = GrainOp::new(0.5, 1.0, 0).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 77));
    }
}
