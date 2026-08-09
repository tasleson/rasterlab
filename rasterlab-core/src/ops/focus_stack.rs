//! Focus stacking operation.
//!
//! Fuses multiple images captured at different focus distances into a single
//! all-in-focus result.  The frames come from one camera position, framed the
//! same way — but they do not land on the sensor the same way, because racking
//! focus also changes the lens's magnification a little (focus breathing), so
//! the op fits and removes that difference before fusing.  See
//! [`super::align`].
//!
//! Algorithm:
//! 1. Load every frame from disk (op is self-contained for replay).
//! 2. Verify all frames have matching dimensions.
//! 3. Align every frame to the first one — a scale, rotation and shift fitted
//!    per frame, then resampled into the first frame's grid.  Each resampled
//!    frame carries a coverage map marking where the fit reached past its own
//!    edge.
//! 4. For each frame, compute a per-pixel focus measure using the
//!    **Sum-Modified-Laplacian** (SML) aggregated over a 7×7 window.
//! 5. Smooth each SML map with a separable box blur so the per-pixel
//!    winner-selection doesn't flicker between adjacent pixels on flat
//!    content.
//! 6. Fuse with a weighted blend `w_i = SML_blur_i^p / Σ SML_blur_j^p`
//!    (p = 4).  The high exponent behaves like winner-takes-all where one
//!    image is clearly sharper while still producing soft transitions on
//!    tied regions.  Weights are scaled by coverage, so a frame contributes
//!    nothing where it was resampled from outside itself.
//!
//! The output keeps the first frame's geometry, so the edges of the result are
//! fused from however many frames still reach that far — never fewer than one,
//! since the first frame is the reference and covers itself completely.
//!
//! `apply()` ignores the input `Image` and reloads every frame from the
//! stored `image_paths`, making the op fully self-contained for
//! serialisation / non-destructive replay from the `.rlab` stack.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use super::align;
use crate::{
    cancel,
    error::{RasterError, RasterResult},
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
/// Radius over which a resampled frame's coverage is feathered before it
/// scales the weights.
///
/// It is exactly how far the focus measure reaches: a pixel this close to the
/// edge of a resampled frame has the edge itself inside its SML window, and
/// that edge is the strongest "detail" in the whole map.  Feathering rather
/// than cutting keeps the hand-over to the remaining frames smooth.
const COVERAGE_FEATHER_RADIUS: usize = SML_HALF + WEIGHT_BLUR_RADIUS;

// ── Public op ────────────────────────────────────────────────────────────────

/// How the frames are brought into a common geometry before they are fused.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameAlignment {
    /// Fuse the frames exactly as they were loaded.
    ///
    /// This is the *serialisation* default, not the one [`FocusStackOp::new`]
    /// picks: stacks saved before alignment existed were rendered without it,
    /// and a non-destructive edit has to replay to the pixels it was created
    /// with.
    #[default]
    None,
    /// Fit a scale, rotation and shift for every frame against the first one
    /// and resample into its geometry.  This is what cancels focus breathing.
    Similarity,
}

/// Non-destructive focus-stacking op.
///
/// Stores the absolute paths of every frame to fuse.  `apply()` ignores
/// its `Image` argument and produces the stacked result from scratch so
/// the op is self-contained and replayable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FocusStackOp {
    /// Absolute paths to the source frames.  The first is the reference: the
    /// result keeps its framing, and every other frame is fitted onto it.
    pub image_paths: Vec<String>,
    /// How the frames are brought into a common geometry.
    #[serde(default)]
    pub alignment: FrameAlignment,
}

impl FocusStackOp {
    /// A stack that corrects focus breathing.
    pub fn new(image_paths: Vec<String>) -> Self {
        Self::with_alignment(image_paths, FrameAlignment::Similarity)
    }

    pub fn with_alignment(image_paths: Vec<String>, alignment: FrameAlignment) -> Self {
        Self {
            image_paths,
            alignment,
        }
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
        let frames = self.image_paths.len();
        match self.alignment {
            FrameAlignment::None => format!("Focus Stack ({frames} frames)"),
            FrameAlignment::Similarity => format!("Focus Stack ({frames} frames, aligned)"),
        }
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

    // Load every frame (plain image files or library `.rlab` photos).
    let images = super::frames::load_frames(&op.image_paths, "Focus Stack")?;
    let refs: Vec<&Image> = images.iter().collect();
    fuse_frames(&refs, op.alignment)
}

/// Fuse pre-loaded frames into one all-in-focus image, aligning them first if
/// asked to.  `images[0]` is the reference: the result has its dimensions and
/// its framing.
///
/// Exposed publicly so tests and callers that already hold decoded frames can
/// exercise the fusion without going through disk I/O.
pub fn fuse_frames(images: &[&Image], alignment: FrameAlignment) -> RasterResult<Image> {
    let Some(&reference) = images.first() else {
        return Err(RasterError::InvalidParams(
            "Focus Stack: no frames provided".into(),
        ));
    };

    if images.len() == 1 {
        // Nothing to fuse — hand back the single frame unchanged.
        return Ok(reference.deep_clone());
    }

    // Every frame must arrive the same size.  Breathing changes magnification,
    // not pixel count, so differing dimensions mean the frames are not one
    // bracket and no amount of alignment will make them into one.
    let (w, h) = (reference.width, reference.height);
    for (i, img) in images.iter().enumerate().skip(1) {
        if img.width != w || img.height != h {
            return Err(RasterError::InvalidParams(format!(
                "Focus Stack: image {i} has dimensions {}x{} but image 0 is {w}x{h}",
                img.width, img.height
            )));
        }
    }

    let wu = w as usize;
    let hu = h as usize;

    // ── Alignment ────────────────────────────────────────────────────────
    //
    // Resampled frames own their pixels, so they have to outlive the fusion;
    // `frames` then points at either the resampled copy or the original.
    let aligned = align_frames(images, alignment)?;
    let frames: Vec<&Image> = images
        .iter()
        .zip(&aligned)
        .map(|(original, aligned)| match aligned {
            Some(a) => &a.image,
            None => *original,
        })
        .collect();

    // ── Per-frame focus measure ──────────────────────────────────────────

    let weights: Vec<Vec<f32>> = frames
        .par_iter()
        .map(|img| {
            let gray = super::luma_f32(img);
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
    //   w_i = coverage_i · (weights[i] + EPS)^p
    //   out = Σ w_i · src_i  /  Σ w_i
    //
    // Parallelise over output scanlines; each worker touches every
    // source image's row-slice, which is cache-friendly.

    let mut out = Image::new(w, h);
    let n = frames.len();

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
                    let mut wt = (weights[k][idx] + WEIGHT_EPSILON).powf(WEIGHT_POWER);
                    if let Some(a) = &aligned[k] {
                        wt *= a.coverage[idx];
                    }
                    if wt <= 0.0 {
                        continue;
                    }
                    let p = &frames[k].data[idx * 4..idx * 4 + 4];
                    r_acc += wt * p[0] as f32;
                    g_acc += wt * p[1] as f32;
                    b_acc += wt * p[2] as f32;
                    w_sum += wt;
                }

                let px = &mut row[x * 4..x * 4 + 4];
                if w_sum > 0.0 {
                    let inv = 1.0 / w_sum;
                    px[0] = (r_acc * inv).clamp(0.0, 255.0) as u8;
                    px[1] = (g_acc * inv).clamp(0.0, 255.0) as u8;
                    px[2] = (b_acc * inv).clamp(0.0, 255.0) as u8;
                } else {
                    // The reference frame always covers itself, so this is only
                    // reachable if every weight underflowed to zero.
                    px[..3].copy_from_slice(&reference.data[idx * 4..idx * 4 + 3]);
                }
                px[3] = 255;
            }
        });

    Ok(out)
}

// ── Alignment ────────────────────────────────────────────────────────────────

/// A frame resampled into the reference frame's grid.
struct Aligned {
    image: Image,
    /// Per-pixel weight multiplier in 0..1.  One where the frame genuinely
    /// covers the output, ramping to zero over the strip where the focus
    /// measure would otherwise be reading the resampled frame's own edge.
    coverage: Vec<f32>,
}

/// Fit and resample every frame onto `images[0]`.
///
/// The returned slot is `None` for any frame used as it arrived: the reference
/// itself, every frame when alignment is off, and any frame whose fit was
/// refused — leaving a frame where it lies is what the op did before alignment
/// existed, and it beats warping it by an answer we do not believe.
fn align_frames(
    images: &[&Image],
    alignment: FrameAlignment,
) -> RasterResult<Vec<Option<Aligned>>> {
    if alignment == FrameAlignment::None {
        return Ok((0..images.len()).map(|_| None).collect());
    }

    let reference = images[0];
    let wu = reference.width as usize;
    let hu = reference.height as usize;

    images
        .par_iter()
        .enumerate()
        .map(|(i, frame)| {
            if cancel::is_requested() {
                return Err(RasterError::Cancelled);
            }
            if i == 0 {
                return Ok(None);
            }
            let Some(fit) = align::estimate_similarity(reference, frame) else {
                return Ok(None);
            };
            let (image, coverage) = align::warp_to_reference(frame, fit);
            Ok(Some(Aligned {
                image,
                coverage: box_blur(&coverage, wu, hu, COVERAGE_FEATHER_RADIUS),
            }))
        })
        .collect()
}

// ── Focus measure: Sum of Modified Laplacian ────────────────────────────────

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
    use crate::ops::test_utils::{
        box_blurred, magnified, mean_abs_diff, split_halves, textured_scene,
    };

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

        let out = fuse_frames(&[&img, &img, &img], FrameAlignment::None).unwrap();

        for (a, b) in img.data.iter().zip(out.data.iter()) {
            assert!((*a as i16 - *b as i16).abs() <= 1, "{a} vs {b}");
        }
    }

    /// Alignment on frames that already line up must not disturb them: every
    /// fit is refused as no improvement, so nothing is resampled.
    #[test]
    fn aligning_already_aligned_frames_changes_nothing() {
        let img = textured_scene(96, 72);
        let plain = fuse_frames(&[&img, &img], FrameAlignment::None).unwrap();
        let aligned = fuse_frames(&[&img, &img], FrameAlignment::Similarity).unwrap();
        assert_eq!(plain.data, aligned.data);
    }

    /// The bug this alignment exists for.  Two frames of one scene, each sharp
    /// where the other is defocused — but the second was shot at a slightly
    /// longer effective focal length, as every lens does when it racks focus.
    /// Fused as-shot, its detail lands in the wrong place and the result is
    /// visibly worse than the scene it came from.
    #[test]
    fn focus_breathing_is_corrected_before_fusing() {
        let (w, h) = (480u32, 360u32);
        let sharp = textured_scene(w, h);
        let soft = box_blurred(&sharp, 3);

        // Near frame: sharp on the left.  Far frame: sharp on the right, and
        // 1.5 % larger — a modest amount of breathing for a macro lens.
        let near = split_halves(&sharp, &soft);
        let far = magnified(&split_halves(&soft, &sharp), 1.015, 0.0, 0.0);

        let unaligned = fuse_frames(&[&near, &far], FrameAlignment::None).unwrap();
        let aligned = fuse_frames(&[&near, &far], FrameAlignment::Similarity).unwrap();

        // Judged against the all-sharp scene the stack is trying to recover.
        // The inset skips the band down the middle where the two halves meet
        // and the frame edges, neither of which is what breathing broke.
        // Measures 9.0 → 2.6 levels of mean error; the bar is set well below
        // that, since what is being pinned is that alignment helps decisively
        // rather than any particular resampling quality.
        let inset = 16;
        let before = mean_abs_diff(&unaligned, &sharp, inset);
        let after = mean_abs_diff(&aligned, &sharp, inset);
        assert!(
            after < before * 0.5,
            "aligned error {after:.2} should be far below the unaligned {before:.2}",
        );
    }

    /// Alignment resamples the frames into the reference's grid, so the result
    /// keeps the reference's dimensions no matter what the fit was.
    #[test]
    fn output_keeps_the_reference_geometry() {
        let reference = textured_scene(128, 96);
        let frame = magnified(&reference, 1.02, 3.0, -2.0);
        let out = fuse_frames(&[&reference, &frame], FrameAlignment::Similarity).unwrap();
        assert_eq!((out.width, out.height), (reference.width, reference.height));
    }

    /// Frames of different sizes are not one bracket, and alignment does not
    /// pretend otherwise.
    #[test]
    fn mismatched_frames_are_rejected() {
        let a = textured_scene(64, 64);
        let b = textured_scene(64, 48);
        for alignment in [FrameAlignment::None, FrameAlignment::Similarity] {
            let err = fuse_frames(&[&a, &b], alignment).unwrap_err();
            assert!(err.to_string().contains("Focus Stack"), "{err}");
        }
    }

    #[test]
    fn a_single_frame_is_returned_unchanged() {
        let img = textured_scene(32, 32);
        let out = fuse_frames(&[&img], FrameAlignment::Similarity).unwrap();
        assert_eq!(out.data, img.data);
    }

    #[test]
    fn no_frames_is_an_error() {
        assert!(fuse_frames(&[], FrameAlignment::Similarity).is_err());
    }

    /// Stacks saved before alignment existed have no `alignment` field, and
    /// replaying one has to produce the pixels it was created with — not the
    /// ones today's default would produce.
    #[test]
    fn a_stack_saved_without_alignment_replays_unaligned() {
        let op: FocusStackOp =
            serde_json::from_str(r#"{"image_paths":["a.png","b.png"]}"#).unwrap();
        assert_eq!(op.alignment, FrameAlignment::None);
        assert_eq!(
            FocusStackOp::new(vec!["a.png".into()]).alignment,
            FrameAlignment::Similarity,
            "new stacks correct breathing",
        );
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
