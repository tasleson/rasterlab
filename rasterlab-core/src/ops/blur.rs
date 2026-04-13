use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Gaussian blur.
///
/// Uses a separable 1-D kernel applied first horizontally then vertically
/// (two passes of O(radius) work per pixel rather than O(radius²)).  The
/// kernel is built from the standard Gaussian formula and truncated at
/// ±3σ, which captures >99.7 % of the distribution.
///
/// * `radius` — standard deviation σ of the Gaussian in pixels.  `1.0`
///   is a gentle softening; `5.0` is noticeably blurry; `20.0` is very
///   heavy.  Clamped to `[0.1, 100.0]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlurOp {
    pub radius: f32,
}

impl BlurOp {
    pub fn new(radius: f32) -> Self {
        Self {
            radius: radius.clamp(0.1, 100.0),
        }
    }
}

/// Build a normalised 1-D Gaussian kernel truncated at ±3σ.
fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let half = (sigma * 3.0).ceil() as usize;
    let len = 2 * half + 1;
    let mut k: Vec<f32> = (0..len)
        .map(|i| {
            let x = i as f32 - half as f32;
            (-0.5 * (x / sigma).powi(2)).exp()
        })
        .collect();
    let sum: f32 = k.iter().sum();
    k.iter_mut().for_each(|v| *v /= sum);
    k
}

/// Horizontal 1-D convolution pass (RGBA, alpha convolved too for
/// pre-multiplied-style blending consistency).
fn blur_h(src: &Image, kernel: &[f32]) -> Image {
    let w = src.width as usize;
    let half = kernel.len() / 2;
    let mut out = Image::new(src.width, src.height);

    out.data
        .par_chunks_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                let mut r = 0.0_f32;
                let mut g = 0.0_f32;
                let mut b = 0.0_f32;
                let mut a = 0.0_f32;
                for (ki, &kv) in kernel.iter().enumerate() {
                    // Clamp-to-edge boundary handling.
                    let sx = (x + ki).saturating_sub(half).min(w - 1);
                    let off = (y * w + sx) * 4;
                    r += src.data[off] as f32 * kv;
                    g += src.data[off + 1] as f32 * kv;
                    b += src.data[off + 2] as f32 * kv;
                    a += src.data[off + 3] as f32 * kv;
                }
                let off = x * 4;
                row[off] = r.clamp(0.0, 255.0).round() as u8;
                row[off + 1] = g.clamp(0.0, 255.0).round() as u8;
                row[off + 2] = b.clamp(0.0, 255.0).round() as u8;
                row[off + 3] = a.clamp(0.0, 255.0).round() as u8;
            }
        });

    out
}

/// Vertical 1-D convolution pass writing into an existing buffer.
///
/// Reads from `src` (the H-blurred intermediate) and writes into `dst`
/// (which is the original input buffer, now free to reuse).  This avoids
/// a second allocation for the V-pass output.
fn blur_v_into(src: &Image, kernel: &[f32], dst: &mut Image) {
    let w = src.width as usize;
    let h = src.height as usize;
    let half = kernel.len() / 2;

    dst.data
        .par_chunks_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..w {
                let mut r = 0.0_f32;
                let mut g = 0.0_f32;
                let mut b = 0.0_f32;
                let mut a = 0.0_f32;
                for (ki, &kv) in kernel.iter().enumerate() {
                    let sy = (y + ki).saturating_sub(half).min(h - 1);
                    let off = (sy * w + x) * 4;
                    r += src.data[off] as f32 * kv;
                    g += src.data[off + 1] as f32 * kv;
                    b += src.data[off + 2] as f32 * kv;
                    a += src.data[off + 3] as f32 * kv;
                }
                let off = x * 4;
                row[off] = r.clamp(0.0, 255.0).round() as u8;
                row[off + 1] = g.clamp(0.0, 255.0).round() as u8;
                row[off + 2] = b.clamp(0.0, 255.0).round() as u8;
                row[off + 3] = a.clamp(0.0, 255.0).round() as u8;
            }
        });
}

#[typetag::serde]
impl Operation for BlurOp {
    fn name(&self) -> &'static str {
        "blur"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        let kernel = gaussian_kernel(self.radius);
        // H-pass: allocate one intermediate buffer.
        let h_blurred = blur_h(&image, &kernel);
        // V-pass: write back into the original `image` buffer (now free to reuse).
        blur_v_into(&h_blurred, &kernel, &mut image);
        Ok(image)
    }

    fn describe(&self) -> String {
        format!("Blur  σ={:.1}px", self.radius)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(v: u8, w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = v;
            p[1] = v;
            p[2] = v;
            p[3] = 255;
        });
        img
    }

    #[test]
    fn uniform_image_unchanged() {
        // Blurring a constant-colour image should produce the same colour.
        let src = solid(128, 16, 16);
        let out = BlurOp::new(2.0).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| {
            assert!((p[0] as i16 - 128).abs() <= 1);
        });
    }

    #[test]
    fn output_dimensions_unchanged() {
        let src = solid(100, 32, 20);
        let out = BlurOp::new(3.0).apply(src).unwrap();
        assert_eq!(out.width, 32);
        assert_eq!(out.height, 20);
    }

    #[test]
    fn centre_of_bright_spot_dims() {
        // A bright dot on a dark background should produce a lower peak
        // value at the centre after blurring.
        let mut src = Image::new(16, 16);
        src.data.chunks_mut(4).for_each(|p| p[3] = 255);
        // Set centre pixel to white.
        let cx = 8usize;
        let cy = 8usize;
        let off = (cy * 16 + cx) * 4;
        src.data[off] = 255;
        src.data[off + 1] = 255;
        src.data[off + 2] = 255;

        let out = BlurOp::new(2.0).apply(src).unwrap();
        assert!(
            out.data[off] < 255,
            "bright spot centre should be dimmed by blur"
        );
    }

    #[test]
    fn kernel_sums_to_one() {
        for sigma in [0.5, 1.0, 2.5, 5.0] {
            let k = gaussian_kernel(sigma);
            let sum: f32 = k.iter().sum();
            assert!(
                (sum - 1.0).abs() < 1e-5,
                "kernel sum={} for sigma={}",
                sum,
                sigma
            );
        }
    }
}
