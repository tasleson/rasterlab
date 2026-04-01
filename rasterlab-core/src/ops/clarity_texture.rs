use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Clarity and Texture — local contrast enhancements at two different scales.
///
/// Both controls work via an [unsharp-mask] approach (`detail = image − blurred`),
/// but differ in scale and weighting:
///
/// * **Clarity** — large-radius (≈3 % of min dimension) blur; midtone-weighted
///   so highlights and shadows are not blown out.  Positive values add punch
///   and depth; negative values create a soft/dreamy look.  The midtone weight
///   `4·L·(1−L)` peaks at mid-grey and falls to zero at black and white; see
///   the [darktable local-contrast docs] for a practical description of the
///   same approach.
///
/// * **Texture** — small-radius (≈0.5 % of min dimension) blur; uniform weight.
///   Targets fine surface detail without affecting broader tones.  Concept
///   introduced by Adobe Lightroom (2019); see the [Adobe texture blog post].
///
/// The blur kernel is approximated by three passes of a separable box blur,
/// which converges to a Gaussian by the central limit theorem.  See Kovesi,
/// *[Fast Almost-Gaussian Filtering]* (2010) for the analytical justification
/// and O(1)-in-radius sliding-window implementation used here.
///
/// Range for both is `[-1.0, 1.0]`.
///
/// [unsharp-mask]: https://en.wikipedia.org/wiki/Unsharp_masking
/// [darktable local-contrast docs]: https://docs.darktable.org/usermanual/development/en/module-reference/processing-modules/local-contrast/
/// [Adobe texture blog post]: https://blog.adobe.com/en/publish/2019/03/26/texture-a-new-slider-in-lightroom
/// [Fast Almost-Gaussian Filtering]: https://www.peterkovesi.com/papers/FastGaussianSmoothing.pdf
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
    // Three passes of separable H+V ≈ Gaussian.
    let mut buf = src.to_vec();
    for _ in 0..3 {
        box_blur_h_1ch(&mut buf, w, h, radius);
        box_blur_v_1ch(&mut buf, w, h, radius);
    }
    buf
}

/// Horizontal box blur — one row per rayon task.
fn box_blur_h_1ch(buf: &mut [f32], w: usize, _h: usize, radius: usize) {
    buf.par_chunks_mut(w).for_each(|row| {
        let mut out = vec![0.0f32; w];
        let mut sum = 0.0f32;
        for &v in row.iter().take(radius.min(w - 1) + 1) {
            sum += v;
        }
        let mut count = (radius.min(w - 1) + 1) as f32;
        for x in 0..w {
            out[x] = sum / count;
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

/// Vertical box blur — parallel column strips, no transpose.
///
/// Columns are processed in strips of STRIP adjacent columns.  Each strip
/// gathers its data into a contiguous [h × STRIP] buffer (one cache-line per
/// row in the source), runs the sliding-window blur across all STRIP columns
/// simultaneously (SIMD-friendly inner loop), then writes the results back.
/// Strips cover disjoint column ranges so parallel mutation is sound.
fn box_blur_v_1ch(buf: &mut [f32], w: usize, h: usize, radius: usize) {
    const STRIP: usize = 16; // 16 f32 = 64 bytes = one cache line per source row

    let n_strips = w.div_ceil(STRIP);
    // Cast to usize so the closure captures a Send+Sync value. Casting back
    // inside the closure is sound because strips cover disjoint column ranges.
    let raw = buf.as_mut_ptr() as usize;

    (0..n_strips).into_par_iter().for_each(|s| {
        let x0 = s * STRIP;
        let sw = STRIP.min(w - x0); // actual columns in this strip (last strip may be narrow)
        let p = raw as *mut f32;

        // --- Gather: copy strip columns into contiguous [h × sw] buffer ---
        // Each row contributes one memcpy of sw floats (≤ one cache line).
        let mut tmp = vec![0.0f32; h * sw];
        for y in 0..h {
            let src_row = unsafe { std::slice::from_raw_parts(p.add(y * w + x0), sw) };
            tmp[y * sw..y * sw + sw].copy_from_slice(src_row);
        }

        // --- Blur: sliding window over all sw columns simultaneously ---
        // Inner loops over sw are auto-vectorised by the compiler.
        let mut sums = vec![0.0f32; sw];
        let seed_end = radius.min(h - 1);
        for y in 0..=seed_end {
            let row = &tmp[y * sw..y * sw + sw];
            for c in 0..sw {
                sums[c] += row[c];
            }
        }
        let mut count = (seed_end + 1) as f32;

        for y in 0..h {
            let inv = 1.0 / count;
            // Write blurred values directly into buf for this strip's columns.
            unsafe {
                let dst_row = std::slice::from_raw_parts_mut(p.add(y * w + x0), sw);
                for c in 0..sw {
                    dst_row[c] = sums[c] * inv;
                }
            }
            if y + radius + 1 < h {
                let add_row = &tmp[(y + radius + 1) * sw..(y + radius + 1) * sw + sw];
                for c in 0..sw {
                    sums[c] += add_row[c];
                }
                count += 1.0;
            }
            if y >= radius {
                let sub_row = &tmp[(y - radius) * sw..(y - radius) * sw + sw];
                for c in 0..sw {
                    sums[c] -= sub_row[c];
                }
                count -= 1.0;
            }
        }
    });
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
