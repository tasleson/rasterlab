//! Frame alignment for the multi-image ops.
//!
//! Focus Stack fuses a bracket of frames pixel-for-pixel, which assumes every
//! frame projects the scene onto the sensor the same way.  A focus bracket does
//! not: racking the focus ring also moves the lens's effective focal length
//! ("focus breathing"), so each frame is a slightly different magnification of
//! the same scene.  1–3 % end to end is ordinary for a macro lens, and 1 % of a
//! 6000 px frame is 30 px of displacement in the corners — enough that fusing
//! the frames as they were shot smears every off-centre edge and hands the
//! focus measure a doubled contour to pick as its "sharpest" frame.
//!
//! This module fits the transform that puts one frame back onto another:
//! uniform scale, rotation and shift.  Scale is the term breathing needs;
//! rotation and shift come along for free and absorb the small drift between
//! shots that a tripod does not quite prevent.
//!
//! The fit is a Gauss–Newton (Lucas–Kanade) minimisation of the intensity
//! difference between the two frames over a luma pyramid, coarsest level first.
//! Two properties make it a better fit here than the feature matching
//! [`super::panorama`] uses:
//!
//! * It needs no corners.  Frames in a bracket are sharp in *different* places,
//!   so a keypoint that is crisp in one frame is often the blurred one in the
//!   next, and descriptor matching degrades exactly where the bracket is
//!   widest.
//! * Coarse pyramid levels are low-passed, and defocus is a low-pass
//!   difference, so the two frames look most alike at the level where the fit
//!   starts and the estimate is refined from there.
//!
//! Residuals are Huber-weighted, which pulls the fit away from the regions
//! where the frames disagree *because* of focus and onto the structure they
//! share.  A fifth parameter absorbs a constant brightness difference so a
//! stack shot on aperture priority does not drag the geometry around.
//!
//! What the fit will not do is claim more precision than it has.  Between a
//! sharp frame and a defocused one an intensity fit is biased by about a pixel
//! (see [`MIN_CORRECTION_PX`]), and resampling a frame to apply a correction
//! that small costs more sharpness than it recovers.  Every result is checked
//! against the identity and against what a lens can actually do, and anything
//! that fails is reported as no fit at all — the caller then fuses that frame
//! exactly as it was shot, which is what this op did before alignment existed.

use rayon::prelude::*;

use crate::image::Image;

// ── Tuning constants ─────────────────────────────────────────────────────────

/// The fit runs on a luma pyramid whose base level is halved until it fits
/// under this many pixels.
///
/// Four parameters fitted over hundreds of thousands of samples are limited by
/// how many samples agree, not by their resolution: halving the base costs a
/// factor of two in per-sample precision but nothing in the parameters, which
/// average it away.  A 45 MP frame would otherwise spend its whole alignment
/// budget on resolution the answer does not use.
const BASE_MAX_PIXELS: usize = 4_000_000;
/// The fit never looks at the frames at full resolution — the base level is
/// always at least one halving down.
///
/// Not a performance choice.  Resampling a frame *blurs* it, so on detail near
/// the sampling limit an intensity fit can lower its own residual by drifting
/// half a pixel off and letting interpolation wash the detail out.  A focus
/// bracket is exactly where that bites: the sharp frame carries fine detail the
/// defocused one does not, so "blur it a little" always looks like progress.
/// One halving averages that detail away in both frames before the fit ever
/// sees it, and leaves the real signal — everything larger than a pixel —
/// untouched.
const MIN_BASE_SHIFT: u32 = 1;
/// Coarsest pyramid level.  Halving stops once either side would fall below
/// this, so the top of the pyramid still holds enough structure to fit.
const MIN_LEVEL_DIM: usize = 24;
/// Most Gauss–Newton steps per pyramid level.  Convergence normally stops it
/// well short; this only bounds the pathological case.
const MAX_ITERS: usize = 24;
/// Roughly how many sample points each level contributes.  Sampling on a
/// stride keeps the cost per iteration flat across levels — and the parameters
/// are over-determined by four orders of magnitude either way.
const MAX_SAMPLES: usize = 400_000;
/// Fewest samples that may produce a step.  Below this the frame overlap is too
/// small to trust.
const MIN_SAMPLES: u64 = 64;
/// A step that moves the frame corner less than this (in level pixels) has
/// converged.
const CONVERGED_PX: f32 = 0.01;
/// Huber threshold, as a multiple of the previous iteration's mean absolute
/// residual.  Residuals past it are down-weighted to their reciprocal, which is
/// what keeps out-of-focus regions from steering the fit.
const HUBER_K: f32 = 2.0;
/// Levenberg damping added to the normal equations' diagonal, relative to its
/// own magnitude.  Only there to keep a degenerate (flat, or single-edge) level
/// from producing a wild step.
const DAMPING: f64 = 1e-6;

// ── Plausibility limits ──────────────────────────────────────────────────────
//
// A fit that lands outside these did not find the scene; it found a local
// minimum somewhere else.  Focus breathing is a few percent, and the drift
// between bracketed frames is small by construction — anything larger means the
// caller handed us frames that do not belong to one bracket, and leaving those
// frames alone is better than warping them by a wrong answer.

/// Largest magnification difference accepted from a fit.
const MAX_SCALE_DEV: f32 = 0.15;
/// Largest rotation accepted from a fit, in degrees.
const MAX_ROT_DEG: f32 = 5.0;
/// Largest shift accepted from a fit, as a fraction of the frame's diagonal.
const MAX_SHIFT_FRACTION: f32 = 0.15;
/// Smallest correction worth resampling for, in full-resolution pixels,
/// measured at the frame corner.
///
/// This is the fit's own accuracy floor, not a preference.  An intensity fit
/// between a sharp frame and a defocused one carries a bias: the difference
/// between the two is roughly the scene's Laplacian, which is not quite
/// orthogonal to its gradient wherever the defocus itself has structure — at
/// the edge of a region that is sharp in one frame and soft in the other,
/// which is every focus bracket.  On the repository's own three-frame test
/// bracket (`focus_top/mid/bot.png`, σ = 4 defocus over two thirds of each
/// frame, no real misalignment at all) the fit still asks for shifts of 0.7 to
/// 0.9 px.  Applying those would resample every frame for nothing and cost
/// exactly the sharpness the stack was assembled to keep — measured, an RMSE
/// against the all-in-focus reference of 17.7 rather than 2.3.
///
/// Anything real is far larger: 0.3 % of breathing, at the low end of what a
/// lens does, already displaces the corner of a 16 MP frame by 10 px.
const MIN_CORRECTION_PX: f32 = 1.5;

// ── The transform ────────────────────────────────────────────────────────────

/// A similarity transform — uniform scale, rotation, translation — mapping
/// *reference* frame coordinates to *source* frame coordinates.
///
/// Coordinates are continuous, with pixel `(x, y)` centred at `(x + 0.5,
/// y + 0.5)`, so the mapping between adjacent pyramid levels is exactly a
/// factor of two and carries no half-pixel correction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Similarity {
    /// `scale · cos θ`
    a: f32,
    /// `scale · sin θ`
    b: f32,
    tx: f32,
    ty: f32,
}

impl Similarity {
    pub(crate) const IDENTITY: Self = Self {
        a: 1.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    };

    /// Map a reference-frame point to the source frame.
    #[inline]
    pub(crate) fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x - self.b * y + self.tx,
            self.b * x + self.a * y + self.ty,
        )
    }

    /// Magnification of the source frame relative to the reference.
    pub(crate) fn scale(&self) -> f32 {
        self.a.hypot(self.b)
    }

    /// Rotation in degrees.
    pub(crate) fn rotation_deg(&self) -> f32 {
        self.b.atan2(self.a).to_degrees()
    }

    /// The same transform expressed one pyramid level finer: the linear part is
    /// scale-invariant, the translation is in pixels and doubles.
    fn finer(self) -> Self {
        Self {
            tx: self.tx * 2.0,
            ty: self.ty * 2.0,
            ..self
        }
    }

    fn is_finite(&self) -> bool {
        self.a.is_finite() && self.b.is_finite() && self.tx.is_finite() && self.ty.is_finite()
    }
}

// ── Public entry points ──────────────────────────────────────────────────────

/// Fit the transform that maps `reference` coordinates onto `frame`
/// coordinates, so that `frame` sampled through it lines up with `reference`.
///
/// Returns `None` when the frame is better left where it is: when the fit does
/// not beat the identity on its own residual, when the correction it asks for
/// is below the accuracy floor, when it lands outside the plausibility limits
/// above, or when the frames are too small to fit at all.  Callers treat `None`
/// as "use this frame unchanged".
pub(crate) fn estimate_similarity(reference: &Image, frame: &Image) -> Option<Similarity> {
    if reference.width != frame.width || reference.height != frame.height {
        return None;
    }

    let reference = Pyramid::build(reference);
    let source = Pyramid::build(frame);
    // A frame too small to low-pass is too small to fit: the protection
    // [`MIN_BASE_SHIFT`] buys is not optional, and nothing photographic is
    // under a hundred pixels on a side anyway.
    if reference.base_shift < MIN_BASE_SHIFT {
        return None;
    }
    let levels = reference.levels.len().min(source.levels.len());

    // Coarsest level first: each level starts from the level above's answer,
    // which is what lets a 30 px corner displacement be found by a fit that
    // only ever searches a pixel or two at a time.
    let mut t = Similarity::IDENTITY;
    for level in (0..levels).rev() {
        t = solve_level(&reference.levels[level], &source.levels[level], t);
        if level > 0 {
            t = t.finer();
        }
    }

    if !t.is_finite() {
        return None;
    }

    // Judge the fit on the base level, before it is scaled back up to full
    // resolution: a transform that does not explain the frames better than
    // leaving them alone is not an improvement, it is a guess.
    let (ref_base, src_base) = (&reference.levels[0], &source.levels[0]);
    if residual_rms(ref_base, src_base, t)?
        >= residual_rms(ref_base, src_base, Similarity::IDENTITY)?
    {
        return None;
    }

    for _ in 0..reference.base_shift {
        t = t.finer();
    }

    let (full_w, full_h) = (reference.full_w as f32, reference.full_h as f32);
    let correction = corner_shift(t, full_w, full_h);
    if (t.scale() - 1.0).abs() > MAX_SCALE_DEV
        || t.rotation_deg().abs() > MAX_ROT_DEG
        || correction > MAX_SHIFT_FRACTION * full_w.hypot(full_h)
        || correction < MIN_CORRECTION_PX
    {
        return None;
    }

    Some(t)
}

/// Resample `frame` through `t` into the reference frame's grid.
///
/// Returns the resampled image and its coverage: 1.0 where the output pixel
/// came from inside `frame`, 0.0 where the transform reached past its edge and
/// the pixel is an edge-clamped invention.  Callers must weight by coverage —
/// the invented border carries no scene detail, and its abrupt edge is a
/// contrast feature a focus measure would happily pick as the sharpest frame.
///
/// Catmull-Rom rather than bilinear: the frames are being aligned so that their
/// sharpness can be compared, and bilinear would spend a visible part of it on
/// the resampling itself.  On the pixel grid the kernel is an exact copy, so a
/// whole-pixel correction costs nothing at all.
pub(crate) fn warp_to_reference(frame: &Image, t: Similarity) -> (Image, Vec<f32>) {
    let w = frame.width as usize;
    let h = frame.height as usize;
    let mut out = Image::new(frame.width, frame.height);
    let mut coverage = vec![0.0f32; w * h];

    out.data
        .par_chunks_mut(w * 4)
        .zip(coverage.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, (row, cover))| {
            let ry = y as f32 + 0.5;
            for x in 0..w {
                let (sx, sy) = t.map(x as f32 + 0.5, ry);
                // Back to pixel-index space for sampling.
                let (sx, sy) = (sx - 0.5, sy - 0.5);
                let inside = sx >= 0.0 && sy >= 0.0 && sx <= (w - 1) as f32 && sy <= (h - 1) as f32;
                cover[x] = if inside { 1.0 } else { 0.0 };
                row[x * 4..x * 4 + 4]
                    .copy_from_slice(&super::resize::sample_bicubic(frame, sx, sy));
            }
        });

    (out, coverage)
}

// ── Luma pyramid ─────────────────────────────────────────────────────────────

struct Level {
    w: usize,
    h: usize,
    lum: Vec<f32>,
}

/// One sample of a level: the interpolated value and the gradient of that
/// interpolant.
struct Sample {
    value: f32,
    gx: f32,
    gy: f32,
}

impl Level {
    /// Bilinear sample with the gradient of the same interpolant, in
    /// pixel-index space.  `None` outside the one-pixel margin where the 2×2
    /// footprint would fall off the edge.
    #[inline]
    fn sample(&self, x: f32, y: f32) -> Option<Sample> {
        if !(x >= 0.0 && y >= 0.0 && x <= (self.w - 1) as f32 && y <= (self.h - 1) as f32) {
            return None;
        }
        let x0 = (x.floor() as usize).min(self.w - 2);
        let y0 = (y.floor() as usize).min(self.h - 2);
        let fx = x - x0 as f32;
        let fy = y - y0 as f32;

        let row0 = y0 * self.w + x0;
        let row1 = row0 + self.w;
        let p00 = self.lum[row0];
        let p10 = self.lum[row0 + 1];
        let p01 = self.lum[row1];
        let p11 = self.lum[row1 + 1];

        let top = p00 + (p10 - p00) * fx;
        let bot = p01 + (p11 - p01) * fx;
        Some(Sample {
            value: top + (bot - top) * fy,
            // Exact gradient of the bilinear interpolant — the four values are
            // already loaded, so it costs three subtractions over sampling a
            // separately-built gradient image, and no memory at all.
            gx: (1.0 - fy) * (p10 - p00) + fy * (p11 - p01),
            gy: bot - top,
        })
    }
}

struct Pyramid {
    /// Finest (base) level first, coarsest last.
    levels: Vec<Level>,
    /// Halvings between the frame and the base level.
    base_shift: u32,
    full_w: u32,
    full_h: u32,
}

impl Pyramid {
    fn build(image: &Image) -> Self {
        let mut w = image.width as usize;
        let mut h = image.height as usize;
        let mut lum = super::luma_f32(image);

        let mut base_shift = 0;
        while (base_shift < MIN_BASE_SHIFT || w * h > BASE_MAX_PIXELS)
            && w / 2 >= MIN_LEVEL_DIM
            && h / 2 >= MIN_LEVEL_DIM
        {
            (lum, w, h) = halve(&lum, w, h);
            base_shift += 1;
        }

        let mut levels = vec![Level { w, h, lum }];
        while w / 2 >= MIN_LEVEL_DIM && h / 2 >= MIN_LEVEL_DIM {
            let last = levels.last().expect("pyramid always has its base");
            let (lum, nw, nh) = halve(&last.lum, w, h);
            levels.push(Level { w: nw, h: nh, lum });
            w = nw;
            h = nh;
        }

        Self {
            levels,
            base_shift,
            full_w: image.width,
            full_h: image.height,
        }
    }
}

/// Downsample by 2 with a 2×2 box.  An odd trailing row or column is dropped,
/// which is why levels stay related by exactly a factor of two.
fn halve(src: &[f32], w: usize, h: usize) -> (Vec<f32>, usize, usize) {
    let nw = (w / 2).max(1);
    let nh = (h / 2).max(1);
    let mut out = vec![0.0f32; nw * nh];
    out.par_chunks_mut(nw).enumerate().for_each(|(y, row)| {
        let top = 2 * y * w;
        let bottom = top + w;
        for (x, cell) in row.iter_mut().enumerate() {
            let l = 2 * x;
            *cell =
                0.25 * (src[top + l] + src[top + l + 1] + src[bottom + l] + src[bottom + l + 1]);
        }
    });
    (out, nw, nh)
}

// ── Gauss–Newton fit ─────────────────────────────────────────────────────────

/// One level's fit: iterate until the step stops moving the frame.
fn solve_level(reference: &Level, source: &Level, start: Similarity) -> Similarity {
    let mut t = start;
    // A constant brightness difference between the frames is absorbed rather
    // than fitted geometrically; it is re-estimated from scratch per level.
    let mut bias = 0.0f32;
    // The first iteration is unweighted: the Huber threshold needs a residual
    // scale, and the only honest source of one is a pass over the data.
    let mut huber = f32::INFINITY;

    for _ in 0..MAX_ITERS {
        let Some(step) = gauss_newton_step(reference, source, t, bias, huber) else {
            break;
        };
        t = step.transform;
        bias = step.bias;
        huber = HUBER_K * step.mean_abs_residual.max(f32::EPSILON);
        if step.corner_shift < CONVERGED_PX {
            break;
        }
    }
    t
}

struct Step {
    transform: Similarity,
    bias: f32,
    mean_abs_residual: f32,
    /// How far the step moved the frame's furthest corner, in level pixels.
    corner_shift: f32,
}

fn gauss_newton_step(
    reference: &Level,
    source: &Level,
    t: Similarity,
    bias: f32,
    huber: f32,
) -> Option<Step> {
    let w = reference.w;
    let stride = sample_stride(w, reference.h);

    let normals = (0..reference.h)
        .into_par_iter()
        .step_by(stride)
        .fold(Normals::default, |mut acc, y| {
            let ry = y as f32 + 0.5;
            let row = &reference.lum[y * w..(y + 1) * w];
            for x in (0..w).step_by(stride) {
                let rx = x as f32 + 0.5;
                let (sx, sy) = t.map(rx, ry);
                let Some(s) = source.sample(sx - 0.5, sy - 0.5) else {
                    continue;
                };
                let r = s.value - row[x] - bias;
                let weight = if r.abs() > huber {
                    huber / r.abs()
                } else {
                    1.0
                };
                // ∂r/∂(a, b, tx, ty, bias) for x′ = a·x − b·y + tx,
                //                               y′ = b·x + a·y + ty.
                let j = [
                    s.gx * rx + s.gy * ry,
                    -s.gx * ry + s.gy * rx,
                    s.gx,
                    s.gy,
                    -1.0,
                ];
                acc.push(j, r, weight);
            }
            acc
        })
        .reduce(Normals::default, Normals::merge);

    if normals.count < MIN_SAMPLES {
        return None;
    }

    let delta = normals.solve()?;
    let transform = Similarity {
        a: t.a + delta[0] as f32,
        b: t.b + delta[1] as f32,
        tx: t.tx + delta[2] as f32,
        ty: t.ty + delta[3] as f32,
    };
    if !transform.is_finite() {
        return None;
    }

    Some(Step {
        transform,
        bias: bias + delta[4] as f32,
        mean_abs_residual: (normals.abs_r / normals.count as f64) as f32,
        corner_shift: corner_shift(
            Similarity {
                a: delta[0] as f32,
                b: delta[1] as f32,
                tx: delta[2] as f32,
                ty: delta[3] as f32,
            },
            reference.w as f32,
            reference.h as f32,
        ),
    })
}

/// Largest displacement the transform's *departure from identity* produces over
/// a `w × h` frame — measured at the corners, where a scale or rotation error
/// shows up first.
fn corner_shift(t: Similarity, w: f32, h: f32) -> f32 {
    [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)]
        .into_iter()
        .map(|(x, y)| {
            let (mx, my) = t.map(x, y);
            (mx - x).hypot(my - y)
        })
        .fold(0.0f32, f32::max)
}

/// Root-mean-square residual between the frames under `t`, with any constant
/// brightness difference removed.  `None` when too little of the frame overlaps
/// to compare.
fn residual_rms(reference: &Level, source: &Level, t: Similarity) -> Option<f32> {
    let w = reference.w;
    let stride = sample_stride(w, reference.h);

    let (sum, sum_sq, count) = (0..reference.h)
        .into_par_iter()
        .step_by(stride)
        .fold(
            || (0.0f64, 0.0f64, 0u64),
            |mut acc, y| {
                let ry = y as f32 + 0.5;
                let row = &reference.lum[y * w..(y + 1) * w];
                for x in (0..w).step_by(stride) {
                    let (sx, sy) = t.map(x as f32 + 0.5, ry);
                    let Some(s) = source.sample(sx - 0.5, sy - 0.5) else {
                        continue;
                    };
                    let r = (s.value - row[x]) as f64;
                    acc.0 += r;
                    acc.1 += r * r;
                    acc.2 += 1;
                }
                acc
            },
        )
        .reduce(|| (0.0, 0.0, 0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2));

    if count < MIN_SAMPLES {
        return None;
    }
    let n = count as f64;
    let mean = sum / n;
    Some((sum_sq / n - mean * mean).max(0.0).sqrt() as f32)
}

/// Sample every `stride`-th pixel in both axes, so a level contributes roughly
/// [`MAX_SAMPLES`] points however large it is.
fn sample_stride(w: usize, h: usize) -> usize {
    let pixels = w * h;
    if pixels <= MAX_SAMPLES {
        1
    } else {
        ((pixels as f32 / MAX_SAMPLES as f32).sqrt().ceil() as usize).max(1)
    }
}

// ── Normal equations ─────────────────────────────────────────────────────────

/// Accumulator for `JᵀWJ · Δ = −JᵀW r` over the sampled pixels.
///
/// Accumulated per row rather than per pixel: at 256 bytes it is far too large
/// to move through a `fold` once per sample.
#[derive(Clone, Copy, Default)]
struct Normals {
    /// Upper triangle of `JᵀWJ`; the lower half is filled in on solve.
    h: [[f64; 5]; 5],
    g: [f64; 5],
    abs_r: f64,
    count: u64,
}

impl Normals {
    #[inline]
    fn push(&mut self, j: [f32; 5], r: f32, weight: f32) {
        let jw: [f64; 5] = std::array::from_fn(|i| (j[i] * weight) as f64);
        for (row, (h_row, g)) in self.h.iter_mut().zip(&mut self.g).enumerate() {
            for (h, jc) in h_row[row..].iter_mut().zip(&j[row..]) {
                *h += jw[row] * *jc as f64;
            }
            *g += jw[row] * r as f64;
        }
        self.abs_r += r.abs() as f64;
        self.count += 1;
    }

    fn merge(mut self, other: Self) -> Self {
        for (mine, theirs) in self.h.iter_mut().flatten().zip(other.h.iter().flatten()) {
            *mine += theirs;
        }
        for (mine, theirs) in self.g.iter_mut().zip(other.g) {
            *mine += theirs;
        }
        self.abs_r += other.abs_r;
        self.count += other.count;
        self
    }

    /// Solve for the parameter step by Gaussian elimination with partial
    /// pivoting.  `None` if the system is degenerate — a level with no
    /// gradient to speak of, which happens on a blank sky.
    ///
    /// Indexed rather than iterated, like the homography solve in
    /// [`super::panorama`]: row and column arithmetic is the subject here.
    #[allow(clippy::needless_range_loop)]
    fn solve(&self) -> Option<[f64; 5]> {
        let mut aug = [[0.0f64; 6]; 5];
        for row in 0..5 {
            for col in 0..5 {
                aug[row][col] = if col >= row {
                    self.h[row][col]
                } else {
                    self.h[col][row]
                };
            }
            aug[row][row] *= 1.0 + DAMPING;
            aug[row][5] = -self.g[row];
        }

        for col in 0..5 {
            let pivot = (col..5).max_by(|&a, &b| {
                aug[a][col]
                    .abs()
                    .partial_cmp(&aug[b][col].abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })?;
            if aug[pivot][col].abs() < f64::EPSILON {
                return None;
            }
            aug.swap(col, pivot);
            for row in (col + 1)..5 {
                let factor = aug[row][col] / aug[col][col];
                for k in col..6 {
                    aug[row][k] -= factor * aug[col][k];
                }
            }
        }

        let mut x = [0.0f64; 5];
        for row in (0..5).rev() {
            let mut sum = aug[row][5];
            for col in (row + 1)..5 {
                sum -= aug[row][col] * x[col];
            }
            x[row] = sum / aug[row][row];
        }
        x.iter().all(|v| v.is_finite()).then_some(x)
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::test_utils::{box_blurred, magnified, split_halves, textured_scene as scene};

    /// The transform a frame magnified by `scale` about the centre and shifted
    /// by `(tx, ty)` has to be undone by — the answer the fit is judged
    /// against, written independently of how the fit reaches it.
    fn breathing(w: u32, h: u32, scale: f32, tx: f32, ty: f32) -> Similarity {
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        Similarity {
            a: scale,
            b: 0.0,
            tx: cx - scale * cx + tx,
            ty: cy - scale * cy + ty,
        }
    }

    fn assert_maps_like(got: Similarity, want: Similarity, w: u32, h: u32, tol: f32) {
        for (x, y) in [
            (0.5, 0.5),
            (w as f32 - 0.5, 0.5),
            (0.5, h as f32 - 0.5),
            (w as f32 - 0.5, h as f32 - 0.5),
            (w as f32 / 2.0, h as f32 / 2.0),
        ] {
            let (gx, gy) = got.map(x, y);
            let (wx, wy) = want.map(x, y);
            assert!(
                (gx - wx).abs() < tol && (gy - wy).abs() < tol,
                "at ({x}, {y}): fit maps to ({gx:.2}, {gy:.2}), want ({wx:.2}, {wy:.2})",
            );
        }
    }

    /// The reason this module exists: a frame shot at a different magnification
    /// has to be found and undone, to well under a pixel.
    #[test]
    fn recovers_a_magnification_difference() {
        let (w, h) = (320, 240);
        let reference = scene(w, h);
        // The frame as the camera recorded it: the scene 2 % larger, with a
        // pixel and a half of drift.
        let frame = magnified(&reference, 1.02, 1.5, -1.0);

        let fit = estimate_similarity(&reference, &frame).expect("fit");

        assert!(
            (fit.scale() - 1.02).abs() < 0.002,
            "scale {} should recover 1.02",
            fit.scale()
        );
        assert!(fit.rotation_deg().abs() < 0.2, "{}", fit.rotation_deg());
        assert_maps_like(fit, breathing(w, h, 1.02, 1.5, -1.0), w, h, 0.5);
    }

    /// Frames in a bracket differ by focus as well as magnification, and the
    /// focus difference is the larger signal.  The fit has to survive it —
    /// this is the case feature matching struggles with.
    #[test]
    fn recovers_magnification_through_a_focus_difference() {
        let (w, h) = (320, 240);
        let reference = scene(w, h);
        let frame = box_blurred(&magnified(&reference, 1.025, 0.0, 0.0), 3);

        let fit = estimate_similarity(&reference, &frame).expect("fit");

        assert!(
            (fit.scale() - 1.025).abs() < 0.004,
            "scale {} should recover 1.025 despite the defocus",
            fit.scale()
        );
    }

    /// A brightness difference between frames is ordinary in a bracket, and it
    /// must not be paid for with geometry.
    #[test]
    fn a_brightness_difference_does_not_move_the_fit() {
        let (w, h) = (320, 240);
        let reference = scene(w, h);
        let mut frame = magnified(&reference, 1.02, 0.0, 0.0);
        for pixel in frame.data.chunks_exact_mut(4) {
            for c in &mut pixel[..3] {
                *c = c.saturating_add(18);
            }
        }

        let fit = estimate_similarity(&reference, &frame).expect("fit");

        assert!(
            (fit.scale() - 1.02).abs() < 0.002,
            "scale {} should recover 1.02 despite the exposure difference",
            fit.scale()
        );
    }

    /// Frames that already line up must be left exactly alone: a fit that
    /// cannot beat the identity is reported as no fit at all, so the caller
    /// resamples nothing and the fusion stays bit-for-bit what it was.
    #[test]
    fn identical_frames_need_no_transform() {
        let reference = scene(200, 150);
        let frame = scene(200, 150);
        assert_eq!(estimate_similarity(&reference, &frame), None);
    }

    /// A bracket that needs no correction must get none.  The frames here are
    /// pixel-aligned and differ only by which half is defocused — but that
    /// difference is most of the residual, and the fit can always shave a
    /// little off it by nudging the frame.  Applying such a nudge resamples
    /// the whole frame to chase the fit's own bias, so anything under the
    /// accuracy floor is reported as no fit at all.
    #[test]
    fn a_defocus_difference_alone_is_not_a_misalignment() {
        let (w, h) = (480, 360);
        let sharp = scene(w, h);
        let soft = box_blurred(&sharp, 3);
        let near = split_halves(&sharp, &soft);
        let far = split_halves(&soft, &sharp);

        assert_eq!(estimate_similarity(&near, &far), None);
    }

    /// Detail at the sampling limit is the trap an intensity fit falls into:
    /// half a pixel off, interpolation washes a checkerboard out to its own
    /// mean, and the fit's residual against a frame that is *flat* there drops
    /// through the floor.  The answer looks excellent and destroys exactly the
    /// detail the stack was assembled to keep.
    ///
    /// Two frames with alternating single-pixel detail in a band the other
    /// frame is flat in — the shape of every focus bracket, at its worst.
    #[test]
    fn near_nyquist_detail_does_not_pull_the_fit_off_the_grid() {
        fn checker_band(band: std::ops::Range<u32>) -> Image {
            let side = 64;
            let mut img = Image::new(side, side);
            for y in 0..side {
                for x in 0..side {
                    let v = match (band.contains(&x), (x + y) % 2 == 0) {
                        (true, true) => 30,
                        (true, false) => 220,
                        (false, _) => 120,
                    };
                    let o = img.pixel_offset(x, y);
                    img.data[o..o + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
            img
        }

        assert_eq!(
            estimate_similarity(&checker_band(8..24), &checker_band(40..56)),
            None,
            "these frames are already aligned; any fit here is interpolation blur",
        );
    }

    /// A fit that lands outside what a focus bracket can produce is refused,
    /// so an unrelated frame is fused where it lies rather than warped by a
    /// meaningless answer.
    #[test]
    fn an_implausible_fit_is_refused() {
        let (w, h) = (200, 150);
        let reference = scene(w, h);
        // 40 % magnification is far outside any lens's breathing.
        let frame = magnified(&reference, 1.4, 0.0, 0.0);
        assert_eq!(estimate_similarity(&reference, &frame), None);
    }

    /// Mismatched frames cannot be fitted at all.
    #[test]
    fn different_sizes_are_not_fitted() {
        assert_eq!(estimate_similarity(&scene(64, 64), &scene(64, 48)), None);
    }

    /// Coverage is the contract with the caller: everything the transform
    /// reached past the frame's edge for is marked, so the focus measure never
    /// sees the edge-clamped border as scene content.
    #[test]
    fn warping_marks_what_it_invented() {
        let (w, h) = (64, 48);
        let img = scene(w, h);

        let (same, cover) = warp_to_reference(&img, Similarity::IDENTITY);
        assert_eq!(same.data, img.data, "identity must not resample");
        assert!(
            cover.iter().all(|&c| c == 1.0),
            "identity covers everything"
        );

        // Shift the sampling ten pixels right: the last ten columns of the
        // output have no source pixel behind them.
        let shifted = Similarity {
            tx: 10.0,
            ..Similarity::IDENTITY
        };
        let (_, cover) = warp_to_reference(&img, shifted);
        for y in 0..h as usize {
            for x in 0..w as usize {
                let want = if x as f32 + 10.0 <= (w - 1) as f32 {
                    1.0
                } else {
                    0.0
                };
                assert_eq!(cover[y * w as usize + x], want, "at ({x}, {y})");
            }
        }
    }

    #[test]
    fn scale_and_rotation_read_back_from_the_parameters() {
        let t = Similarity {
            a: 2.0 * 30.0f32.to_radians().cos(),
            b: 2.0 * 30.0f32.to_radians().sin(),
            tx: 0.0,
            ty: 0.0,
        };
        assert!((t.scale() - 2.0).abs() < 1e-5, "{}", t.scale());
        assert!(
            (t.rotation_deg() - 30.0).abs() < 1e-3,
            "{}",
            t.rotation_deg()
        );
    }
}
