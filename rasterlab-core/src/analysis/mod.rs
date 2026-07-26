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
//!
//! Alongside the global measurements the analysis also builds a grid of
//! *regional* statistics ([`RegionalStats`]) describing how tone and chroma
//! vary across the frame.  Those descriptors are what let the planner see two
//! things a whole-frame histogram cannot:
//!
//! * A **uniform scan border**, excluded from the histograms the planner reads,
//!   so a Polaroid frame no longer drags the midtone neutralisation off target.
//! * A frame so **unevenly lit** that no single curve can serve both ends of
//!   it, which is the one situation calling for a local tone operator.  The
//!   planner answers that by pushing the tile medians through the LUTs the
//!   earlier stages will apply and asking whether regions remain far from the
//!   midtone in *both* directions afterwards.
//!
//! The saturation and local-tone stages interact, and the interaction is
//! handled explicitly.  Lifting a shadow makes the colour that was hidden in it
//! visible, so local tone raises apparent colourfulness by itself; boosting
//! saturation to the usual chroma target on top of that is too much.  The
//! saturation boost is therefore damped in proportion to the planned local
//! tone, reducing to the previous behaviour exactly when none is planned.

mod tiles;

use rayon::prelude::*;

pub use tiles::{BorderRegion, Rect, RegionalStats, TileStats};

use crate::image::Image;
use crate::ops::histogram::HistogramData;
use crate::ops::local_laplacian::{self, LocalLaplacianOp};
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

/// A border-excluded analysis region.
///
/// Present on [`ImageStats`] only when [`RegionalStats::uniform_border`]
/// confidently found a frame.  The histograms are measured over the picture
/// area alone, which is what the planner's cast and tone stages read.
#[derive(Debug, Clone)]
pub struct ContentRegion {
    /// The detected border and the rectangle it leaves.
    pub border: BorderRegion,
    /// Per-channel + luma histograms of [`BorderRegion::content_rect`] only.
    pub hist: HistogramData,
}

/// Measurements Smart Enhance derives its plan from.
#[derive(Debug, Clone)]
pub struct ImageStats {
    /// Per-channel + luma histograms of the whole image.
    pub hist: HistogramData,
    /// Variance of the 4-neighbour Laplacian of luma.  A standard blur
    /// estimate: soft images score low, crisp images high.  `None` when the
    /// image is too small for the kernel (needs at least 3×3).
    pub laplacian_variance: Option<f64>,
    /// Variance of the luma histogram — used to normalise
    /// `laplacian_variance` into a contrast-independent sharpness score.
    ///
    /// Deliberately measured over the *whole* frame even when a border is
    /// detected: the sharpness constants were calibrated against bordered
    /// scans, so switching this to the content region would invalidate them.
    pub luma_variance: f64,
    /// Tiled regional statistics.  `None` only for degenerate images.
    pub regional: Option<RegionalStats>,
    /// Border-excluded analysis region, `Some` only when a uniform border was
    /// confidently detected.  When it is `None` — the overwhelmingly common
    /// case — the planner reads `hist` and behaves exactly as it always has.
    pub content: Option<ContentRegion>,
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

    /// The histograms the planner's tone and cast stages should read: the
    /// border-excluded ones when a frame was detected, the whole-frame ones
    /// otherwise.
    pub fn analysis_hist(&self) -> &HistogramData {
        self.content.as_ref().map_or(&self.hist, |c| &c.hist)
    }

    /// The rectangle [`Self::analysis_hist`] covers.
    pub fn analysis_rect(&self, image: &Image) -> Rect {
        self.content
            .as_ref()
            .map_or_else(|| Rect::whole(image), |c| c.border.content_rect)
    }

    pub fn compute(image: &Image) -> Self {
        let hist = HistogramData::compute(image);
        let luma_variance = variance(&hist.luma);

        // One luma plane, three consumers: the Laplacian kernel, the tile
        // grid, and (indirectly) border detection.  Building it here rather
        // than inside `laplacian_variance` is what keeps the regional pass
        // from costing a second traversal of the pixel buffer.
        let luma = luma_plane(image);
        let w = image.width as usize;
        let h = image.height as usize;
        let laplacian_variance = luma
            .as_ref()
            .and_then(|plane| laplacian_variance_of_plane(plane, w, h));
        let regional = luma
            .as_ref()
            .and_then(|plane| RegionalStats::compute(image, plane));

        // Only pay for a second histogram when a border was actually found.
        let content = regional
            .as_ref()
            .and_then(|r| r.uniform_border())
            .map(|border| ContentRegion {
                hist: histogram_of_rect(image, border.content_rect),
                border,
            });

        Self {
            hist,
            laplacian_variance,
            luma_variance,
            regional,
            content,
        }
    }
}

/// Integer BT.709 luma, identical to the histogram computation.
#[inline]
fn luma_of(p: &[u8]) -> u8 {
    ((218u32 * p[0] as u32 + 732u32 * p[1] as u32 + 74u32 * p[2] as u32 + 512) >> 10) as u8
}

/// Full-resolution luma plane (1 byte/pixel), built row-parallel.
///
/// Shared by every measurement that only needs luminance, so the pixel buffer
/// is converted once.  `None` for a zero-size image.
fn luma_plane(image: &Image) -> Option<Vec<u8>> {
    let w = image.width as usize;
    let h = image.height as usize;
    if w == 0 || h == 0 {
        return None;
    }
    let mut luma = vec![0u8; w * h];
    luma.par_chunks_mut(w)
        .zip(image.data.par_chunks(image.row_stride()))
        .for_each(|(luma_row, px_row)| {
            for (l, p) in luma_row.iter_mut().zip(px_row.chunks_exact(4)) {
                *l = luma_of(p);
            }
        });
    Some(luma)
}

/// Per-channel + luma histograms of a sub-rectangle of `image`.
///
/// Only used on the border-excluded path, so it favours clarity over the
/// last few percent of speed.  Row groups keep the number of 8 KiB fold
/// accumulators proportional to the number of tasks, not to the row count.
fn histogram_of_rect(image: &Image, rect: Rect) -> HistogramData {
    type Acc = ([u64; 256], [u64; 256], [u64; 256], [u64; 256]);
    let zero: fn() -> Acc = || ([0u64; 256], [0u64; 256], [0u64; 256], [0u64; 256]);

    /// Rows per parallel task.  Large enough that the accumulator is reused
    /// across many pixels, small enough to keep every core busy.
    const ROWS_PER_TASK: usize = 64;

    let row_stride = image.row_stride();
    if rect.is_empty() || row_stride == 0 {
        let (red, green, blue, luma) = zero();
        return HistogramData {
            red,
            green,
            blue,
            luma,
        };
    }

    let x0 = rect.x as usize * 4;
    let x1 = (rect.x + rect.width) as usize * 4;
    let tasks = (rect.height as usize).div_ceil(ROWS_PER_TASK);
    let (red, green, blue, luma) = (0..tasks)
        .into_par_iter()
        .map(|t| {
            let y0 = rect.y as usize + t * ROWS_PER_TASK;
            let y1 = (y0 + ROWS_PER_TASK).min((rect.y + rect.height) as usize);
            let mut acc = zero();
            for y in y0..y1 {
                let row = &image.data[y * row_stride + x0..y * row_stride + x1];
                for p in row.chunks_exact(4) {
                    acc.0[p[0] as usize] += 1;
                    acc.1[p[1] as usize] += 1;
                    acc.2[p[2] as usize] += 1;
                    acc.3[luma_of(p) as usize] += 1;
                }
            }
            acc
        })
        .reduce(zero, |mut a, b| {
            for i in 0..256 {
                a.0[i] += b.0[i];
                a.1[i] += b.1[i];
                a.2[i] += b.2[i];
                a.3[i] += b.3[i];
            }
            a
        });

    HistogramData {
        red,
        green,
        blue,
        luma,
    }
}

/// Variance of the 4-neighbour Laplacian over an existing luma plane.
fn laplacian_variance_of_plane(luma: &[u8], w: usize, h: usize) -> Option<f64> {
    if w < 3 || h < 3 {
        return None;
    }

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

// ── Local-tone thresholds ─────────────────────────────────────────────────────
//
// The obvious trigger — "this frame has a wide tonal spread" — does not work.
// Measured across the repository's test images, the p90−p10 spread of tile
// medians runs from 52 (an evenly-lit scene) to 217 (a deliberately
// underexposed bracket), with ordinary photographs sitting at 106, 117 and 145.
// Almost anything with real content scores high, so a spread threshold fires on
// everything.
//
// What actually calls for local tone is narrower: after the best *global*
// correction this planner can make, the frame still has regions pinned at the
// bottom of the range and regions pinned at the top.  That is the one situation
// a single curve cannot fix, because lifting one end pushes the other further
// out.  So the test is applied to the tile medians *after* pushing them through
// the planned LUTs, which costs nothing — it is a lookup per tile.

/// Corrected tile median at or below which a region reads as too dark.
/// Corrected tile median at or above which a region reads as too bright.
///
/// These are deliberately *not* clipping thresholds.  Measured across the
/// repository's test images, the smaller of the two stuck fractions comes out
/// as (evenly-lit scenes first, then ones that want local tone):
///
/// | band     | focus | pano | showcase | airplane | hdr over | hdr mid |
/// |----------|-------|------|----------|----------|----------|---------|
/// | 45 / 215 | 0.00  | 0.00 | 0.02     | **0.03** | 0.09     | 0.18    |
/// | 85 / 175 | 0.03  | 0.00 | 0.10     | **0.31** | 0.18     | 0.27    |
/// | 100 / 160| 0.12  | 0.00 | 0.11     | 0.43     | 0.18     | 0.35    |
///
/// At clipping levels the backlit frame this feature exists for scores 0.03 —
/// indistinguishable from an evenly-lit scene — because its problem is not
/// clipping.  Nothing is pinned; the sky simply sits 120 levels above the
/// foreground and no single curve serves both.  Widening the bands separates it
/// cleanly.  Widening them further starts catching evenly-lit scenes (`focus`
/// jumps to 0.12), so 85/175 is the usable window.
const DARK_LEVEL: u8 = 85;
const BRIGHT_LEVEL: u8 = 175;
/// Fraction of tiles that must be far from the midtone at *both* ends before
/// local tone is worth its cost.  Set just under the evenly-lit scenes above.
const STUCK_FRACTION_MIN: f32 = 0.08;
/// Stuck fraction at which the planner reaches its strongest setting.
const STUCK_FRACTION_FULL: f32 = 0.30;
/// Strongest local tone the planner will choose unprompted.
///
/// Well short of the 1.0 the slider allows, and chosen by looking at results
/// rather than by argument: on a backlit mountain frame, 0.75 and 0.45 recover
/// a similar amount of shadow, but the stronger setting mostly buys more
/// *visible shadow noise and colour* along with it.  An automatic correction
/// should stop where the returns stop and leave the user room to push further.
const LOCAL_TONE_MAX: f32 = 0.45;

/// Concrete, per-image correction values produced by [`plan_enhancement`].
///
/// Each field is `None` when the analysis found nothing worth correcting.
#[derive(Debug, Clone)]
pub struct EnhancementPlan {
    /// Per-channel stretch + midtone neutralisation (colour-cast removal).
    pub channel_levels: Option<ChannelLevelsOp>,
    /// Overall midtone gamma steering median luma toward [`TONE_TARGET`].
    pub tone: Option<LevelsOp>,
    /// Local tone, planned only when the frame is lit so unevenly that no
    /// single global curve can serve both ends of it.
    pub local_tone: Option<LocalLaplacianOp>,
    /// Saturation recovery for faded images.
    pub saturation: Option<SaturationOp>,
    /// Sharpening scaled to the measured softness.
    pub sharpen: Option<SharpenOp>,
}

impl EnhancementPlan {
    pub fn is_empty(&self) -> bool {
        self.channel_levels.is_none()
            && self.tone.is_none()
            && self.local_tone.is_none()
            && self.saturation.is_none()
            && self.sharpen.is_none()
    }

    /// The planned corrections as pipeline ops, in application order.
    ///
    /// Local tone sits directly after the global tone: both are luminance
    /// moves, and the local one should refine the range the global one has
    /// already set rather than fight it.
    pub fn into_ops(self) -> Vec<Box<dyn Operation>> {
        let mut ops: Vec<Box<dyn Operation>> = Vec::new();
        if let Some(op) = self.channel_levels {
            ops.push(Box::new(op));
        }
        if let Some(op) = self.tone {
            ops.push(Box::new(op));
        }
        if let Some(op) = self.local_tone {
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
        if let Some(l) = &self.local_tone {
            parts.push(format!("local tone {:+.2}", l.tone));
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
        local_tone: None,
        saturation: None,
        sharpen: None,
    };
    // Cast and tone are measured over the analysis region: the whole frame
    // normally, the border-excluded picture area when a uniform frame was
    // confidently detected.  A large white Polaroid margin otherwise piles
    // tens of percent of the pixels into the top histogram buckets, dragging
    // the medians up and making midtone neutralisation over-correct.
    let hist = stats.analysis_hist();
    let total: u64 = hist.luma.iter().sum();
    if total == 0 {
        return empty;
    }

    let channels = [&hist.red, &hist.green, &hist.blue];

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

    // Compose stage-1 and stage-2 LUTs per channel.  Everything downstream
    // measures the image as seen through these rather than as loaded: cast
    // removal can cut chroma drastically (a colour cast makes even grey pixels
    // look chromatic), so measuring the source would overestimate what is left.
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
    // Restricted to the same region as the histograms: a neutral paper frame
    // has zero chroma, so including it would understate the picture's own
    // saturation and over-boost it.
    let region = stats.content.as_ref().map(|c| c.border.content_rect);

    // ── Stage 3: local tone, from the tile grid seen through the same LUTs ───
    // Asks the one question a global curve cannot answer for itself: once
    // stages 1–2 have done their best, is any of the frame still far from the
    // midtone at both ends at once?  See the constants above for why the
    // seemingly obvious "wide tonal range" test is not the right trigger.
    let local_tone = stats
        .regional
        .as_ref()
        .and_then(|regional| plan_local_tone(regional, &luts));

    // ── Stage 4: saturation, stood down in proportion to local tone ─────────
    // Mean chroma is measured through the LUTs as before.  What is new is the
    // damping: lifting a shadow makes the colour that was hiding in it visible,
    // so local tone raises apparent colourfulness on its own, without any
    // saturation op at all.  Boosting to the same chroma target on top of that
    // turned the shadowed rock of a backlit mountain frame vivid red.
    //
    // Damping the *boost* rather than the measurement is deliberate.  The
    // obvious fix — re-measure chroma on a thumbnail with local tone actually
    // applied — was tried and abandoned: it moved mean chroma only 10.5 -> 11.9
    // on that frame, nowhere near enough to change the outcome, because the
    // damage is concentrated in the lifted region while the mean is dominated
    // by a large neutral sky.  The mean is simply the wrong statistic to catch
    // this, so the interaction is handled where it is legible instead.
    //
    // At `tone == 0` this is exactly the previous behaviour, which is what
    // keeps the hand-calibrated restoration results intact.
    let mean_chroma = sampled_mean_chroma(image, region, &luts[0], &luts[1], &luts[2]);
    let local_strength = local_tone.as_ref().map_or(0.0, |op| op.tone.max(0.0));
    let saturation = mean_chroma
        .filter(|&c| c > 1.0)
        .map(|c| (CHROMA_TARGET / c) as f32)
        .map(|s| 1.0 + (s.min(1.5) - 1.0) * (1.0 - local_strength))
        .filter(|&s| s > 1.03)
        .map(SaturationOp::new);

    // ── Stage 5: sharpening scaled to measured softness ──────────────────────
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
        local_tone,
        saturation,
        sharpen,
    }
}

/// Decide whether the frame needs local tone, and how much.
///
/// `luts` are the composed per-channel LUTs the earlier stages will apply.
/// Each tile's median is pushed through them to ask what that region will
/// actually look like *after* the global correction — a region that is dark now
/// but will be lifted by the planned gamma is not a problem, and must not be
/// counted as one.
///
/// Local tone is planned only when both ends are occupied afterwards, because
/// that is exactly the case a single curve cannot resolve: any gamma that
/// rescues the shadows pushes the highlights further out, and vice versa.  A
/// frame that is merely dark, or merely bright, is left to the global stages.
///
/// Strength follows the *smaller* of the two stuck fractions — the binding
/// constraint.  A frame with half its tiles crushed but nothing blown does not
/// need local treatment; a frame with a tenth stuck at each end does.
fn plan_local_tone(regional: &RegionalStats, luts: &[[u8; 256]]) -> Option<LocalLaplacianOp> {
    let tiles = regional.tiles();
    if tiles.is_empty() || luts.len() < 3 {
        return None;
    }

    // What a neutral pixel at this level becomes under the planned LUTs.  The
    // channels can diverge (cast removal is per-channel), so weight them the
    // same way luma does rather than picking one.
    let corrected = |v: u8| -> f32 {
        let i = v as usize;
        0.2126 * luts[0][i] as f32 + 0.7152 * luts[1][i] as f32 + 0.0722 * luts[2][i] as f32
    };

    let mut dark = 0usize;
    let mut bright = 0usize;
    for tile in tiles {
        let level = corrected(tile.luma_median);
        if level <= DARK_LEVEL as f32 {
            dark += 1;
        } else if level >= BRIGHT_LEVEL as f32 {
            bright += 1;
        }
    }

    let n = tiles.len() as f32;
    let stuck = (dark as f32 / n).min(bright as f32 / n);
    if stuck < STUCK_FRACTION_MIN {
        return None;
    }

    let t =
        ((stuck - STUCK_FRACTION_MIN) / (STUCK_FRACTION_FULL - STUCK_FRACTION_MIN)).clamp(0.0, 1.0);
    // Round so the planned value reads as a deliberate setting in the edit
    // stack rather than a float artefact.
    let tone = ((t * LOCAL_TONE_MAX) * 100.0).round() / 100.0;
    (tone > 0.05).then(|| {
        // Detail is left alone: the planner already has a sharpening stage
        // measured from the actual blur, and stacking the two would double-
        // count fine contrast.
        LocalLaplacianOp::new(tone, 0.0, local_laplacian::DEFAULT_THRESHOLD)
    })
}

/// Mean chroma (max−min of RGB) of `image` as seen through per-channel LUTs,
/// sampled on a stride so cost stays negligible even at full resolution.
///
/// `rect` restricts the sample to a sub-rectangle; `None` means the whole
/// image, in which case the traversal is byte-for-byte the one this function
/// has always performed.
fn sampled_mean_chroma(
    image: &Image,
    rect: Option<Rect>,
    r_lut: &[u8; 256],
    g_lut: &[u8; 256],
    b_lut: &[u8; 256],
) -> Option<f64> {
    const PIXEL_STRIDE: usize = 4;
    let row_stride = image.row_stride();
    if row_stride == 0 {
        return None;
    }
    let rect = rect.unwrap_or_else(|| Rect::whole(image));
    if rect.is_empty() {
        return None;
    }
    let x0 = rect.x as usize * 4;
    let x1 = (rect.x + rect.width) as usize * 4;

    let (sum, count) = (rect.y as usize..(rect.y + rect.height) as usize)
        .into_par_iter()
        .map(|y| {
            let row = &image.data[y * row_stride + x0..y * row_stride + x1];
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

    /// Regression guard for the regional-statistics work: an image with no
    /// detectable border must produce exactly the plan the pre-tiling planner
    /// produced.  The expected values were captured from that planner; the
    /// comparison is exact because `analysis.rs`'s constants were
    /// hand-calibrated and even a one-LSB drift would invalidate them.
    #[test]
    fn plan_is_unchanged_for_images_without_a_border() {
        struct Case {
            name: &'static str,
            image: Image,
            channel_levels: Option<[[f32; 3]; 3]>,
            tone: Option<f32>,
            local_tone: Option<f32>,
            saturation: Option<f32>,
        }

        let cases = vec![
            Case {
                name: "linear cast",
                image: gradient_image(64, 64, |i| {
                    let v = ((i * 7) % 200) as u8;
                    [80 + (v / 2), 40 + (v / 2), 20 + (v / 4)]
                }),
                channel_levels: Some([
                    [0.313_725_5, 0.701_960_8, 1.0],
                    [0.156_862_75, 0.545_098_07, 1.0],
                    [0.078_431_375, 0.270_588_25, 1.0],
                ]),
                tone: None,
                local_tone: None,
                saturation: Some(1.5),
            },
            Case {
                name: "dark",
                image: gradient_image(64, 64, |i| {
                    let v = if i % 16 == 0 { 255 } else { (i % 50) as u8 };
                    [v, v, v]
                }),
                channel_levels: None,
                tone: Some(1.3),
                local_tone: None,
                saturation: None,
            },
            Case {
                // Exercises the midtone-neutralisation gammas, which are the
                // stage the border fix reroutes.
                name: "nonlinear cast",
                image: gradient_image(64, 64, |i| {
                    let v = ((i * 7) % 256) as f32 / 255.0;
                    [
                        (v.powf(0.7) * 255.0) as u8,
                        (v * 255.0) as u8,
                        (v.powf(1.4) * 255.0) as u8,
                    ]
                }),
                channel_levels: Some([
                    [0.019_607_844, 1.0, 0.712_675_33],
                    [0.0, 1.0, 1.0],
                    [0.0, 1.0, 1.380_544_2],
                ]),
                tone: None,
                local_tone: None,
                saturation: Some(1.5),
            },
            Case {
                name: "photo-like, odd dimensions",
                image: gradient_image(101, 47, |i| {
                    let (x, y) = ((i % 101) as u32, (i / 101) as u32);
                    let mut s =
                        x.wrapping_mul(1973).wrapping_add(y.wrapping_mul(9277)) ^ 0x5bf0_3635;
                    s ^= s >> 13;
                    s = s.wrapping_mul(0x9e37_79b9);
                    s ^= s >> 15;
                    let n = (s % 40) as i32 - 20;
                    let base = 40 + (y * 150 / 47) as i32 + (x * 40 / 101) as i32;
                    [
                        (base + n + 25).clamp(0, 255) as u8,
                        (base + n).clamp(0, 255) as u8,
                        (base + n - 18).clamp(0, 255) as u8,
                    ]
                }),
                channel_levels: Some([
                    [0.231_372_55, 1.0, 1.0],
                    [0.133_333_34, 0.894_117_65, 1.0],
                    [0.062_745_1, 0.823_529_4, 1.0],
                ]),
                tone: None,
                local_tone: Some(0.42),
                // Damped from 1.5 by the planned local tone: 1 + 0.5*(1-0.42).
                saturation: Some(1.29),
            },
        ];

        for case in cases {
            let stats = ImageStats::compute(&case.image);
            assert!(
                stats.content.is_none(),
                "{}: no border should be detected",
                case.name
            );
            let plan = plan_from_stats(&case.image, &stats);

            match (case.channel_levels, &plan.channel_levels) {
                (None, got) => assert!(got.is_none(), "{}: unexpected {got:?}", case.name),
                (Some(want), Some(got)) => {
                    let actual = [
                        [got.red.black, got.red.white, got.red.gamma],
                        [got.green.black, got.green.white, got.green.gamma],
                        [got.blue.black, got.blue.white, got.blue.gamma],
                    ];
                    assert_eq!(actual, want, "{}: channel levels drifted", case.name);
                }
                (Some(_), None) => panic!("{}: channel levels went missing", case.name),
            }
            assert_eq!(
                plan.tone.map(|t| t.midtone),
                case.tone,
                "{}: tone drifted",
                case.name
            );
            assert_eq!(
                plan.local_tone.map(|l| l.tone),
                case.local_tone,
                "{}: local tone drifted",
                case.name
            );
            assert_eq!(
                plan.saturation.map(|s| s.saturation),
                case.saturation,
                "{}: saturation drifted",
                case.name
            );
        }
    }

    /// A framed scan: the planner must measure the picture, not the mount.
    /// Without the fix, the white frame pushes every channel median up and
    /// the neutralisation/tone stages correct for a brightness the picture
    /// does not have.
    #[test]
    fn border_is_excluded_from_the_planner_histograms() {
        const W: u32 = 400;
        const H: u32 = 400;
        const MARGIN: u32 = 60;

        // Dark, red-cast picture inside a bright neutral paper frame.
        let picture = |x: u32, y: u32| -> [u8; 3] {
            let base = 15 + (x + y) * 70 / (W + H);
            [
                (base + 25).min(255) as u8,
                base as u8,
                base.saturating_sub(8) as u8,
            ]
        };
        let mut framed = Image::new(W, H);
        for y in 0..H {
            for x in 0..W {
                let [r, g, b] = if x < MARGIN || y < MARGIN || x >= W - MARGIN || y >= H - MARGIN {
                    [244, 244, 244]
                } else {
                    picture(x, y)
                };
                let o = framed.pixel_offset(x, y);
                framed.data[o] = r;
                framed.data[o + 1] = g;
                framed.data[o + 2] = b;
                framed.data[o + 3] = 255;
            }
        }

        let stats = ImageStats::compute(&framed);
        let content = stats
            .content
            .as_ref()
            .expect("uniform frame must be detected");
        assert!(content.border.level > 200.0);
        assert!(content.border.content_rect.x >= MARGIN / 2);

        // The border-excluded histograms must be much darker than the global
        // ones — that difference is the entire point of the fix.
        assert!(
            median(&content.hist.luma) < median(&stats.hist.luma),
            "content median {} should be below frame median {}",
            median(&content.hist.luma),
            median(&stats.hist.luma)
        );

        // And the plan must match the one computed from the cropped picture
        // alone, not the one the whole framed scan would have produced.
        let mut cropped = Image::new(W - 2 * MARGIN, H - 2 * MARGIN);
        for y in 0..cropped.height {
            for x in 0..cropped.width {
                let [r, g, b] = picture(x + MARGIN, y + MARGIN);
                let o = cropped.pixel_offset(x, y);
                cropped.data[o] = r;
                cropped.data[o + 1] = g;
                cropped.data[o + 2] = b;
                cropped.data[o + 3] = 255;
            }
        }
        let framed_plan = plan_from_stats(&framed, &stats);
        let cropped_plan = plan_enhancement(&cropped);
        let framed_white = framed_plan.channel_levels.map(|c| c.green.white);
        let cropped_white = cropped_plan.channel_levels.map(|c| c.green.white);
        assert!(
            framed_white.is_some() && cropped_white.is_some(),
            "both should stretch: {framed_white:?} vs {cropped_white:?}"
        );
        assert!(
            (framed_white.unwrap() - cropped_white.unwrap()).abs() < 0.1,
            "framed plan {framed_white:?} should track the cropped plan {cropped_white:?}"
        );
    }

    #[test]
    fn regional_stats_are_present_and_degenerate_sizes_are_safe() {
        let img = gradient_image(64, 64, |i| {
            let v = ((i * 7) % 256) as u8;
            [v, v, v]
        });
        let stats = ImageStats::compute(&img);
        let regional = stats.regional.expect("64x64 has a tile grid");
        assert_eq!(
            regional.tiles().len(),
            (regional.cols() * regional.rows()) as usize
        );

        for (w, h) in [(0, 0), (1, 1), (3, 3), (1, 40)] {
            let stats = ImageStats::compute(&Image::new(w, h));
            if w == 0 || h == 0 {
                assert!(stats.regional.is_none(), "{w}x{h} must have no grid");
            } else {
                assert!(stats.regional.is_some(), "{w}x{h} must have a grid");
            }
            assert!(stats.content.is_none(), "{w}x{h} must not detect a border");
        }
    }

    /// Build an image from a per-pixel closure of `(x, y)`.
    fn xy_image(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = f(x, y);
                let o = img.pixel_offset(x, y);
                img.data[o] = r;
                img.data[o + 1] = g;
                img.data[o + 2] = b;
                img.data[o + 3] = 255;
            }
        }
        img
    }

    fn texture(x: u32, y: u32) -> i32 {
        let mut s = x.wrapping_mul(1973) ^ y.wrapping_mul(9277) ^ 0x5bf0_3635;
        s ^= s >> 13;
        s = s.wrapping_mul(0x9e37_79b9);
        ((s >> 24) % 24) as i32 - 12
    }

    /// The case the whole feature exists for: a frame whose regions disagree
    /// about exposure gets local tone, and one that is merely contrasty but
    /// evenly lit does not.
    #[test]
    fn local_tone_is_planned_only_for_unevenly_lit_frames() {
        // Bright top, dark bottom — both bands far from the midtone, which no
        // single gamma can pull together.
        let backlit = xy_image(256, 256, |x, y| {
            let base = if y < 128 { 225 } else { 40 };
            let v = (base + texture(x, y)).clamp(0, 255) as u8;
            [v, v, v]
        });
        let plan = plan_enhancement(&backlit);
        let local = plan
            .local_tone
            .clone()
            .expect("a backlit frame must get local tone");
        assert!(
            local.tone > 0.3,
            "backlit frame deserves a substantial correction, got {}",
            local.tone
        );
        assert_eq!(local.detail, 0.0, "the planner must not ask for detail");

        // Local tone must refine the range the global stages set, so it has to
        // land after them — applying it first would compress a range that is
        // then stretched again.
        let names: Vec<&str> = plan.into_ops().iter().map(|op| op.name()).collect();
        let local_at = names.iter().position(|n| *n == "local_laplacian");
        assert!(local_at.is_some(), "local tone missing from ops: {names:?}");
        for earlier in ["channel_levels", "levels"] {
            if let Some(i) = names.iter().position(|n| *n == earlier) {
                assert!(i < local_at.unwrap(), "{earlier} must precede local tone");
            }
        }
        if let Some(i) = names.iter().position(|n| *n == "saturation") {
            assert!(local_at.unwrap() < i, "local tone must precede saturation");
        }

        // Same overall contrast range, but spread smoothly across the frame so
        // every region sits near the middle: a global curve handles this.
        let even = xy_image(256, 256, |x, y| {
            let v = (110 + texture(x, y)).clamp(0, 255) as u8;
            [v, v, v]
        });
        assert!(
            plan_enhancement(&even).local_tone.is_none(),
            "an evenly-lit frame must not get local tone"
        );
    }

    /// Planning local tone must stand the saturation boost down, because
    /// lifting shadows reveals colour on its own.  Left undamped, a backlit
    /// mountain frame came out with vivid red rock.
    #[test]
    fn local_tone_damps_the_saturation_boost() {
        // Unevenly lit *and* undersaturated, so both stages have something to
        // do and their interaction is visible.
        let backlit = xy_image(256, 256, |x, y| {
            let base = if y < 128 { 225 } else { 40 };
            let v = (base + texture(x, y)).clamp(0, 255);
            [(v + 6).min(255) as u8, v as u8, (v - 6).max(0) as u8]
        });
        let plan = plan_enhancement(&backlit);
        let local = plan.local_tone.clone().expect("should plan local tone");
        let damped = plan.saturation.map(|s| s.saturation);

        // The same frame without the local-tone stage is what the saturation
        // stage would have asked for on its own.
        let stats = ImageStats::compute(&backlit);
        let undamped = plan_from_stats(&backlit, &stats)
            .saturation
            .map(|s| s.saturation);
        assert_eq!(damped, undamped, "planner must be deterministic");

        if let Some(s) = damped {
            assert!(
                s < 1.5,
                "boost should be damped below the cap when local tone is planned, got {s}"
            );
            assert!(s >= 1.0, "damping must not invert the boost, got {s}");
        }
        assert!(local.tone > 0.0);
    }

    /// A frame that is uniformly dark needs a *global* lift, not local tone —
    /// only one end is stuck, so a curve can still fix it.
    #[test]
    fn one_sided_exposure_is_left_to_the_global_stages() {
        let dim = xy_image(256, 256, |x, y| {
            let v = (35 + texture(x, y)).clamp(0, 255) as u8;
            [v, v, v]
        });
        let plan = plan_enhancement(&dim);
        assert!(
            plan.local_tone.is_none(),
            "uniformly dark frame should be handled globally, got {:?}",
            plan.local_tone.map(|l| l.tone)
        );
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
