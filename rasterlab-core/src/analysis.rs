//! Image analysis and automatic enhancement planning.
//!
//! This module powers **Smart Enhance**: instead of applying fixed preset
//! values, it measures the image (histograms, colour cast, chroma, sharpness)
//! and computes the correction each measurement calls for in closed form.
//! The result is an [`EnhancementPlan`] of ordinary pipeline ops with
//! concrete parameter values, so the user sees exactly what was applied and
//! can tweak or undo each step individually.
//!
//! The planner works in two measured stages rather than a blind loop:
//!
//! 1. **Cast + tone** are derived from per-channel histograms.  Because every
//!    planned colour op is a per-channel LUT, the planner *re-measures* the
//!    corrected image exactly and cheaply by pushing the histograms through
//!    the LUTs (`transform_histogram`) — no pixel pass, no approximation.
//! 2. **Saturation** is measured on the actual pixels *after* composing the
//!    planned LUTs (a strided sampling pass), because colour-cast removal
//!    changes chroma in ways channel histograms alone cannot capture.

use rayon::prelude::*;

use crate::image::Image;
use crate::ops::histogram::HistogramData;
use crate::ops::{ChannelLevelsOp, ChannelRange, LevelsOp, SaturationOp, SharpenOp};
use crate::traits::operation::Operation;

// ── Histogram helpers ─────────────────────────────────────────────────────────

/// Value below which `pct` of the pixels fall (0.0–1.0), as a bucket index.
pub fn percentile(hist: &[u64; 256], pct: f64) -> u8 {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return 0;
    }
    let target = ((total as f64 * pct).ceil() as u64).clamp(1, total);
    let mut cumsum = 0u64;
    for (i, &count) in hist.iter().enumerate() {
        cumsum += count;
        if cumsum >= target {
            return i as u8;
        }
    }
    255
}

/// Median bucket of a histogram.
pub fn median(hist: &[u64; 256]) -> u8 {
    percentile(hist, 0.5)
}

/// Exactly recompute a histogram as if every pixel were passed through `lut`.
pub fn transform_histogram(hist: &[u64; 256], lut: &[u8; 256]) -> [u64; 256] {
    let mut out = [0u64; 256];
    for (v, &count) in hist.iter().enumerate() {
        out[lut[v] as usize] += count;
    }
    out
}

fn variance(hist: &[u64; 256]) -> f64 {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let n = total as f64;
    let mut sum = 0.0;
    let mut sumsq = 0.0;
    for (v, &count) in hist.iter().enumerate() {
        let c = count as f64;
        sum += v as f64 * c;
        sumsq += (v as f64) * (v as f64) * c;
    }
    let mean = sum / n;
    (sumsq / n - mean * mean).max(0.0)
}

// ── Image statistics ──────────────────────────────────────────────────────────

/// Measurements Smart Enhance derives its plan from.
#[derive(Debug, Clone)]
pub struct ImageStats {
    /// Per-channel + luma histograms of the analysed image.
    pub hist: HistogramData,
    /// Variance of the 4-neighbour Laplacian of luma.  A standard blur
    /// estimate: soft images score low, crisp images high.  `None` when the
    /// image is too small for the kernel (needs at least 3×3).
    pub laplacian_variance: Option<f64>,
    /// Variance of the luma histogram — used to normalise
    /// `laplacian_variance` into a contrast-independent sharpness score.
    pub luma_variance: f64,
}

impl ImageStats {
    /// Contrast-normalised sharpness score.  Invariant under linear tonal
    /// stretch (both variances scale by the same factor), so measuring on
    /// the uncorrected image remains valid for the corrected one.
    pub fn sharpness(&self) -> Option<f64> {
        let lap = self.laplacian_variance?;
        if self.luma_variance < 1.0 {
            return None; // flat image — sharpness is meaningless
        }
        Some(lap / self.luma_variance)
    }

    pub fn compute(image: &Image) -> Self {
        let hist = HistogramData::compute(image);
        let luma_variance = variance(&hist.luma);
        let laplacian_variance = laplacian_variance(image);
        Self {
            hist,
            laplacian_variance,
            luma_variance,
        }
    }
}

/// Integer BT.709 luma, identical to the histogram computation.
#[inline]
fn luma_of(p: &[u8]) -> u8 {
    ((218u32 * p[0] as u32 + 732u32 * p[1] as u32 + 74u32 * p[2] as u32 + 512) >> 10) as u8
}

/// Variance of the 4-neighbour Laplacian over the luma plane.
fn laplacian_variance(image: &Image) -> Option<f64> {
    let w = image.width as usize;
    let h = image.height as usize;
    if w < 3 || h < 3 {
        return None;
    }

    // Luma plane (1 byte/pixel), built row-parallel.
    let mut luma = vec![0u8; w * h];
    luma.par_chunks_mut(w)
        .zip(image.data.par_chunks(image.row_stride()))
        .for_each(|(luma_row, px_row)| {
            for (l, p) in luma_row.iter_mut().zip(px_row.chunks_exact(4)) {
                *l = luma_of(p);
            }
        });

    // Interior rows in parallel; each row folds into a tiny (f64, f64, u64)
    // accumulator, well under the 64-byte fold-accumulator limit.
    let (sum, sumsq, count) = (1..h - 1)
        .into_par_iter()
        .map(|y| {
            let above = &luma[(y - 1) * w..y * w];
            let row = &luma[y * w..(y + 1) * w];
            let below = &luma[(y + 1) * w..(y + 2) * w];
            let mut sum = 0.0f64;
            let mut sumsq = 0.0f64;
            for x in 1..w - 1 {
                let lap = 4 * row[x] as i32
                    - row[x - 1] as i32
                    - row[x + 1] as i32
                    - above[x] as i32
                    - below[x] as i32;
                sum += lap as f64;
                sumsq += (lap as f64) * (lap as f64);
            }
            (sum, sumsq, (w - 2) as u64)
        })
        .reduce(
            || (0.0, 0.0, 0u64),
            |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2),
        );

    if count == 0 {
        return None;
    }
    let n = count as f64;
    let mean = sum / n;
    Some((sumsq / n - mean * mean).max(0.0))
}

// ── Enhancement planning ──────────────────────────────────────────────────────

/// Percentiles clipped when stretching each channel (matches Auto Enhance).
const CLIP_LO: f64 = 0.005;
const CLIP_HI: f64 = 0.995;
/// Midtone the corrected image is steered toward (fraction of full scale).
const TONE_TARGET: f32 = 0.45;
/// Mean chroma (max−min of RGB, 0–255) considered pleasantly saturated.
/// Calibrated against professionally restored photographs (~30).
const CHROMA_TARGET: f64 = 30.0;
/// Sharpness score at or above which no sharpening is added.  Real photos
/// score far lower than synthetic edges: a crisp print scan measures ~0.04,
/// a badly soft one ~0.004.
const SHARPNESS_GOOD: f64 = 0.030;
/// Sharpness score at or below which maximum sharpening is applied.
const SHARPNESS_SOFT: f64 = 0.003;
const SHARPEN_MAX: f32 = 1.2;

/// Concrete, per-image correction values produced by [`plan_enhancement`].
///
/// Each field is `None` when the analysis found nothing worth correcting.
#[derive(Debug, Clone)]
pub struct EnhancementPlan {
    /// Per-channel stretch + midtone neutralisation (colour-cast removal).
    pub channel_levels: Option<ChannelLevelsOp>,
    /// Overall midtone gamma steering median luma toward [`TONE_TARGET`].
    pub tone: Option<LevelsOp>,
    /// Saturation recovery for faded images.
    pub saturation: Option<SaturationOp>,
    /// Sharpening scaled to the measured softness.
    pub sharpen: Option<SharpenOp>,
}

impl EnhancementPlan {
    pub fn is_empty(&self) -> bool {
        self.channel_levels.is_none()
            && self.tone.is_none()
            && self.saturation.is_none()
            && self.sharpen.is_none()
    }

    /// The planned corrections as pipeline ops, in application order.
    pub fn into_ops(self) -> Vec<Box<dyn Operation>> {
        let mut ops: Vec<Box<dyn Operation>> = Vec::new();
        if let Some(op) = self.channel_levels {
            ops.push(Box::new(op));
        }
        if let Some(op) = self.tone {
            ops.push(Box::new(op));
        }
        if let Some(op) = self.saturation {
            ops.push(Box::new(op));
        }
        if let Some(op) = self.sharpen {
            ops.push(Box::new(op));
        }
        ops
    }

    /// One-line human summary, e.g. for the status bar.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.channel_levels.is_some() {
            parts.push("cast removal".to_string());
        }
        if let Some(t) = &self.tone {
            parts.push(format!("tone γ={:.2}", t.midtone));
        }
        if let Some(s) = &self.saturation {
            parts.push(format!("saturation ×{:.2}", s.saturation));
        }
        if let Some(s) = &self.sharpen {
            parts.push(format!("sharpen {:.2}", s.strength));
        }
        if parts.is_empty() {
            "no corrections needed".to_string()
        } else {
            parts.join(", ")
        }
    }
}

/// Analyse `image` and compute the corrections it needs.
///
/// See the module docs for the approach.  The plan is deterministic for a
/// given image and typically costs two passes over the pixels (statistics +
/// strided chroma sampling), a few hundred µs of histogram math aside.
pub fn plan_enhancement(image: &Image) -> EnhancementPlan {
    let stats = ImageStats::compute(image);
    plan_from_stats(image, &stats)
}

/// Planner core, split out so callers that already have stats can reuse them.
pub fn plan_from_stats(image: &Image, stats: &ImageStats) -> EnhancementPlan {
    let empty = EnhancementPlan {
        channel_levels: None,
        tone: None,
        saturation: None,
        sharpen: None,
    };
    let total: u64 = stats.hist.luma.iter().sum();
    if total == 0 {
        return empty;
    }

    let channels = [&stats.hist.red, &stats.hist.green, &stats.hist.blue];

    // ── Stage 1a: per-channel stretch (black/white points) ──────────────────
    // Endpoints within ~4/255 of full range are measurement noise on a
    // well-exposed image; snap them so a good image gets a no-op, not a
    // hair-thin stretch.
    const SNAP: f32 = 4.0 / 255.0;
    let mut ranges = [ChannelRange::identity(); 3];
    for (range, hist) in ranges.iter_mut().zip(channels) {
        let mut black = percentile(hist, CLIP_LO) as f32 / 255.0;
        let mut white = percentile(hist, CLIP_HI) as f32 / 255.0;
        if black <= SNAP {
            black = 0.0;
        }
        if white >= 1.0 - SNAP {
            white = 1.0;
        }
        if white > black {
            *range = ChannelRange::new(black, white, 1.0);
        }
    }

    // ── Stage 1b: re-measure through the stretch LUTs, neutralise midtones ──
    // Push each channel histogram through its stretch LUT (exact, no pixel
    // pass) and read the post-stretch medians.  A residual cast shows up as
    // diverging medians; a per-channel gamma pulls each toward their mean.
    let mut medians = [0.0f32; 3];
    for (m, (range, hist)) in medians.iter_mut().zip(ranges.iter().zip(channels)) {
        let stretched = transform_histogram(hist, &range.build_lut());
        *m = (median(&stretched) as f32 / 255.0).clamp(0.02, 0.98);
    }
    let mid_target = (medians[0] + medians[1] + medians[2]) / 3.0;
    for (range, m) in ranges.iter_mut().zip(medians) {
        // Solve m^(1/gamma) = target  →  gamma = ln(m) / ln(target).
        // The dead zone leaves mild, plausibly intentional warmth alone;
        // only clear casts get pulled toward neutral.
        let gamma = (m.ln() / mid_target.ln()).clamp(0.65, 1.5);
        range.gamma = if (gamma - 1.0).abs() < 0.04 {
            1.0
        } else {
            gamma
        };
    }

    let channel_levels = ChannelLevelsOp::new(ranges[0], ranges[1], ranges[2]);
    let channel_levels = (!channel_levels.is_identity()).then_some(channel_levels);

    // ── Stage 2: overall tone (uniform midtone gamma) ────────────────────────
    // After neutralisation every channel median sits at mid_target, so it
    // serves as the corrected image's midtone.  A dead zone around the
    // target keeps acceptable exposures untouched — like a human editor,
    // only clearly-dark images are lifted and clearly-bright ones tamed.
    let tone_gamma = if mid_target < TONE_TARGET - 0.03 {
        (mid_target.ln() / TONE_TARGET.ln()).clamp(0.8, 1.3)
    } else if mid_target > 0.58 {
        (mid_target.ln() / 0.52f32.ln()).clamp(0.8, 1.3)
    } else {
        1.0
    };
    let tone = ((tone_gamma - 1.0).abs() > 0.03).then(|| LevelsOp::new(0.0, 1.0, tone_gamma));

    // ── Stage 3: saturation, measured through the composed LUTs ─────────────
    // Compose stage-1 and stage-2 LUTs per channel, then sample the actual
    // post-correction chroma.  Cast removal can cut chroma drastically (a
    // colour cast makes even grey pixels look chromatic), so measuring the
    // source image would systematically overestimate remaining saturation.
    let tone_lut = tone
        .as_ref()
        .map(|t| t.build_lut())
        .unwrap_or_else(|| std::array::from_fn(|i| i as u8));
    let luts: Vec<[u8; 256]> = ranges
        .iter()
        .map(|r| {
            let ch = r.build_lut();
            std::array::from_fn(|v| tone_lut[ch[v] as usize])
        })
        .collect();
    let mean_chroma = sampled_mean_chroma(image, &luts[0], &luts[1], &luts[2]);
    let saturation = mean_chroma
        .filter(|&c| c > 1.0)
        .map(|c| (CHROMA_TARGET / c) as f32)
        .filter(|&s| s > 1.03)
        .map(|s| SaturationOp::new(s.min(1.5)));

    // ── Stage 4: sharpening scaled to measured softness ──────────────────────
    let sharpen = stats
        .sharpness()
        .filter(|&s| s < SHARPNESS_GOOD)
        .map(|s| {
            let t = ((SHARPNESS_GOOD - s) / (SHARPNESS_GOOD - SHARPNESS_SOFT)).clamp(0.0, 1.0);
            SharpenOp::new((t as f32 * SHARPEN_MAX * 100.0).round() / 100.0)
        })
        .filter(|op| op.strength > 0.05);

    EnhancementPlan {
        channel_levels,
        tone,
        saturation,
        sharpen,
    }
}

/// Mean chroma (max−min of RGB) of `image` as seen through per-channel LUTs,
/// sampled on a stride so cost stays negligible even at full resolution.
fn sampled_mean_chroma(
    image: &Image,
    r_lut: &[u8; 256],
    g_lut: &[u8; 256],
    b_lut: &[u8; 256],
) -> Option<f64> {
    const PIXEL_STRIDE: usize = 4;
    let row_stride = image.row_stride();
    if row_stride == 0 {
        return None;
    }

    let (sum, count) = image
        .data
        .par_chunks(row_stride)
        .map(|row| {
            let mut sum = 0u64;
            let mut count = 0u64;
            for p in row.chunks_exact(4).step_by(PIXEL_STRIDE) {
                let r = r_lut[p[0] as usize];
                let g = g_lut[p[1] as usize];
                let b = b_lut[p[2] as usize];
                let max = r.max(g).max(b);
                let min = r.min(g).min(b);
                sum += (max - min) as u64;
                count += 1;
            }
            (sum, count)
        })
        .reduce(|| (0, 0), |a, b| (a.0 + b.0, a.1 + b.1));

    (count > 0).then(|| sum as f64 / count as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient_image(w: u32, h: u32, f: impl Fn(usize) -> [u8; 3]) -> Image {
        let mut img = Image::new(w, h);
        for (i, p) in img.data.chunks_mut(4).enumerate() {
            let [r, g, b] = f(i);
            p[0] = r;
            p[1] = g;
            p[2] = b;
            p[3] = 255;
        }
        img
    }

    #[test]
    fn percentile_and_median_basics() {
        let mut hist = [0u64; 256];
        hist[10] = 50;
        hist[200] = 50;
        assert_eq!(percentile(&hist, 0.005), 10);
        assert_eq!(percentile(&hist, 0.995), 200);
        assert_eq!(median(&hist), 10); // cumulative hits 50% at bucket 10
    }

    #[test]
    fn transform_histogram_preserves_count() {
        let mut hist = [0u64; 256];
        hist[100] = 7;
        hist[30] = 3;
        let lut: [u8; 256] = std::array::from_fn(|v| (v / 2) as u8);
        let out = transform_histogram(&hist, &lut);
        assert_eq!(out.iter().sum::<u64>(), 10);
        assert_eq!(out[50], 7);
        assert_eq!(out[15], 3);
    }

    #[test]
    fn well_exposed_neutral_image_needs_little() {
        // Neutral grey gradient spanning the full range with lots of edges:
        // no cast, full contrast → no channel levels, no tone correction.
        let img = gradient_image(64, 64, |i| {
            let v = ((i * 7) % 256) as u8;
            [v, v, v]
        });
        let plan = plan_enhancement(&img);
        assert!(
            plan.channel_levels.is_none(),
            "neutral full-range image should need no cast removal: {:?}",
            plan.channel_levels
        );
        assert!(
            plan.saturation.is_none(),
            "grey image must not be saturated"
        );
    }

    #[test]
    fn strong_cast_yields_corrective_channel_levels() {
        // Simulated faded scan: red compressed high, blue compressed low.
        let img = gradient_image(64, 64, |i| {
            let v = ((i * 7) % 200) as u8;
            [80 + (v / 2), 40 + (v / 2), 20 + (v / 4)]
        });
        let plan = plan_enhancement(&img);
        let cl = plan
            .channel_levels
            .expect("cast image needs channel levels");
        // Red floor is 80 → black point must rise well above zero.
        assert!(cl.red.black > 0.2, "red black point: {}", cl.red.black);
        // Blue tops out at ~69 → white point must drop well below one.
        assert!(cl.blue.white < 0.5, "blue white point: {}", cl.blue.white);
    }

    #[test]
    fn dark_image_gets_brightening_tone() {
        // Full-range but heavily dark-skewed: stretch can't fix the median,
        // so a brightening midtone gamma is required.
        let img = gradient_image(64, 64, |i| {
            let v = if i % 16 == 0 { 255 } else { (i % 50) as u8 };
            [v, v, v]
        });
        let plan = plan_enhancement(&img);
        let t = plan.tone.expect("dark-skewed image needs tone correction");
        assert!(
            t.midtone > 1.0,
            "midtone gamma should brighten, got {}",
            t.midtone
        );
    }

    #[test]
    fn empty_image_yields_empty_plan() {
        let img = Image::new(0, 0);
        let plan = plan_enhancement(&img);
        assert!(plan.is_empty());
    }

    #[test]
    fn sharpness_high_for_noise_low_for_flat() {
        // Checkerboard = maximal edges; flat = none.
        let sharp = gradient_image(32, 32, |i| {
            let v = if (i + i / 32) % 2 == 0 { 0 } else { 255 };
            [v, v, v]
        });
        let s = ImageStats::compute(&sharp);
        assert!(s.sharpness().unwrap() > 1.0, "checkerboard is sharp");

        let flat = gradient_image(32, 32, |_| [128, 128, 128]);
        let f = ImageStats::compute(&flat);
        assert!(f.sharpness().is_none(), "flat image has no sharpness");
    }

    #[test]
    fn plan_ops_order_and_summary() {
        let img = gradient_image(64, 64, |i| {
            let v = ((i * 7) % 200) as u8;
            [80 + (v / 2), 40 + (v / 2), 20 + (v / 4)]
        });
        let plan = plan_enhancement(&img);
        assert!(!plan.summary().is_empty());
        let ops = plan.clone().into_ops();
        assert!(!ops.is_empty());
        // Cast removal must come first when present.
        assert_eq!(ops[0].name(), "channel_levels");
    }
}
