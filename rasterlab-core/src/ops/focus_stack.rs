//! Focus stacking operation.
//!
//! Fuses multiple images captured at different focus distances into a single
//! all-in-focus result.  The inputs are assumed to be pre-aligned (same
//! camera position, same framing) — only the focus plane differs.
//!
//! Algorithm:
//! 1. Load every frame from disk (op is self-contained for replay).
//! 2. Verify all frames have matching dimensions.
//! 3. For each frame, compute a per-pixel focus measure using the
//!    **Sum-Modified-Laplacian** (SML) aggregated over a 7×7 window.
//! 4. Smooth each SML map with a separable box blur so the per-pixel
//!    winner-selection doesn't flicker between adjacent pixels on flat
//!    content.
//! 5. Fuse with a weighted blend `w_i = SML_blur_i^p / Σ SML_blur_j^p`
//!    (p = 4).  The high exponent behaves like winner-takes-all where one
//!    image is clearly sharper while still producing soft transitions on
//!    tied regions.
//!
//! `apply()` ignores the input `Image` and reloads every frame from the
//! stored `image_paths`, making the op fully self-contained for
//! serialisation / non-destructive replay from the `.rlab` stack.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    cancel,
    error::{RasterError, RasterResult},
    formats::FormatRegistry,
    image::Image,
    traits::operation::Operation,
};

// ── Tuning constants ─────────────────────────────────────────────────────────

/// Half-side of the Modified-Laplacian aggregation window.  A 7×7 window
/// (`SML_HALF = 3`) balances noise tolerance and spatial locality.
const SML_HALF: usize = 3;
/// Pixel step between the centre and neighbour samples used in the
/// Modified-Laplacian.  A step of 1 is standard; larger steps are more
/// robust to high-frequency noise at the cost of detail.
const ML_STEP: usize = 1;
/// Box-blur radius applied to the SML map before fusion.  Smooths
/// per-pixel winner selection.
const WEIGHT_BLUR_RADIUS: usize = 5;
/// Exponent applied to SML weights.  Higher values → closer to
/// winner-takes-all; lower → softer blend.
const WEIGHT_POWER: f32 = 4.0;
/// Floor added to each weight before normalisation so that pixels with
/// no usable focus signal (completely flat in every frame) still produce
/// a finite output instead of NaN.
const WEIGHT_EPSILON: f32 = 1e-4;

// ── Public op ────────────────────────────────────────────────────────────────

/// Non-destructive focus-stacking op.
///
/// Stores the absolute paths of every frame to fuse.  `apply()` ignores
/// its `Image` argument and produces the stacked result from scratch so
/// the op is self-contained and replayable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusStackOp {
    /// Absolute paths to the source frames, in any order.
    pub image_paths: Vec<String>,
}

impl FocusStackOp {
    pub fn new(image_paths: Vec<String>) -> Self {
        Self { image_paths }
    }
}

#[typetag::serde]
impl Operation for FocusStackOp {
    fn name(&self) -> &'static str {
        "focus_stack"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn apply(&self, _image: Image) -> RasterResult<Image> {
        stack(self)
    }

    fn describe(&self) -> String {
        format!("Focus Stack ({} frames)", self.image_paths.len())
    }

    fn is_geometric(&self) -> bool {
        false
    }
}

// ── Top-level entry point ────────────────────────────────────────────────────

fn stack(op: &FocusStackOp) -> RasterResult<Image> {
    if op.image_paths.is_empty() {
        return Err(RasterError::InvalidParams(
            "Focus Stack: no image paths specified".into(),
        ));
    }

    let reg = FormatRegistry::with_builtins();

    // Load every frame.
    let images: Vec<Image> = op
        .image_paths
        .iter()
        .map(|p| {
            if cancel::is_requested() {
                return Err(RasterError::Cancelled);
            }
            reg.decode_file(std::path::Path::new(p)).map_err(|e| {
                RasterError::InvalidParams(format!("Focus Stack: cannot load '{p}': {e}"))
            })
        })
        .collect::<RasterResult<_>>()?;

    if images.len() == 1 {
        // Nothing to fuse — return the single loaded frame unchanged.
        return Ok(images.into_iter().next().unwrap());
    }

    // All frames must have identical dimensions (the op assumes alignment).
    let (w, h) = (images[0].width, images[0].height);
    for (i, img) in images.iter().enumerate().skip(1) {
        if img.width != w || img.height != h {
            return Err(RasterError::InvalidParams(format!(
                "Focus Stack: image {i} has dimensions {}x{} but image 0 is {w}x{h}",
                img.width, img.height
            )));
        }
    }

    // ── Per-frame focus measure ──────────────────────────────────────────
    let wu = w as usize;
    let hu = h as usize;

    let weights: Vec<Vec<f32>> = images
        .par_iter()
        .map(|img| {
            let gray = to_gray(img);
            let sml = sum_modified_laplacian(&gray, wu, hu);
            box_blur(&sml, wu, hu, WEIGHT_BLUR_RADIUS)
        })
        .collect();

    if cancel::is_requested() {
        return Err(RasterError::Cancelled);
    }

    // ── Weighted fusion ─────────────────────────────────────────────────
    //
    // For each output pixel:
    //   w_i = (weights[i] + EPS)^p
    //   out = Σ w_i · src_i  /  Σ w_i
    //
    // Parallelise over output scanlines; each worker touches every
    // source image's row-slice, which is cache-friendly.

    let mut out = Image::new(w, h);
    let n = images.len();

    out.data
        .par_chunks_mut(wu * 4)
        .enumerate()
        .for_each(|(y, row)| {
            for x in 0..wu {
                let idx = y * wu + x;
                let mut r_acc = 0.0f32;
                let mut g_acc = 0.0f32;
                let mut b_acc = 0.0f32;
                let mut w_sum = 0.0f32;

                for k in 0..n {
                    let wt = (weights[k][idx] + WEIGHT_EPSILON).powf(WEIGHT_POWER);
                    let p = &images[k].data[idx * 4..idx * 4 + 4];
                    r_acc += wt * p[0] as f32;
                    g_acc += wt * p[1] as f32;
                    b_acc += wt * p[2] as f32;
                    w_sum += wt;
                }

                let inv = 1.0 / w_sum;
                let px = &mut row[x * 4..x * 4 + 4];
                px[0] = (r_acc * inv).clamp(0.0, 255.0) as u8;
                px[1] = (g_acc * inv).clamp(0.0, 255.0) as u8;
                px[2] = (b_acc * inv).clamp(0.0, 255.0) as u8;
                px[3] = 255;
            }
        });

    Ok(out)
}

// ── Focus measure: Sum of Modified Laplacian ────────────────────────────────

/// Luminance conversion (Rec. 709 weights).
fn to_gray(image: &Image) -> Vec<f32> {
    image
        .data
        .chunks_exact(4)
        .map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32)
        .collect()
}

/// Modified Laplacian at a single pixel, using horizontal and vertical
/// second differences (Nayar 1994).
#[inline]
fn modified_laplacian_at(gray: &[f32], w: usize, h: usize, x: usize, y: usize) -> f32 {
    let step = ML_STEP;
    let xm = x.saturating_sub(step);
    let xp = (x + step).min(w - 1);
    let ym = y.saturating_sub(step);
    let yp = (y + step).min(h - 1);
    let c = gray[y * w + x];
    let lx = (2.0 * c - gray[y * w + xm] - gray[y * w + xp]).abs();
    let ly = (2.0 * c - gray[ym * w + x] - gray[yp * w + x]).abs();
    lx + ly
}

/// Sum of Modified Laplacian over a `(2·SML_HALF+1)²` window.  The result
/// is the per-pixel focus-measure map.
fn sum_modified_laplacian(gray: &[f32], w: usize, h: usize) -> Vec<f32> {
    // Precompute per-pixel Modified-Laplacian, then aggregate with a box
    // sum over the square window.  The two-pass approach is O(wh) instead
    // of O(wh·k²) for the naive implementation.
    let mut ml = vec![0.0f32; w * h];
    ml.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, cell) in row.iter_mut().enumerate() {
            *cell = modified_laplacian_at(gray, w, h, x, y);
        }
    });

    box_blur_sum(&ml, w, h, SML_HALF)
}

// ── Separable box blur / box sum ─────────────────────────────────────────────

/// Running-sum box aggregation over a `(2·radius+1)²` window.  Returns
/// the sum (not the mean) — used for the SML aggregation so the magnitude
/// of the weights tracks the window area.
fn box_blur_sum(src: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let k = 2 * radius + 1;
    let mut tmp = vec![0.0f32; w * h];

    // Horizontal pass.
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let row_src = &src[y * w..(y + 1) * w];
        let mut acc = 0.0f32;
        for &v in row_src.iter().take(k.min(w)) {
            acc += v;
        }
        row[radius.min(w - 1)] = acc;
        for x in (radius + 1)..w.saturating_sub(radius) {
            acc += row_src[x + radius];
            acc -= row_src[x - radius - 1];
            row[x] = acc;
        }
        // Edge fill: reuse the nearest interior sum.
        let first_valid = radius.min(w - 1);
        for x in 0..first_valid {
            row[x] = row[first_valid];
        }
        let last_valid = w.saturating_sub(radius + 1);
        for x in (last_valid + 1)..w {
            row[x] = row[last_valid];
        }
    });

    // Vertical pass.  Done serially because column strides are cache
    // hostile; at ~5 ms on 20 MP it's dwarfed by the fusion cost anyway.
    let mut out = vec![0.0f32; w * h];
    for x in 0..w {
        let mut acc = 0.0f32;
        for i in 0..k.min(h) {
            acc += tmp[i * w + x];
        }
        let first_valid = radius.min(h - 1);
        out[first_valid * w + x] = acc;
        for y in (radius + 1)..h.saturating_sub(radius) {
            acc += tmp[(y + radius) * w + x];
            acc -= tmp[(y - radius - 1) * w + x];
            out[y * w + x] = acc;
        }
        // Edge fill.
        let val = out[first_valid * w + x];
        for y in 0..first_valid {
            out[y * w + x] = val;
        }
        let last = h.saturating_sub(radius + 1);
        let val = out[last * w + x];
        for y in (last + 1)..h {
            out[y * w + x] = val;
        }
    }

    out
}

/// Box blur returning the mean (divided by window area).  Used to smooth
/// the SML map before fusion so the winner selection doesn't flicker.
fn box_blur(src: &[f32], w: usize, h: usize, radius: usize) -> Vec<f32> {
    let sum = box_blur_sum(src, w, h, radius);
    let k = (2 * radius + 1) as f32;
    let inv_area = 1.0 / (k * k);
    sum.into_iter().map(|v| v * inv_area).collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// When every frame is identical, focus stacking must return an
    /// image whose pixels match the input (up to rounding in the fusion).
    #[test]
    fn identical_frames_round_trip() {
        let w = 32u32;
        let h = 24u32;
        let mut img = Image::new(w, h);
        for (i, chunk) in img.data.chunks_exact_mut(4).enumerate() {
            chunk[0] = (i * 7 % 256) as u8;
            chunk[1] = (i * 11 % 256) as u8;
            chunk[2] = (i * 13 % 256) as u8;
            chunk[3] = 255;
        }

        // Fake a three-frame stack where every frame is identical.  Skip the
        // op's file loading by calling the internal fusion path directly.
        let images = [img.deep_clone(), img.deep_clone(), img.deep_clone()];
        let wu = w as usize;
        let hu = h as usize;
        let weights: Vec<Vec<f32>> = images
            .iter()
            .map(|i| {
                let g = to_gray(i);
                let sml = sum_modified_laplacian(&g, wu, hu);
                box_blur(&sml, wu, hu, WEIGHT_BLUR_RADIUS)
            })
            .collect();

        let mut out = Image::new(w, h);
        let n = images.len();
        #[allow(clippy::needless_range_loop)]
        for idx in 0..(wu * hu) {
            let mut r = 0.0f32;
            let mut g = 0.0f32;
            let mut b = 0.0f32;
            let mut ws = 0.0f32;
            for k in 0..n {
                let wt = (weights[k][idx] + WEIGHT_EPSILON).powf(WEIGHT_POWER);
                let p = &images[k].data[idx * 4..idx * 4 + 4];
                r += wt * p[0] as f32;
                g += wt * p[1] as f32;
                b += wt * p[2] as f32;
                ws += wt;
            }
            let inv = 1.0 / ws;
            let px = &mut out.data[idx * 4..idx * 4 + 4];
            px[0] = (r * inv).clamp(0.0, 255.0) as u8;
            px[1] = (g * inv).clamp(0.0, 255.0) as u8;
            px[2] = (b * inv).clamp(0.0, 255.0) as u8;
            px[3] = 255;
        }

        for (a, b) in img.data.iter().zip(out.data.iter()) {
            assert!((*a as i16 - *b as i16).abs() <= 1, "{a} vs {b}");
        }
    }

    #[test]
    fn sharper_frame_wins() {
        // Build a 64×64 scene where one frame has a sharp checker pattern
        // in the centre and another is a flat grey.  The focus weights at
        // the checker centre must be larger for the sharp frame.
        let w = 64usize;
        let h = 64usize;
        let mut sharp = vec![120.0f32; w * h];
        for y in 20..44 {
            for x in 20..44 {
                sharp[y * w + x] = if (x + y) % 2 == 0 { 20.0 } else { 220.0 };
            }
        }
        let flat = vec![120.0f32; w * h];

        let sml_sharp = sum_modified_laplacian(&sharp, w, h);
        let sml_flat = sum_modified_laplacian(&flat, w, h);
        let wb_sharp = box_blur(&sml_sharp, w, h, WEIGHT_BLUR_RADIUS);
        let wb_flat = box_blur(&sml_flat, w, h, WEIGHT_BLUR_RADIUS);

        let centre = 32 * w + 32;
        assert!(
            wb_sharp[centre] > 10.0 * wb_flat[centre].max(1e-3),
            "sharp SML {} should dominate flat SML {}",
            wb_sharp[centre],
            wb_flat[centre]
        );
    }
}
