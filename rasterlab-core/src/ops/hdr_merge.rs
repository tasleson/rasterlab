//! True HDR merge from bracketed exposures.
//!
//! Fuses 2+ aligned LDR exposures of the same scene into a single image
//! with extended dynamic range, then tone-maps the result back to sRGB
//! so it can be displayed and exported like any other 8-bit image.
//!
//! Algorithm (Debevec-style, simplified — no camera-response calibration):
//!
//! 1. Load every frame from disk (op is self-contained for replay).
//! 2. Verify all frames share the same dimensions.
//! 3. Decode sRGB gamma → linear RGB for each frame.
//! 4. **Estimate relative exposures** automatically.  For each frame, take
//!    the geometric mean of well-exposed linear luma (pixels where no
//!    channel is clipped or near-black) and divide by the darkest frame.
//! 5. **Merge to HDR radiance**.  For each output pixel:
//!    ```text
//!      w_i  = hat(srgb_luma_i)         // triangular weight, 0 at 0/1, 1 at 0.5
//!      rad  = Σ w_i · linear_i / e_i
//!             ─────────────────────
//!                    Σ w_i
//!    ```
//!    When every frame is clipped (Σ w_i → 0), fall back to the frame
//!    whose sRGB luma is closest to 0.5.
//! 6. **Tone map** the radiance buffer back to a displayable [0, 1] range
//!    with an auto-key Reinhard operator, then re-encode as sRGB 8-bit.
//!
//! The op ignores its `Image` argument and rebuilds the result from the
//! stored `image_paths` so it can round-trip through `.rlab` project
//! files without relying on cached pixel data.

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

/// sRGB luma values below this are treated as crushed (no signal) and
/// excluded from the exposure estimate.  Matched in linear light to
/// `srgb_to_linear(0.05) ≈ 0.0035`.
const VALID_LUMA_LO_SRGB: f32 = 0.05;
/// sRGB luma values above this are treated as blown (clipped) and
/// excluded from the exposure estimate.
const VALID_LUMA_HI_SRGB: f32 = 0.95;
/// Reinhard key value.  0.18 is the photographic middle-grey convention.
const REINHARD_KEY: f32 = 0.18;

// ── Public op ────────────────────────────────────────────────────────────────

/// Non-destructive true-HDR merge op.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HdrMergeOp {
    /// Absolute paths to the bracketed source frames, in any order.
    pub image_paths: Vec<String>,
}

impl HdrMergeOp {
    pub fn new(image_paths: Vec<String>) -> Self {
        Self { image_paths }
    }
}

#[typetag::serde]
impl Operation for HdrMergeOp {
    fn name(&self) -> &'static str {
        "hdr_merge"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn apply(&self, _image: Image) -> RasterResult<Image> {
        if self.image_paths.is_empty() {
            return Err(RasterError::InvalidParams(
                "HDR Merge: no image paths specified".into(),
            ));
        }

        let reg = FormatRegistry::with_builtins();
        let images: Vec<Image> = self
            .image_paths
            .iter()
            .map(|p| {
                if cancel::is_requested() {
                    return Err(RasterError::Cancelled);
                }
                reg.decode_file(std::path::Path::new(p)).map_err(|e| {
                    RasterError::InvalidParams(format!("HDR Merge: cannot load '{p}': {e}"))
                })
            })
            .collect::<RasterResult<_>>()?;

        if images.len() == 1 {
            return Ok(images.into_iter().next().unwrap());
        }

        let refs: Vec<&Image> = images.iter().collect();
        merge_images(&refs)
    }

    fn describe(&self) -> String {
        format!("HDR Merge ({} frames)", self.image_paths.len())
    }

    fn is_geometric(&self) -> bool {
        false
    }
}

// ── Per-frame decoded data ───────────────────────────────────────────────────

struct FrameData {
    /// Interleaved linear-light RGB, one entry per pixel (length = w*h*3).
    lin: Vec<f32>,
    /// sRGB-encoded luma per pixel, used to drive the triangular merge weight.
    luma_srgb: Vec<f32>,
    /// Linear-light luma per pixel, used to compute exposure ratios.
    luma_lin: Vec<f32>,
}

// ── Entry points shared by the op and unit tests ─────────────────────────────

/// Merge pre-loaded aligned frames into an 8-bit sRGB HDR image.
///
/// Exposed publicly so tests can exercise the fusion without going
/// through disk I/O.
pub fn merge_images(images: &[&Image]) -> RasterResult<Image> {
    if images.is_empty() {
        return Err(RasterError::InvalidParams(
            "HDR Merge: no frames provided".into(),
        ));
    }

    let (w, h) = (images[0].width, images[0].height);
    for (i, img) in images.iter().enumerate().skip(1) {
        if img.width != w || img.height != h {
            return Err(RasterError::InvalidParams(format!(
                "HDR Merge: image {i} has dimensions {}x{} but image 0 is {w}x{h}",
                img.width, img.height
            )));
        }
    }

    let radiance = merge_linear(images)?;
    let tone_mapped = reinhard_auto_key(&radiance);

    let mut out = Image::new(w, h);
    out.data
        .par_chunks_mut(4)
        .zip(tone_mapped.par_chunks_exact(3))
        .for_each(|(px, rad)| {
            px[0] = (super::linear_to_srgb(rad[0].clamp(0.0, 1.0)) * 255.0).round() as u8;
            px[1] = (super::linear_to_srgb(rad[1].clamp(0.0, 1.0)) * 255.0).round() as u8;
            px[2] = (super::linear_to_srgb(rad[2].clamp(0.0, 1.0)) * 255.0).round() as u8;
            px[3] = 255;
        });

    Ok(out)
}

/// Produce a flat `Vec<f32>` of interleaved RGB radiance values in
/// arbitrary linear units, one entry per `w * h` pixel.  Used by the
/// public merge and by unit tests that want to verify exposure
/// estimation before tone-mapping destroys the radiance scale.
pub fn merge_linear(images: &[&Image]) -> RasterResult<Vec<f32>> {
    let (w, h) = (images[0].width as usize, images[0].height as usize);

    // Pre-decode every frame to linear RGB and pre-compute both sRGB and
    // linear luma.  sRGB luma drives the per-pixel weight; linear luma
    // drives the exposure-ratio estimate.
    let frames: Vec<FrameData> = images
        .par_iter()
        .map(|img| {
            let mut lin = Vec::with_capacity(w * h * 3);
            let mut luma_srgb = Vec::with_capacity(w * h);
            let mut luma_lin = Vec::with_capacity(w * h);
            for px in img.data.chunks_exact(4) {
                let r = px[0] as f32 / 255.0;
                let g = px[1] as f32 / 255.0;
                let b = px[2] as f32 / 255.0;
                luma_srgb.push(0.2126 * r + 0.7152 * g + 0.0722 * b);
                let rl = super::srgb_to_linear(r);
                let gl = super::srgb_to_linear(g);
                let bl = super::srgb_to_linear(b);
                luma_lin.push(0.2126 * rl + 0.7152 * gl + 0.0722 * bl);
                lin.push(rl);
                lin.push(gl);
                lin.push(bl);
            }
            FrameData {
                lin,
                luma_srgb,
                luma_lin,
            }
        })
        .collect();

    if cancel::is_requested() {
        return Err(RasterError::Cancelled);
    }

    let exposures = estimate_exposures_from_frames(&frames);

    // Fuse per pixel.
    let mut radiance = vec![0.0f32; w * h * 3];
    radiance.par_chunks_mut(3).enumerate().for_each(|(i, out)| {
        let mut r_acc = 0.0f32;
        let mut g_acc = 0.0f32;
        let mut b_acc = 0.0f32;
        let mut w_sum = 0.0f32;
        let mut best_k = 0usize;
        let mut best_dist = f32::INFINITY;

        for (k, f) in frames.iter().enumerate() {
            let luma = f.luma_srgb[i];
            // Triangular hat: peaks at 0.5, zero at 0 and 1.
            let wt = (1.0 - (2.0 * luma - 1.0).abs()).max(0.0);
            let e = exposures[k].max(1e-6);
            let idx = i * 3;
            r_acc += wt * f.lin[idx] / e;
            g_acc += wt * f.lin[idx + 1] / e;
            b_acc += wt * f.lin[idx + 2] / e;
            w_sum += wt;

            let dist = (luma - 0.5).abs();
            if dist < best_dist {
                best_dist = dist;
                best_k = k;
            }
        }

        if w_sum > 1e-5 {
            let inv = 1.0 / w_sum;
            out[0] = r_acc * inv;
            out[1] = g_acc * inv;
            out[2] = b_acc * inv;
        } else {
            // Every frame is crushed or clipped at this pixel.  Pick
            // the frame whose sRGB luma is closest to mid-grey (ties
            // broken by smallest k for determinism) and divide by its
            // exposure to get a consistent linear estimate.
            let idx = i * 3;
            let e = exposures[best_k].max(1e-6);
            out[0] = frames[best_k].lin[idx] / e;
            out[1] = frames[best_k].lin[idx + 1] / e;
            out[2] = frames[best_k].lin[idx + 2] / e;
        }
    });

    Ok(radiance)
}

// ── Exposure estimation ──────────────────────────────────────────────────────

/// Estimate each frame's relative exposure by chaining pairwise ratios
/// across frames sorted from darkest to brightest.
///
/// Why pairwise: bracketed frames have non-overlapping well-exposed
/// regions (the dark frame's valid pixels are in the bright parts of
/// the scene; the bright frame's valid pixels are in the dark parts).
/// Averaging each frame's own valid pixels in isolation compares
/// different scene content and gives meaningless ratios.  Instead we
/// compare consecutive frames only in the scene regions that are
/// well-exposed in BOTH, then multiply the ratios to chain.
fn estimate_exposures_from_frames(frames: &[FrameData]) -> Vec<f32> {
    let n = frames.len();
    if n == 0 {
        return Vec::new();
    }

    // Sort frame indices by mean sRGB luma (darkest first).
    let mut order: Vec<usize> = (0..n).collect();
    let means: Vec<f32> = frames
        .iter()
        .map(|f| f.luma_srgb.iter().copied().sum::<f32>() / f.luma_srgb.len().max(1) as f32)
        .collect();
    order.sort_by(|&a, &b| means[a].partial_cmp(&means[b]).unwrap());

    let mut exp = vec![1.0f32; n];
    exp[order[0]] = 1.0;

    for pair in order.windows(2) {
        let (lo, hi) = (pair[0], pair[1]);
        let ratio = pairwise_ratio(&frames[lo], &frames[hi]);
        exp[hi] = exp[lo] * ratio;
    }

    // Normalise so the darkest exposure = 1.0 (already true by
    // construction, but guard against rounding).
    let min = exp.iter().cloned().fold(f32::INFINITY, f32::min).max(1e-6);
    exp.into_iter().map(|e| e / min).collect()
}

/// Median of `linear_luma_hi / linear_luma_lo` over pixels where both
/// frames are well-exposed in sRGB.  Falls back to the ratio of means
/// if no pixel qualifies.
fn pairwise_ratio(lo: &FrameData, hi: &FrameData) -> f32 {
    let n = lo.luma_srgb.len();
    let mut ratios: Vec<f32> = Vec::with_capacity(n);

    for i in 0..n {
        let slo = lo.luma_srgb[i];
        let shi = hi.luma_srgb[i];
        if slo > VALID_LUMA_LO_SRGB
            && slo < VALID_LUMA_HI_SRGB
            && shi > VALID_LUMA_LO_SRGB
            && shi < VALID_LUMA_HI_SRGB
        {
            let a = lo.luma_lin[i].max(1e-6);
            let b = hi.luma_lin[i].max(1e-6);
            ratios.push(b / a);
        }
    }

    if ratios.is_empty() {
        // Degenerate: no overlap in the well-exposed range.  Use the
        // ratio of linear-luma means as a best effort.
        let sum_lo: f32 = lo.luma_lin.iter().sum::<f32>() / lo.luma_lin.len().max(1) as f32;
        let sum_hi: f32 = hi.luma_lin.iter().sum::<f32>() / hi.luma_lin.len().max(1) as f32;
        return (sum_hi / sum_lo.max(1e-6)).max(1.0);
    }

    ratios.sort_by(|x, y| x.partial_cmp(y).unwrap());
    let mid = ratios.len() / 2;
    let r = if ratios.len().is_multiple_of(2) {
        0.5 * (ratios[mid - 1] + ratios[mid])
    } else {
        ratios[mid]
    };
    r.max(1.0)
}

// ── Tone mapping ─────────────────────────────────────────────────────────────

/// Auto-key Reinhard tone mapper.  Scales the radiance buffer so its
/// geometric-mean luminance sits at `REINHARD_KEY`, then applies the
/// per-channel Reinhard compression `x / (1 + x)`.
fn reinhard_auto_key(radiance: &[f32]) -> Vec<f32> {
    // Geometric mean of luminance, excluding exact zeros.
    let mut log_sum = 0.0f64;
    let mut count = 0usize;
    for rad in radiance.chunks_exact(3) {
        let l = 0.2126 * rad[0] + 0.7152 * rad[1] + 0.0722 * rad[2];
        if l > 1e-6 {
            log_sum += (l as f64).ln();
            count += 1;
        }
    }
    let geo = if count == 0 {
        REINHARD_KEY
    } else {
        (log_sum / count as f64).exp() as f32
    };
    let scale = REINHARD_KEY / geo.max(1e-6);

    radiance
        .par_iter()
        .map(|&v| {
            let s = v * scale;
            s / (1.0 + s)
        })
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::super::{linear_to_srgb, srgb_to_linear};
    use super::*;

    /// Encode a linear-light RGB value as an 8-bit sRGB pixel, clipping
    /// anything above 1.0 (what a real sensor does).
    fn encode_pixel(lin: [f32; 3]) -> [u8; 3] {
        [
            (linear_to_srgb(lin[0].clamp(0.0, 1.0)) * 255.0).round() as u8,
            (linear_to_srgb(lin[1].clamp(0.0, 1.0)) * 255.0).round() as u8,
            (linear_to_srgb(lin[2].clamp(0.0, 1.0)) * 255.0).round() as u8,
        ]
    }

    /// Build a synthetic "exposure" of a known linear radiance map by
    /// scaling every pixel and clipping to [0, 1].
    fn make_exposure(width: u32, height: u32, radiance: &[f32], exposure: f32) -> Image {
        let mut img = Image::new(width, height);
        for (i, chunk) in img.data.chunks_exact_mut(4).enumerate() {
            let r = radiance[i * 3] * exposure;
            let g = radiance[i * 3 + 1] * exposure;
            let b = radiance[i * 3 + 2] * exposure;
            let px = encode_pixel([r, g, b]);
            chunk[0] = px[0];
            chunk[1] = px[1];
            chunk[2] = px[2];
            chunk[3] = 255;
        }
        img
    }

    /// Build a linear-radiance map that spans ~10 stops across a
    /// horizontal gradient.  The darkest pixels are well below what a
    /// single LDR frame can capture at low ISO, and the brightest are
    /// well above 1.0 — so a single exposure must necessarily clip one
    /// end or the other.
    fn ten_stop_gradient(w: u32, h: u32) -> Vec<f32> {
        let wu = w as usize;
        let hu = h as usize;
        let mut r = Vec::with_capacity(wu * hu * 3);
        for _ in 0..hu {
            for x in 0..wu {
                let t = x as f32 / (wu - 1).max(1) as f32;
                // 2^(-5) .. 2^5  = 1/32 .. 32 linear radiance
                let rad = (-5.0 + 10.0 * t).exp2();
                r.push(rad);
                r.push(rad);
                r.push(rad);
            }
        }
        r
    }

    #[test]
    fn single_frame_passthrough() {
        // With one frame, the result should match the input byte-for-byte
        // (tone-mapping still runs but an LDR-only scene stays near the
        // "no-op" region of Reinhard after auto-key normalisation).
        // We just check dimensions and that alpha survives.
        let w = 32;
        let h = 24;
        let mut img = Image::new(w, h);
        img.data.chunks_exact_mut(4).for_each(|p| {
            p[0] = 120;
            p[1] = 130;
            p[2] = 140;
            p[3] = 255;
        });
        let out = merge_images(&[&img]).unwrap();
        assert_eq!(out.width, w);
        assert_eq!(out.height, h);
        assert!(out.data.chunks_exact(4).all(|p| p[3] == 255));
    }

    #[test]
    fn exposure_estimation_recovers_known_ratios() {
        // Construct three frames whose "true" exposures are 0.25x, 1x, 4x.
        // Use a gradient that has well-exposed pixels in every frame.
        let w = 128u32;
        let h = 1u32;
        let rad = ten_stop_gradient(w, h);

        let f1 = make_exposure(w, h, &rad, 0.25);
        let f2 = make_exposure(w, h, &rad, 1.0);
        let f3 = make_exposure(w, h, &rad, 4.0);

        // Drive exposure estimation through the public linear-merge path
        // and read the ratios back out.
        let _radiance = merge_linear(&[&f1, &f2, &f3]).unwrap();
        let frame_from = |img: &Image| -> FrameData {
            let mut lin = Vec::with_capacity(img.data.len() / 4 * 3);
            let mut luma_srgb = Vec::with_capacity(img.data.len() / 4);
            let mut luma_lin = Vec::with_capacity(img.data.len() / 4);
            for p in img.data.chunks_exact(4) {
                let r = p[0] as f32 / 255.0;
                let g = p[1] as f32 / 255.0;
                let b = p[2] as f32 / 255.0;
                luma_srgb.push(0.2126 * r + 0.7152 * g + 0.0722 * b);
                let rl = srgb_to_linear(r);
                let gl = srgb_to_linear(g);
                let bl = srgb_to_linear(b);
                luma_lin.push(0.2126 * rl + 0.7152 * gl + 0.0722 * bl);
                lin.push(rl);
                lin.push(gl);
                lin.push(bl);
            }
            FrameData {
                lin,
                luma_srgb,
                luma_lin,
            }
        };
        let frames = [frame_from(&f1), frame_from(&f2), frame_from(&f3)];
        let exps = estimate_exposures_from_frames(&frames);

        // Darkest frame normalised to 1.0; ratios should be ≈ 1, 4, 16.
        assert!((exps[0] - 1.0).abs() < 0.05, "e[0] = {}", exps[0]);
        assert!(
            (exps[1] / exps[0] - 4.0).abs() / 4.0 < 0.20,
            "e[1]/e[0] = {}, want ~4",
            exps[1] / exps[0]
        );
        assert!(
            (exps[2] / exps[0] - 16.0).abs() / 16.0 < 0.25,
            "e[2]/e[0] = {}, want ~16",
            exps[2] / exps[0]
        );
    }

    #[test]
    fn merged_radiance_monotone_on_gradient() {
        // Merge three bracketed exposures of a monotone-increasing
        // radiance gradient.  The recovered linear radiance must also be
        // monotone-increasing along the gradient axis, even where single
        // frames clip.
        let w = 128u32;
        let h = 1u32;
        let rad = ten_stop_gradient(w, h);

        let f1 = make_exposure(w, h, &rad, 0.25);
        let f2 = make_exposure(w, h, &rad, 1.0);
        let f3 = make_exposure(w, h, &rad, 4.0);

        let merged = merge_linear(&[&f1, &f2, &f3]).unwrap();

        // Sample every 4 px to dodge hat-weight ringing right at the
        // end-points; the overall shape must be strictly increasing.
        let sample: Vec<f32> = (0..(w as usize))
            .step_by(4)
            .map(|x| merged[x * 3])
            .collect();
        for pair in sample.windows(2) {
            assert!(
                pair[1] >= pair[0] * 0.99,
                "radiance regressed: {} → {}",
                pair[0],
                pair[1]
            );
        }
        // And should span at least a 10× dynamic range (we covered 10
        // stops, tone-mapping has not run yet).
        let min = sample.first().copied().unwrap_or(0.0).max(1e-6);
        let max = sample.last().copied().unwrap_or(0.0);
        assert!(
            max / min > 10.0,
            "merged radiance span {}× is too narrow for 10-stop input",
            max / min
        );
    }

    #[test]
    fn identical_frames_produce_stable_output() {
        // Three identical LDR frames should merge to essentially the same
        // scene (tone-mapped) — no crazy exposure blow-out.
        let w = 16u32;
        let h = 16u32;
        let mut f = Image::new(w, h);
        f.data.chunks_exact_mut(4).for_each(|p| {
            p[0] = 128;
            p[1] = 64;
            p[2] = 200;
            p[3] = 255;
        });
        let out = merge_images(&[&f, &f.deep_clone(), &f.deep_clone()]).unwrap();

        // Blue > Red > Green ordering must be preserved.
        let p = &out.data[0..4];
        assert!(p[2] > p[0] && p[0] > p[1], "channel order broke: {:?}", p);
    }

    #[test]
    fn rejects_mismatched_dimensions() {
        let a = Image::new(32, 16);
        let b = Image::new(32, 8);
        let err = merge_images(&[&a, &b]).unwrap_err();
        match err {
            RasterError::InvalidParams(msg) => assert!(msg.contains("dimensions")),
            other => panic!("wrong error: {other:?}"),
        }
    }
}
