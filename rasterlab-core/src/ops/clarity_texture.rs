use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Clarity and Texture — local contrast enhancements at two different scales.
///
/// Both controls work via an unsharp-mask approach (detail = image − blurred),
/// but differ in scale and weighting:
///
/// * **Clarity** — large-radius (≈3 % of min dimension) blur; midtone-weighted
///   so highlights and shadows are not blown out.  Positive values add punch
///   and depth; negative values create a soft/dreamy look.
///
/// * **Texture** — small-radius (≈0.5 % of min dimension) blur; uniform weight.
///   Targets fine surface detail without affecting broader tones.
///
/// Range for both is `[-1.0, 1.0]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarityTextureOp {
    /// Midtone contrast. `0.0` = no change. `[-1.0, 1.0]`.
    pub clarity: f32,
    /// Fine-detail contrast. `0.0` = no change. `[-1.0, 1.0]`.
    pub texture: f32,
}

impl ClarityTextureOp {
    pub fn new(clarity: f32, texture: f32) -> Self {
        Self {
            clarity: clarity.clamp(-1.0, 1.0),
            texture: texture.clamp(-1.0, 1.0),
        }
    }
}

#[typetag::serde]
impl Operation for ClarityTextureOp {
    fn name(&self) -> &'static str {
        "clarity_texture"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn apply(&self, image: Image) -> RasterResult<Image> {
        if self.clarity == 0.0 && self.texture == 0.0 {
            return Ok(image);
        }

        let w = image.width as usize;
        let h = image.height as usize;
        let min_dim = w.min(h) as f32;

        // Work in linear f32 [0,1] per channel
        let mut pixels: Vec<[f32; 3]> = image
            .data
            .chunks_exact(4)
            .map(|p| {
                [
                    p[0] as f32 / 255.0,
                    p[1] as f32 / 255.0,
                    p[2] as f32 / 255.0,
                ]
            })
            .collect();

        // Extract luminance for weighting
        let lum: Vec<f32> = pixels
            .iter()
            .map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
            .collect();

        if self.clarity != 0.0 {
            let radius = ((min_dim * 0.03).round() as usize).max(2);
            let blurred_lum = box_blur_1ch(&lum, w, h, radius);
            apply_detail(
                &mut pixels,
                &lum,
                &blurred_lum,
                w,
                h,
                self.clarity,
                true, // midtone weighting
            );
        }

        if self.texture != 0.0 {
            let radius = ((min_dim * 0.005).round() as usize).max(1);
            // Re-extract lum after clarity (if any) so texture builds on current state
            let lum2: Vec<f32> = pixels
                .iter()
                .map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2])
                .collect();
            let blurred_lum = box_blur_1ch(&lum2, w, h, radius);
            apply_detail(
                &mut pixels,
                &lum2,
                &blurred_lum,
                w,
                h,
                self.texture,
                false, // uniform weight
            );
        }

        // Convert back to Image
        let mut out = Image::new(image.width, image.height);
        out.metadata = image.metadata.clone();
        for (i, (p, src)) in out.data.chunks_exact_mut(4).zip(pixels.iter()).enumerate() {
            p[0] = (src[0] * 255.0).clamp(0.0, 255.0) as u8;
            p[1] = (src[1] * 255.0).clamp(0.0, 255.0) as u8;
            p[2] = (src[2] * 255.0).clamp(0.0, 255.0) as u8;
            p[3] = image.data[i * 4 + 3]; // preserve alpha
        }
        Ok(out)
    }

    fn describe(&self) -> String {
        match (self.clarity != 0.0, self.texture != 0.0) {
            (true, true) => format!("Clarity {:.2}  Texture {:.2}", self.clarity, self.texture),
            (true, false) => format!("Clarity {:.2}", self.clarity),
            (false, true) => format!("Texture {:.2}", self.texture),
            (false, false) => "Clarity / Texture (none)".into(),
        }
    }
}

/// Add `amount * detail * weight` to each RGB channel.
///
/// `detail = lum - blurred_lum` (positive = edges/texture, negative = flat areas).
/// When `midtone_weight` is true the contribution is scaled by `4*L*(1-L)`,
/// which peaks at mid-grey and falls to zero at black and white.
fn apply_detail(
    pixels: &mut [[f32; 3]],
    lum: &[f32],
    blurred_lum: &[f32],
    w: usize,
    h: usize,
    amount: f32,
    midtone_weight: bool,
) {
    let n = w * h;
    pixels[..n]
        .par_chunks_mut(w)
        .zip(lum.par_chunks(w))
        .zip(blurred_lum.par_chunks(w))
        .for_each(|((row_px, row_lum), row_blur)| {
            for i in 0..row_px.len() {
                let l = row_lum[i];
                let detail = l - row_blur[i];
                let weight = if midtone_weight {
                    4.0 * l * (1.0 - l)
                } else {
                    1.0
                };
                let boost = amount * detail * weight;
                for c in row_px[i].iter_mut().take(3) {
                    *c = (*c + boost).clamp(0.0, 1.0);
                }
            }
        });
}

// ---------------------------------------------------------------------------
// Box blur (single channel, separable sliding-window, O(w*h) regardless of radius)
// ---------------------------------------------------------------------------

fn box_blur_1ch(src: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    // 2D separable box blur ≈ Gaussian via 3 passes.
    //
    // Because box blur is a linear separable filter, all horizontal passes
    // commute with all vertical passes:
    //   (H¹V¹)(H²V²)(H³V³) = H³ · V³
    //
    // So we do all 3 H passes first, then transpose once, run 3 H passes
    // (which act as V passes in the original space), and transpose back.
    // That is 2 transposes total instead of 6 (one per H+V pair).
    let mut buf = src.to_vec();
    for _ in 0..3 {
        box_blur_h_1ch(&mut buf, w, h, radius);
    }
    let mut t = transpose(&buf, w, h);
    for _ in 0..3 {
        box_blur_h_1ch(&mut t, h, w, radius);
    }
    transpose(&t, h, w)
}

/// Horizontal box blur — one row per rayon task.
fn box_blur_h_1ch(buf: &mut [f32], w: usize, _h: usize, radius: usize) {
    buf.par_chunks_mut(w).for_each(|row| {
        let mut out = vec![0.0f32; w];
        // Seed the window sum for x = 0
        let mut sum = 0.0f32;
        for &v in row.iter().take(radius.min(w - 1) + 1) {
            sum += v;
        }
        // How many samples are actually in the initial window (right side may be clamped)
        let mut count = (radius.min(w - 1) + 1) as f32;

        for x in 0..w {
            out[x] = sum / count;
            // Advance window: remove leaving sample (x-radius), add entering (x+radius+1)
            if x + radius + 1 < w {
                sum += row[x + radius + 1];
                count += 1.0;
            }
            if x >= radius {
                sum -= row[x - radius];
                count -= 1.0;
            }
        }
        row.copy_from_slice(&out);
    });
}

/// Transpose a row-major w×h buffer into an h×w buffer.
/// Both the read and write passes are tiled so each touches cache-friendly blocks.
fn transpose(src: &[f32], w: usize, h: usize) -> Vec<f32> {
    const TILE: usize = 64;
    let mut dst = vec![0.0f32; w * h];
    for ty in (0..h).step_by(TILE) {
        for tx in (0..w).step_by(TILE) {
            let row_end = (ty + TILE).min(h);
            let col_end = (tx + TILE).min(w);
            for y in ty..row_end {
                for x in tx..col_end {
                    dst[x * h + y] = src[y * w + x];
                }
            }
        }
    }
    dst
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_is_identity() {
        let mut src = Image::new(8, 8);
        src.data.chunks_mut(4).enumerate().for_each(|(i, p)| {
            p[0] = (i * 7 % 200) as u8;
            p[1] = (i * 13 % 200) as u8;
            p[2] = (i * 17 % 200) as u8;
            p[3] = 255;
        });
        let expected = src.data.clone();
        let out = ClarityTextureOp::new(0.0, 0.0).apply(src).unwrap();
        assert_eq!(out.data, expected);
    }

    #[test]
    fn flat_image_unchanged() {
        // A completely flat image has zero detail, so clarity/texture = no change
        let mut src = Image::new(16, 16);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 128;
            p[1] = 100;
            p[2] = 80;
            p[3] = 255;
        });
        let expected = src.data.clone();
        let out = ClarityTextureOp::new(1.0, 1.0).apply(src).unwrap();
        // Allow 1-unit rounding error from f32↔u8 conversion
        for (a, b) in out.data.iter().zip(expected.iter()) {
            assert!(a.abs_diff(*b) <= 1, "pixel mismatch: {a} vs {b}");
        }
    }
}
