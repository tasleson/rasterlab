use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    cancel,
    error::{RasterError, RasterResult},
    image::Image,
    traits::operation::Operation,
};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NrMethod {
    Wavelet,
    NonLocalMeans,
}

/// High-quality noise reduction with luminance/chroma separation and
/// detail-preservation masking.
///
/// Two algorithms are available:
/// * **Wavelet** — fast Haar-wavelet soft-thresholding.  Good quality in
///   milliseconds; suitable for interactive use.
/// * **NonLocalMeans** — patch-based NLM for maximum quality.  Much slower
///   (~10–30 s on 24 MP) but handles complex noise patterns better.
///
/// Parameters
/// ----------
/// * `luma_strength`        — Y-channel NR intensity `[0, 1]`.
/// * `color_strength`       — Cb/Cr NR intensity `[0, 1]`.
/// * `detail_preservation`  — Edge-sharpness preservation `[0, 1]`.
///   `0` = maximum smoothing, `1` = preserve all edges.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseReductionOp {
    pub method: NrMethod,
    /// Luminance (Y-channel) NR strength. Range [0.0, 1.0].
    pub luma_strength: f32,
    /// Chrominance (Cb/Cr) NR strength. Range [0.0, 1.0].
    pub color_strength: f32,
    /// How much to preserve fine detail / edges. Range [0.0, 1.0].
    /// 0.0 = maximum smoothing, 1.0 = preserve all edges.
    pub detail_preservation: f32,
}

impl Default for NoiseReductionOp {
    fn default() -> Self {
        Self {
            method: NrMethod::Wavelet,
            luma_strength: 0.3,
            color_strength: 0.5,
            detail_preservation: 0.5,
        }
    }
}

// ---------------------------------------------------------------------------
// Color space helpers
// ---------------------------------------------------------------------------

#[inline]
fn rgb_to_ycbcr(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let r = r as f32;
    let g = g as f32;
    let b = b as f32;
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let cb = -0.16874 * r - 0.33126 * g + 0.5 * b + 128.0;
    let cr = 0.5 * r - 0.41869 * g - 0.08131 * b + 128.0;
    (y, cb, cr)
}

#[inline]
fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> (u8, u8, u8) {
    let r = y + 1.402 * (cr - 128.0);
    let g = y - 0.34414 * (cb - 128.0) - 0.71414 * (cr - 128.0);
    let b = y + 1.772 * (cb - 128.0);
    (
        r.clamp(0.0, 255.0) as u8,
        g.clamp(0.0, 255.0) as u8,
        b.clamp(0.0, 255.0) as u8,
    )
}

// ---------------------------------------------------------------------------
// Wavelet helpers
// ---------------------------------------------------------------------------

#[inline]
fn soft_threshold(v: f32, t: f32) -> f32 {
    let abs = v.abs();
    if abs <= t {
        0.0
    } else {
        v.signum() * (abs - t)
    }
}

fn next_pow2(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut p = 1usize;
    while p < n {
        p <<= 1;
    }
    p
}

/// Pad a plane to (pw × ph) using mirror reflection at the borders.
fn pad_to_pow2(plane: &[f32], w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    let pw = next_pow2(w);
    let ph = next_pow2(h);
    let mut out = vec![0.0f32; pw * ph];
    for row in 0..ph {
        let src_row = if row < h {
            row
        } else {
            // mirror: reflect about row h-1
            let excess = row - h;
            h.saturating_sub(1 + excess).min(h - 1)
        };
        for col in 0..pw {
            let src_col = if col < w {
                col
            } else {
                let excess = col - w;
                w.saturating_sub(1 + excess).min(w - 1)
            };
            out[row * pw + col] = plane[src_row * w + src_col];
        }
    }
    (out, pw, ph)
}

/// Forward 1-D Haar on a slice of length `n` (must be even), in-place.
/// Stores averages in [0..n/2) and differences in [n/2..n).
fn haar1d_forward(buf: &mut [f32]) {
    let n = buf.len();
    debug_assert!(n.is_multiple_of(2));
    let half = n / 2;
    let mut tmp = vec![0.0f32; n];
    for i in 0..half {
        let a = buf[2 * i];
        let b = buf[2 * i + 1];
        tmp[i] = (a + b) * 0.5;
        tmp[i + half] = (a - b) * 0.5;
    }
    buf.copy_from_slice(&tmp);
}

/// Inverse 1-D Haar on a slice of length `n` (must be even), in-place.
fn haar1d_inverse(buf: &mut [f32]) {
    let n = buf.len();
    debug_assert!(n.is_multiple_of(2));
    let half = n / 2;
    let mut tmp = vec![0.0f32; n];
    for i in 0..half {
        let a = buf[i]; // average
        let b = buf[i + half]; // difference
        tmp[2 * i] = a + b;
        tmp[2 * i + 1] = a - b;
    }
    buf.copy_from_slice(&tmp);
}

/// Forward 2-D Haar for one level on a (pw × ph) plane,
/// operating on the top-left (bw × bh) LL subband.
fn haar2d_forward(plane: &mut [f32], pw: usize, bw: usize, bh: usize) {
    // Row-wise
    for row in 0..bh {
        let start = row * pw;
        haar1d_forward(&mut plane[start..start + bw]);
    }
    // Column-wise — need a temporary column buffer
    let mut col_buf = vec![0.0f32; bh];
    for col in 0..bw {
        for row in 0..bh {
            col_buf[row] = plane[row * pw + col];
        }
        haar1d_forward(&mut col_buf);
        for row in 0..bh {
            plane[row * pw + col] = col_buf[row];
        }
    }
}

/// Inverse 2-D Haar for one level, restoring (bw × bh) from its subbands.
fn haar2d_inverse(plane: &mut [f32], pw: usize, bw: usize, bh: usize) {
    // Column-wise
    let mut col_buf = vec![0.0f32; bh];
    for col in 0..bw {
        for row in 0..bh {
            col_buf[row] = plane[row * pw + col];
        }
        haar1d_inverse(&mut col_buf);
        for row in 0..bh {
            plane[row * pw + col] = col_buf[row];
        }
    }
    // Row-wise
    for row in 0..bh {
        let start = row * pw;
        haar1d_inverse(&mut plane[start..start + bw]);
    }
}

const N_LEVELS: usize = 2;

/// Soft-threshold all detail subbands of a (pw × ph) padded plane.
fn threshold_detail_bands(plane: &mut [f32], pw: usize, ph: usize, base_threshold: f32) {
    // After N_LEVELS of forward transforms, the LL subband occupies
    // [0..bw) × [0..bh) in the padded grid.
    let bw_ll = pw >> N_LEVELS;
    let bh_ll = ph >> N_LEVELS;

    // Walk levels from finest (level 0) to coarsest (level N_LEVELS-1).
    // At level l the subband block size is (pw >> l) × (ph >> l).
    for l in 0..N_LEVELS {
        let block_w = pw >> l;
        let block_h = ph >> l;
        let half_w = block_w / 2;
        let half_h = block_h / 2;
        let threshold = base_threshold * (1u32 << l) as f32;

        // Three detail subbands:
        //   LH: col in [0,half_w), row in [half_h, block_h)
        //   HL: col in [half_w, block_w), row in [0, half_h)
        //   HH: col in [half_w, block_w), row in [half_h, block_h)
        for row in 0..block_h {
            for col in 0..block_w {
                // Skip the LL subband at the coarsest level
                if col < bw_ll && row < bh_ll {
                    continue;
                }
                // Only touch this level's subbands, not finer ones
                // (finer subbands are in [0..half_w)×[0..half_h) already
                //  thresholded at a finer level)
                let in_ll = col < half_w && row < half_h;
                if in_ll {
                    continue;
                }
                let idx = row * pw + col;
                plane[idx] = soft_threshold(plane[idx], threshold);
            }
        }
    }
}

/// Apply wavelet NR to a single YCbCr plane (in-place).
/// `strength` controls the base threshold magnitude.
fn apply_wavelet_nr(plane: &mut [f32], w: usize, h: usize, strength: f32, is_luma: bool) {
    if strength == 0.0 {
        return;
    }
    let base_threshold = if is_luma {
        strength * 255.0 * 0.05
    } else {
        strength * 255.0 * 0.08
    };

    let (mut padded, pw, ph) = pad_to_pow2(plane, w, h);

    // Forward transform
    let mut bw = pw;
    let mut bh = ph;
    for _ in 0..N_LEVELS {
        haar2d_forward(&mut padded, pw, bw, bh);
        bw /= 2;
        bh /= 2;
    }

    // Threshold
    threshold_detail_bands(&mut padded, pw, ph, base_threshold);

    // Inverse transform
    for l in 0..N_LEVELS {
        let level = N_LEVELS - 1 - l;
        let bw_inv = pw >> level;
        let bh_inv = ph >> level;
        haar2d_inverse(&mut padded, pw, bw_inv, bh_inv);
    }

    // Crop back to original size
    for row in 0..h {
        for col in 0..w {
            plane[row * w + col] = padded[row * pw + col];
        }
    }
}

// ---------------------------------------------------------------------------
// Sobel gradient for detail preservation masking
// ---------------------------------------------------------------------------

fn compute_sobel_y(y_plane: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut grad = vec![0.0f32; w * h];
    for row in 0..h {
        for col in 0..w {
            let get = |r: isize, c: isize| -> f32 {
                let r = r.clamp(0, h as isize - 1) as usize;
                let c = c.clamp(0, w as isize - 1) as usize;
                y_plane[r * w + c]
            };
            let r = row as isize;
            let c = col as isize;
            let gx = -get(r - 1, c - 1) + get(r - 1, c + 1) - 2.0 * get(r, c - 1)
                + 2.0 * get(r, c + 1)
                - get(r + 1, c - 1)
                + get(r + 1, c + 1);
            let gy = -get(r - 1, c - 1) - 2.0 * get(r - 1, c) - get(r - 1, c + 1)
                + get(r + 1, c - 1)
                + 2.0 * get(r + 1, c)
                + get(r + 1, c + 1);
            grad[row * w + col] = (gx * gx + gy * gy).sqrt();
        }
    }
    grad
}

fn apply_detail_mask(out: &mut [f32], orig: &[f32], grad: &[f32], detail_preservation: f32) {
    for i in 0..out.len() {
        let g_norm = (grad[i] / 128.0).clamp(0.0, 1.0);
        let mask = g_norm * detail_preservation;
        // lerp: out = (1-mask)*out + mask*orig
        out[i] = out[i] + mask * (orig[i] - out[i]);
    }
}

// ---------------------------------------------------------------------------
// Non-Local Means
// ---------------------------------------------------------------------------

struct NlmParams {
    luma_h: f32,
    color_h: f32,
    patch_r: usize,
    search_r: usize,
}

/// Apply NLM to three YCbCr planes simultaneously.
/// Returns denoised (Y, Cb, Cr) planes.
///
/// Polls [`cancel::is_requested`] at the start of every output row.  When a
/// cancel is pending, the remaining rows return a zero-filled placeholder and
/// the caller detects this via the post-collect `is_requested` check and
/// returns `RasterError::Cancelled`.  This keeps the rayon workers from
/// finishing a full minute of work after the user has asked to abort.
fn apply_nlm(
    y_in: &[f32],
    cb_in: &[f32],
    cr_in: &[f32],
    w: usize,
    h: usize,
    params: &NlmParams,
) -> RasterResult<(Vec<f32>, Vec<f32>, Vec<f32>)> {
    let luma_h = params.luma_h;
    let color_h = params.color_h;
    let patch_r = params.patch_r;
    let search_r = params.search_r;
    let luma_h2 = luma_h * luma_h;
    let color_h2 = color_h * color_h;
    let patch_size = (2 * patch_r + 1) * (2 * patch_r + 1);
    let patch_norm = 1.0 / patch_size as f32;

    let get_y = |r: usize, c: usize| y_in[r * w + c];
    let get_cb = |r: usize, c: usize| cb_in[r * w + c];
    let get_cr = |r: usize, c: usize| cr_in[r * w + c];

    // Output planes — compute per-row in parallel.
    // Each row is self-contained (reads from shared read-only y_in/cb_in/cr_in).
    let mut out_y = vec![0.0f32; w * h];
    let mut out_cb = vec![0.0f32; w * h];
    let mut out_cr = vec![0.0f32; w * h];

    // We collect row results (Vec of (y_row, cb_row, cr_row)) in parallel.
    let rows: Vec<(Vec<f32>, Vec<f32>, Vec<f32>)> = (0..h)
        .into_par_iter()
        .map(|py| {
            // Cooperative cancellation: bail out of this row immediately and
            // let the caller detect the cancel after the parallel collect.
            if cancel::is_requested() {
                return (vec![0.0f32; w], vec![0.0f32; w], vec![0.0f32; w]);
            }
            let mut row_y = vec![0.0f32; w];
            let mut row_cb = vec![0.0f32; w];
            let mut row_cr = vec![0.0f32; w];

            for px in 0..w {
                let mut sum_wy = 0.0f32;
                let mut sum_wc = 0.0f32;
                let mut acc_y = 0.0f32;
                let mut acc_cb = 0.0f32;
                let mut acc_cr = 0.0f32;

                let qy_lo = py.saturating_sub(search_r);
                let qy_hi = (py + search_r + 1).min(h);
                let qx_lo = px.saturating_sub(search_r);
                let qx_hi = (px + search_r + 1).min(w);

                for qy in qy_lo..qy_hi {
                    for qx in qx_lo..qx_hi {
                        // Compute patch distance for Y
                        let mut dist_y = 0.0f32;
                        let mut dist_c = 0.0f32;
                        for dy in -(patch_r as isize)..=(patch_r as isize) {
                            for dx in -(patch_r as isize)..=(patch_r as isize) {
                                let pr = (py as isize + dy).clamp(0, h as isize - 1) as usize;
                                let pc = (px as isize + dx).clamp(0, w as isize - 1) as usize;
                                let qr = (qy as isize + dy).clamp(0, h as isize - 1) as usize;
                                let qc = (qx as isize + dx).clamp(0, w as isize - 1) as usize;

                                let dy_val = get_y(pr, pc) - get_y(qr, qc);
                                dist_y += dy_val * dy_val;

                                let dcb = get_cb(pr, pc) - get_cb(qr, qc);
                                let dcr = get_cr(pr, pc) - get_cr(qr, qc);
                                dist_c += dcb * dcb + dcr * dcr;
                            }
                        }
                        dist_y *= patch_norm;
                        dist_c *= patch_norm;

                        let wy = (-dist_y / luma_h2.max(1e-9)).exp();
                        let wc = (-dist_c / color_h2.max(1e-9)).exp();

                        acc_y += wy * get_y(qy, qx);
                        sum_wy += wy;

                        acc_cb += wc * get_cb(qy, qx);
                        acc_cr += wc * get_cr(qy, qx);
                        sum_wc += wc;
                    }
                }

                row_y[px] = if sum_wy > 1e-9 {
                    acc_y / sum_wy
                } else {
                    get_y(py, px)
                };
                row_cb[px] = if sum_wc > 1e-9 {
                    acc_cb / sum_wc
                } else {
                    get_cb(py, px)
                };
                row_cr[px] = if sum_wc > 1e-9 {
                    acc_cr / sum_wc
                } else {
                    get_cr(py, px)
                };
            }
            (row_y, row_cb, row_cr)
        })
        .collect();

    if cancel::is_requested() {
        return Err(RasterError::Cancelled);
    }

    for (py, (ry, rcb, rcr)) in rows.into_iter().enumerate() {
        let base = py * w;
        out_y[base..base + w].copy_from_slice(&ry);
        out_cb[base..base + w].copy_from_slice(&rcb);
        out_cr[base..base + w].copy_from_slice(&rcr);
    }

    Ok((out_y, out_cb, out_cr))
}

// ---------------------------------------------------------------------------
// Operation impl
// ---------------------------------------------------------------------------

impl NoiseReductionOp {
    fn apply_inner(&self, image: Image) -> RasterResult<Image> {
        let w = image.width as usize;
        let h = image.height as usize;
        let n = w * h;

        // Convert to YCbCr planes
        let mut y_plane = vec![0.0f32; n];
        let mut cb_plane = vec![0.0f32; n];
        let mut cr_plane = vec![0.0f32; n];

        for i in 0..n {
            let r = image.data[i * 4];
            let g = image.data[i * 4 + 1];
            let b = image.data[i * 4 + 2];
            let (y, cb, cr) = rgb_to_ycbcr(r, g, b);
            y_plane[i] = y;
            cb_plane[i] = cb;
            cr_plane[i] = cr;
        }

        // Save originals for detail masking
        let y_orig = y_plane.clone();

        // Apply NR
        let (mut out_y, mut out_cb, mut out_cr) = match self.method {
            NrMethod::Wavelet => {
                let mut y = y_plane.clone();
                let mut cb = cb_plane.clone();
                let mut cr = cr_plane.clone();
                apply_wavelet_nr(&mut y, w, h, self.luma_strength, true);
                if cancel::is_requested() {
                    return Err(RasterError::Cancelled);
                }
                apply_wavelet_nr(&mut cb, w, h, self.color_strength, false);
                if cancel::is_requested() {
                    return Err(RasterError::Cancelled);
                }
                apply_wavelet_nr(&mut cr, w, h, self.color_strength, false);
                (y, cb, cr)
            }
            NrMethod::NonLocalMeans => {
                let nlm_params = NlmParams {
                    luma_h: self.luma_strength * 25.0,
                    color_h: self.color_strength * 25.0,
                    patch_r: 3,
                    search_r: 7,
                };
                apply_nlm(&y_plane, &cb_plane, &cr_plane, w, h, &nlm_params)?
            }
        };

        if cancel::is_requested() {
            return Err(RasterError::Cancelled);
        }

        // Detail preservation masking.
        // Gradient is computed from the *denoised* Y plane so that real edges
        // (which survive NR) are protected, while noise-induced false gradients
        // in the noisy input do not cause the mask to cancel out the NR effect.
        if self.detail_preservation > 0.0 {
            let grad = compute_sobel_y(&out_y, w, h);
            apply_detail_mask(&mut out_y, &y_orig, &grad, self.detail_preservation);
            // Also apply to chroma (using same gradient but chroma originals)
            apply_detail_mask(&mut out_cb, &cb_plane, &grad, self.detail_preservation);
            apply_detail_mask(&mut out_cr, &cr_plane, &grad, self.detail_preservation);
        }

        // Convert back to RGBA8
        let mut out = image.deep_clone();
        for i in 0..n {
            let y = out_y[i].clamp(0.0, 255.0);
            let cb = out_cb[i].clamp(0.0, 255.0);
            let cr = out_cr[i].clamp(0.0, 255.0);
            let (r, g, b) = ycbcr_to_rgb(y, cb, cr);
            out.data[i * 4] = r;
            out.data[i * 4 + 1] = g;
            out.data[i * 4 + 2] = b;
            // alpha unchanged (already deep_clone'd)
        }

        Ok(out)
    }
}

#[typetag::serde]
impl Operation for NoiseReductionOp {
    fn name(&self) -> &'static str {
        "noise_reduction"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn apply(&self, image: Image) -> RasterResult<Image> {
        self.apply_inner(image)
    }

    fn describe(&self) -> String {
        format!(
            "NR {:?}  L={:.2} C={:.2} D={:.2}",
            self.method, self.luma_strength, self.color_strength, self.detail_preservation
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_noisy(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        img.data.chunks_mut(4).enumerate().for_each(|(i, p)| {
            let base = 128u8;
            let noise = ((i * 7919 + 1) % 40) as u8;
            p[0] = base.saturating_add(noise);
            p[1] = base.saturating_add(((i * 6271) % 40) as u8);
            p[2] = base.saturating_add(((i * 4523) % 40) as u8);
            p[3] = 255;
        });
        img
    }

    fn variance(data: &[u8], channel: usize) -> f64 {
        let vals: Vec<f64> = data.chunks(4).map(|p| p[channel] as f64).collect();
        let mean = vals.iter().sum::<f64>() / vals.len() as f64;
        vals.iter().map(|&v| (v - mean).powi(2)).sum::<f64>() / vals.len() as f64
    }

    #[test]
    fn wavelet_reduces_noise() {
        let src = make_noisy(64, 64);
        let var_before = variance(&src.data, 0);
        let op = NoiseReductionOp {
            method: NrMethod::Wavelet,
            luma_strength: 0.5,
            color_strength: 0.5,
            detail_preservation: 0.0,
        };
        let out = op.apply(src).unwrap();
        let var_after = variance(&out.data, 0);
        assert!(
            var_after < var_before,
            "wavelet NR should reduce variance: before={var_before:.1} after={var_after:.1}"
        );
    }

    // Exercises the actual default parameters (detail_preservation=0.5) — the
    // same config the user sees in the GUI.  Previously the detail mask was
    // computed from the noisy input, causing noise to be classified as "detail"
    // and blended back, which made NR imperceptible at default settings.
    #[test]
    fn wavelet_default_params_reduces_noise() {
        let src = make_noisy(64, 64);
        let var_before = variance(&src.data, 0);
        let out = NoiseReductionOp::default().apply(src).unwrap();
        let var_after = variance(&out.data, 0);
        // Require at least 20% variance reduction so the effect is perceptible.
        assert!(
            var_after < var_before * 0.80,
            "default-param wavelet NR should visibly reduce noise: before={var_before:.1} after={var_after:.1}"
        );
    }

    #[test]
    fn nlm_reduces_noise() {
        let src = make_noisy(32, 32);
        let var_before = variance(&src.data, 0);
        let op = NoiseReductionOp {
            method: NrMethod::NonLocalMeans,
            luma_strength: 0.5,
            color_strength: 0.5,
            detail_preservation: 0.0,
        };
        let out = op.apply(src).unwrap();
        let var_after = variance(&out.data, 0);
        assert!(
            var_after < var_before,
            "NLM NR should reduce variance: before={var_before:.1} after={var_after:.1}"
        );
    }

    // Same as above but with detail_preservation enabled, matching defaults.
    #[test]
    fn nlm_default_params_reduces_noise() {
        let src = make_noisy(32, 32);
        let var_before = variance(&src.data, 0);
        let op = NoiseReductionOp {
            method: NrMethod::NonLocalMeans,
            luma_strength: 0.5,
            color_strength: 0.5,
            detail_preservation: 0.5,
        };
        let out = op.apply(src).unwrap();
        let var_after = variance(&out.data, 0);
        assert!(
            var_after < var_before * 0.80,
            "NLM NR with detail_preservation=0.5 should visibly reduce noise: before={var_before:.1} after={var_after:.1}"
        );
    }

    #[test]
    fn alpha_unchanged() {
        let mut src = Image::new(16, 16);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 100;
            p[1] = 120;
            p[2] = 80;
            p[3] = 77;
        });
        let op = NoiseReductionOp::default();
        let out = op.apply(src).unwrap();
        out.data
            .chunks(4)
            .for_each(|p| assert_eq!(p[3], 77, "alpha must be preserved"));
    }

    #[test]
    fn zero_strength_is_identity() {
        let src = make_noisy(16, 16);
        let orig = src.data.clone();
        let op = NoiseReductionOp {
            method: NrMethod::Wavelet,
            luma_strength: 0.0,
            color_strength: 0.0,
            detail_preservation: 1.0,
        };
        let out = op.apply(src).unwrap();
        // With zero strength AND full detail preservation, output should be very close to input
        let max_diff = orig
            .chunks(4)
            .zip(out.data.chunks(4))
            .map(|(a, b)| {
                (a[0] as i16 - b[0] as i16)
                    .unsigned_abs()
                    .max((a[1] as i16 - b[1] as i16).unsigned_abs())
                    .max((a[2] as i16 - b[2] as i16).unsigned_abs())
            })
            .max()
            .unwrap_or(0);
        assert!(
            max_diff <= 5,
            "near-zero strength should barely change the image, max_diff={max_diff}"
        );
    }
}
