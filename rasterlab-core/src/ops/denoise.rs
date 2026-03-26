use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Edge-preserving noise reduction via a bilateral filter.
///
/// A bilateral filter smooths a pixel by averaging its neighbours, but weights
/// each neighbour by both **spatial distance** (like a Gaussian blur) and
/// **colour similarity** (so edges are not blurred across).  Noisy flat areas
/// are smoothed strongly; high-contrast edges are left intact.
///
/// Parameters
/// ----------
/// * `strength`  — controls the colour-similarity Gaussian (`σ_r`).
///   Range `[0.01, 1.0]`.  Higher values blend colours that are further apart,
///   giving more aggressive smoothing at the cost of edge fidelity.
///   Good starting point: `0.1`.
/// * `radius`    — spatial kernel half-width in pixels.
///   Range `[1, 10]`.  Larger radii remove bigger noise features but are
///   slower (`O(radius²)` per pixel).  Good starting point: `3`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenoiseOp {
    /// Colour-similarity sigma (range-domain bandwidth). Range `[0.01, 1.0]`.
    pub strength: f32,
    /// Spatial kernel radius in pixels. Range `[1, 10]`.
    pub radius: u32,
}

impl DenoiseOp {
    pub fn new(strength: f32, radius: u32) -> Self {
        Self {
            strength: strength.clamp(0.01, 1.0),
            radius: radius.clamp(1, 10),
        }
    }
}

impl Default for DenoiseOp {
    fn default() -> Self {
        Self {
            strength: 0.1,
            radius: 3,
        }
    }
}

#[typetag::serde]
impl Operation for DenoiseOp {
    fn name(&self) -> &'static str {
        "denoise"
    }

    fn apply(&self, image: &Image) -> RasterResult<Image> {
        let w = image.width as usize;
        let h = image.height as usize;
        let r = self.radius as usize;

        // σ_r governs colour-similarity weighting.  Internally we work in
        // [0,1] float space; `strength` maps directly to σ_r.
        let sigma_r = self.strength;
        let sigma_r2 = 2.0 * sigma_r * sigma_r;

        // Spatial Gaussian kernel (precomputed for the square window).
        // σ_s is set to radius/2 so the kernel falls off meaningfully within
        // the window without being a flat box.
        let sigma_s = (r as f32).max(1.0) * 0.5;
        let sigma_s2 = 2.0 * sigma_s * sigma_s;

        // Precompute spatial weights for a (2r+1)×(2r+1) window.
        let ks = 2 * r + 1;
        let spatial: Vec<f32> = (0..ks * ks)
            .map(|idx| {
                let dy = (idx / ks) as f32 - r as f32;
                let dx = (idx % ks) as f32 - r as f32;
                (-(dx * dx + dy * dy) / sigma_s2).exp()
            })
            .collect();

        let mut out = image.deep_clone();

        // Process each pixel in parallel.
        out.data
            .par_chunks_mut(4)
            .enumerate()
            .for_each(|(idx, dst)| {
                let py = idx / w;
                let px = idx % w;

                let src_r = image.data[idx * 4] as f32 / 255.0;
                let src_g = image.data[idx * 4 + 1] as f32 / 255.0;
                let src_b = image.data[idx * 4 + 2] as f32 / 255.0;

                let mut sum_r = 0.0f32;
                let mut sum_g = 0.0f32;
                let mut sum_b = 0.0f32;
                let mut sum_w = 0.0f32;

                let y_lo = py.saturating_sub(r);
                let y_hi = (py + r + 1).min(h);
                let x_lo = px.saturating_sub(r);
                let x_hi = (px + r + 1).min(w);

                for ny in y_lo..y_hi {
                    let dy = (ny as isize - py as isize + r as isize) as usize;
                    for nx in x_lo..x_hi {
                        let dx = (nx as isize - px as isize + r as isize) as usize;
                        let s_w = spatial[dy * ks + dx];

                        let off = (ny * w + nx) * 4;
                        let nr = image.data[off] as f32 / 255.0;
                        let ng = image.data[off + 1] as f32 / 255.0;
                        let nb = image.data[off + 2] as f32 / 255.0;

                        let dr = nr - src_r;
                        let dg = ng - src_g;
                        let db = nb - src_b;
                        let colour_dist2 = dr * dr + dg * dg + db * db;
                        let r_w = (-colour_dist2 / sigma_r2).exp();

                        let w_total = s_w * r_w;
                        sum_r += w_total * nr;
                        sum_g += w_total * ng;
                        sum_b += w_total * nb;
                        sum_w += w_total;
                    }
                }

                if sum_w > 1e-9 {
                    dst[0] = ((sum_r / sum_w) * 255.0).clamp(0.0, 255.0) as u8;
                    dst[1] = ((sum_g / sum_w) * 255.0).clamp(0.0, 255.0) as u8;
                    dst[2] = ((sum_b / sum_w) * 255.0).clamp(0.0, 255.0) as u8;
                }
                // alpha unchanged
            });

        Ok(out)
    }

    fn describe(&self) -> String {
        format!("Denoise  r={} s={:.2}", self.radius, self.strength)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy_image() -> Image {
        // Alternating 100/200 pixel pattern — high frequency noise.
        let mut img = Image::new(16, 16);
        img.data.chunks_mut(4).enumerate().for_each(|(i, p)| {
            let v = if i % 2 == 0 { 100u8 } else { 200u8 };
            p[0] = v;
            p[1] = v;
            p[2] = v;
            p[3] = 255;
        });
        img
    }

    fn solid_grey(v: u8) -> Image {
        let mut img = Image::new(16, 16);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = v;
            p[1] = v;
            p[2] = v;
            p[3] = 255;
        });
        img
    }

    #[test]
    fn flat_region_largely_preserved() {
        // A uniform grey should come out nearly identical.
        let src = solid_grey(128);
        let out = DenoiseOp::new(0.1, 3).apply(&src).unwrap();
        for (a, b) in src.data.chunks(4).zip(out.data.chunks(4)) {
            assert!((a[0] as i16 - b[0] as i16).abs() <= 2);
        }
    }

    #[test]
    fn noisy_signal_reduced() {
        // After denoising the alternating pattern should be more uniform.
        let src = noisy_image();
        let out = DenoiseOp::new(0.5, 3).apply(&src).unwrap();

        // Compute variance of the output; should be less than input.
        let var = |data: &[u8]| {
            let mean: f64 =
                data.iter().step_by(4).map(|&v| v as f64).sum::<f64>() / (data.len() / 4) as f64;
            data.iter()
                .step_by(4)
                .map(|&v| {
                    let d = v as f64 - mean;
                    d * d
                })
                .sum::<f64>()
                / (data.len() / 4) as f64
        };
        assert!(
            var(&out.data) < var(&src.data),
            "denoised variance should be lower"
        );
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(8, 8);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 120;
            p[1] = 130;
            p[2] = 140;
            p[3] = 55;
        });
        let out = DenoiseOp::new(0.1, 2).apply(&src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 55));
    }

    #[test]
    fn edge_not_blurred_across() {
        // Left half white (200), right half dark (50) — edge pixel should
        // stay closer to its own side than to the opposite side.
        let mut src = Image::new(16, 16);
        src.data.chunks_mut(4).enumerate().for_each(|(i, p)| {
            let x = i % 16;
            let v = if x < 8 { 200u8 } else { 50u8 };
            p[0] = v;
            p[1] = v;
            p[2] = v;
            p[3] = 255;
        });
        let out = DenoiseOp::new(0.1, 3).apply(&src).unwrap();
        // Pixel at x=6 (left side) should stay close to 200.
        let left_idx = 6 * 4;
        assert!(
            out.data[left_idx] > 150,
            "left-side pixel should stay bright"
        );
        // Pixel at x=9 (right side) should stay close to 50.
        let right_idx = 9 * 4;
        assert!(
            out.data[right_idx] < 100,
            "right-side pixel should stay dark"
        );
    }
}
