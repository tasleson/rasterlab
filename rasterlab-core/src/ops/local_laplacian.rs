use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Local tone — edge-aware tone and detail via local Laplacian filtering.
///
/// Every other tonal control in this crate is *global*: one curve, one gamma,
/// one number applied to every pixel regardless of where it sits.  That is
/// exactly what fails on a backlit frame, where the sky wants to come down and
/// the subject wants to come up, and a single gamma can only do one of them.
///
/// This operation decides per pixel *in the context of its surroundings*.  It
/// implements the local Laplacian filter of Paris, Hasinoff & Kautz,
/// *[Local Laplacian Filters]* (SIGGRAPH 2011), which manipulates the
/// coefficients of a Laplacian pyramid so that large-scale contrast (the
/// difference between the sky and the shade) can be compressed while
/// small-scale contrast (texture, edges, grain) is preserved or boosted.
/// Unlike an unsharp mask it does not halo, because the decision of what counts
/// as "an edge" versus "detail" is made by amplitude rather than by scale.
///
/// # Controls
///
/// * **tone** — large-scale range compression.  Positive lifts shadows and
///   restrains highlights, bringing the whole frame into a comfortable range
///   without flattening it.  Negative expands, adding drama.  This is the
///   control that fixes backlighting.
/// * **detail** — small-scale contrast.  Positive brings out texture; negative
///   smooths it.
/// * **threshold** — the amplitude, in luma units, that separates "detail"
///   from "an edge".  Differences below it are treated as texture and governed
///   by `detail`; differences above it are treated as scene structure and
///   governed by `tone`.
///
/// # Implementation
///
/// The literal algorithm rebuilds a pyramid per pixel, which is far too slow to
/// use.  This follows the standard acceleration of Aubry, Paris, Hasinoff,
/// Kautz & Durand, *[Fast Local Laplacian Filters]* (ACM TOG 2014): the
/// remapping is evaluated at a small number of discrete reference intensities
/// and the per-pixel result is interpolated between the two bracketing ones.
/// Because the interpolation weights form a partition of unity, each bin's
/// contribution can be accumulated and its pyramid dropped immediately, so
/// memory stays at a few planes rather than one pyramid per bin.
///
/// Work happens on a single luminance plane; the colour channels are then
/// scaled by the luminance gain, which preserves hue and keeps the cost at a
/// third of filtering RGB separately.
///
/// [Local Laplacian Filters]: https://people.csail.mit.edu/sparis/publi/2011/siggraph/
/// [Fast Local Laplacian Filters]: https://doi.org/10.1145/2629645
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalLaplacianOp {
    /// Large-scale range compression. `0.0` = no change. `[-1.0, 1.0]`.
    pub tone: f32,
    /// Small-scale (texture) contrast. `0.0` = no change. `[-1.0, 1.0]`.
    pub detail: f32,
    /// Amplitude separating detail from edges, in luma units. `(0.0, 1.0]`.
    pub threshold: f32,
}

/// Default edge/detail threshold.  0.15 of full scale sits above film grain,
/// sensor noise and skin texture but below the step from a lit face to a shaded
/// background, which is the boundary the two controls need to straddle.
pub const DEFAULT_THRESHOLD: f32 = 0.15;

/// Strongest remapping exponent either control may reach.
///
/// `tone`/`detail` of ±1 map to an exponent of `1 ∓ CONTROL_RANGE`.  Beyond
/// about 0.7 the exponent starts producing visible gradient reversals on smooth
/// skies, so the slider is scaled to stop short of that rather than letting the
/// user reach settings that are never useful.
const CONTROL_RANGE: f32 = 0.7;

/// Number of discrete reference intensities the remapping is evaluated at.
///
/// This is the accuracy/speed dial of the fast algorithm: cost is linear in it.
/// Chosen by measuring this implementation against a brute-force per-pixel
/// reference (the literal 2011 algorithm) on a synthetic backlit frame.  Mean
/// error against that ground truth, in 8-bit levels:
///
/// | bins | `tone` +0.8 | `detail` +0.9 |
/// |------|-------------|---------------|
/// | 8    | 0.46        | 4.10          |
/// | 16   | 0.06        | 0.94          |
/// | 32   | 0.01        | 0.85          |
///
/// 16 puts both controls under one level, which is below what 8-bit output can
/// represent; 32 only meaningfully helps the extreme `detail` setting and costs
/// twice as much.
const INTENSITY_BINS: usize = 16;

/// Amplitude, in luma units, below which a difference is treated as noise and
/// left alone by the `detail` control.
///
/// This serves two purposes at once.  Photographically it is what stops
/// `detail` from turning sensor noise and film grain into confetti — Paris et
/// al. include the same term for exactly that reason.
///
/// Numerically it is what makes the fast algorithm work at all.  The detail
/// exponent `x^α` with `α < 1` has infinite slope at `x = 0`, so the remapping
/// is not Lipschitz at the reference intensity and interpolating between
/// discrete bins cannot track it: without this term the error against the
/// brute-force reference stalls around 3 levels no matter how many bins are
/// used (measured: 3.04 levels at 64 bins).  Blending to a straight line below
/// the floor removes the singularity, and the same measurement then converges
/// to 0.94 levels at 16 bins.
///
/// 0.04 is about 10 levels of 255.  Lower floors regularise less and converge
/// worse (measured at 16 bins: 2.83 levels at 0.02, 1.64 at 0.03).
const NOISE_FLOOR: f32 = 0.04;

/// Short side, in pixels, at which the pyramid stops.
///
/// The residual is deliberately left untouched so overall brightness survives,
/// so whatever the coarsest level still resolves is immune to the `tone`
/// control.  That makes this constant decide how much range compression is
/// even reachable, and it has to go almost all the way down: at a 4×4 residual
/// a bright-sky/dark-foreground split is still *resolved* there, and measured
/// compression of such a frame was only 14%.  Taking the residual down to
/// near-DC leaves that contrast in the bands where `tone` acts.
///
/// Going deep is nearly free — each level is a quarter the area of the one
/// above, so the entire tail below the first few levels costs a few percent.
const MIN_COARSEST_PX: usize = 2;

/// Hard cap on pyramid depth, so a panorama cannot generate an unbounded number
/// of levels for no further benefit.
const MAX_LEVELS: usize = 14;

/// Luminance below which the colour channels are shifted rather than scaled.
/// The gain `l_out / l_in` is unusable as `l_in` approaches zero — it explodes
/// and turns sensor noise in the blacks into confetti.
const MIN_GAIN_LUMA: f32 = 1.0 / 255.0;

/// Burt–Adelson 5-tap binomial kernel, the standard pyramid filter.
const KERNEL: [f32; 5] = [0.0625, 0.25, 0.375, 0.25, 0.0625];

impl LocalLaplacianOp {
    pub fn new(tone: f32, detail: f32, threshold: f32) -> Self {
        Self {
            tone: tone.clamp(-1.0, 1.0),
            detail: detail.clamp(-1.0, 1.0),
            // A zero threshold would divide by zero in the remap; clamp to a
            // value that still means "almost everything is an edge".
            threshold: threshold.clamp(1.0 / 255.0, 1.0),
        }
    }

    /// `tone`/`detail` with the default threshold.
    pub fn with_defaults(tone: f32, detail: f32) -> Self {
        Self::new(tone, detail, DEFAULT_THRESHOLD)
    }

    fn is_identity(&self) -> bool {
        self.tone == 0.0 && self.detail == 0.0
    }

    /// Exponent applied to sub-threshold (detail) differences.
    ///
    /// Below 1 the remapping expands small differences, so texture gains
    /// contrast; above 1 it compresses them.
    fn detail_alpha(&self) -> f32 {
        1.0 - self.detail * CONTROL_RANGE
    }

    /// Slope applied to super-threshold (edge) differences.
    ///
    /// Below 1 the remapping pulls large differences together — the sky comes
    /// down toward the shade — which is range compression.
    fn tone_beta(&self) -> f32 {
        1.0 - self.tone * CONTROL_RANGE
    }
}

#[typetag::serde]
impl Operation for LocalLaplacianOp {
    fn name(&self) -> &'static str {
        "local_laplacian"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        let w = image.width as usize;
        let h = image.height as usize;
        if self.is_identity() || w == 0 || h == 0 {
            return Ok(image);
        }

        let levels = pyramid_levels(w, h);
        if levels < 2 {
            // No Laplacian band exists, so there is nothing to remap.
            return Ok(image);
        }

        let luma = Plane::luma_of(&image);
        let filtered = local_laplacian(
            &luma,
            levels,
            self.threshold,
            self.detail_alpha(),
            self.tone_beta(),
        );

        apply_luma_gain(&mut image, &luma.data, &filtered.data);
        Ok(image)
    }

    fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.tone != 0.0 {
            parts.push(format!("tone {:+.2}", self.tone));
        }
        if self.detail != 0.0 {
            parts.push(format!("detail {:+.2}", self.detail));
        }
        if parts.is_empty() {
            return "Local Tone (none)".into();
        }
        format!(
            "Local Tone  {}  threshold {:.2}",
            parts.join("  "),
            self.threshold
        )
    }
}

// ── Pyramid plumbing ──────────────────────────────────────────────────────────

/// A single-channel f32 image, the working representation for the pyramid.
#[derive(Debug, Clone)]
struct Plane {
    data: Vec<f32>,
    w: usize,
    h: usize,
}

impl Plane {
    fn zeros(w: usize, h: usize) -> Self {
        Self {
            data: vec![0.0; w * h],
            w,
            h,
        }
    }

    /// BT.709 luminance of `image`, normalised to `[0, 1]`.
    fn luma_of(image: &Image) -> Self {
        let w = image.width as usize;
        let h = image.height as usize;
        let mut data = vec![0.0f32; w * h];
        data.par_chunks_mut(w)
            .zip(image.data.par_chunks(image.row_stride()))
            .for_each(|(row, px)| {
                for (l, p) in row.iter_mut().zip(px.chunks_exact(4)) {
                    *l = (0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32)
                        / 255.0;
                }
            });
        Self { data, w, h }
    }
}

/// Number of pyramid levels (including the residual) for a `w` × `h` image.
fn pyramid_levels(w: usize, h: usize) -> usize {
    let mut short = w.min(h);
    let mut levels = 1;
    while short > MIN_COARSEST_PX && levels < MAX_LEVELS {
        short = short.div_ceil(2);
        levels += 1;
    }
    levels
}

/// Scratch buffers shared by every blur, at every level and every bin.
///
/// The filter rebuilds a whole pyramid per intensity bin, so anything
/// allocated inside those loops is allocated a few hundred times.  Letting each
/// blur allocate its own intermediates cost roughly 770 MB of churn over a
/// 16-bin run on a 3 MP image and dominated the runtime.  These are sized once
/// for the finest level; every coarser level borrows a prefix.
struct Workspace {
    /// Intermediate between the horizontal and vertical blur passes.
    blur_tmp: Vec<f32>,
    /// Holds the zero-inserted image during upsampling, and the blurred
    /// full-size image during downsampling.
    stage: Vec<f32>,
}

impl Workspace {
    fn new(n: usize) -> Self {
        Self {
            blur_tmp: vec![0.0; n],
            stage: vec![0.0; n],
        }
    }
}

/// Separable 5-tap binomial blur with clamped edges, into a caller-owned
/// buffer.  `dst` and `tmp` must each hold at least `w * h` elements; their
/// previous contents are overwritten, never read.
fn blur5_into(src: &[f32], w: usize, h: usize, dst: &mut [f32], tmp: &mut [f32]) {
    let n = w * h;
    let tmp = &mut tmp[..n];
    let dst = &mut dst[..n];

    // Horizontal — one row per task.
    tmp.par_chunks_mut(w)
        .zip(src[..n].par_chunks(w))
        .for_each(|(dst_row, src_row)| {
            for (x, out) in dst_row.iter_mut().enumerate() {
                let mut acc = 0.0;
                for (k, &coeff) in KERNEL.iter().enumerate() {
                    let sx = (x as isize + k as isize - 2).clamp(0, w as isize - 1) as usize;
                    acc += coeff * src_row[sx];
                }
                *out = acc;
            }
        });

    // Vertical — one output row per task, reading strided from `tmp`.  The
    // first tap writes and the rest accumulate, so `dst` needs no pre-zeroing.
    dst.par_chunks_mut(w).enumerate().for_each(|(y, dst_row)| {
        for (k, &coeff) in KERNEL.iter().enumerate() {
            let sy = (y as isize + k as isize - 2).clamp(0, h as isize - 1) as usize;
            let src_row = &tmp[sy * w..(sy + 1) * w];
            if k == 0 {
                for (o, &s) in dst_row.iter_mut().zip(src_row) {
                    *o = coeff * s;
                }
            } else {
                for (o, &s) in dst_row.iter_mut().zip(src_row) {
                    *o += coeff * s;
                }
            }
        }
    });
}

/// Blur `src` then drop every other sample into `dst`.
fn downsample_into(
    src: &[f32],
    sw: usize,
    sh: usize,
    dst: &mut [f32],
    dw: usize,
    ws: &mut Workspace,
) {
    blur5_into(src, sw, sh, &mut ws.stage, &mut ws.blur_tmp);
    let stage = &ws.stage;
    dst.par_chunks_mut(dw).enumerate().for_each(|(y, row)| {
        let src_row = &stage[(y * 2) * sw..(y * 2 + 1) * sw];
        for (x, o) in row.iter_mut().enumerate() {
            *o = src_row[x * 2];
        }
    });
}

/// Upsample `src` to `dw` × `dh` into `dst`.
///
/// Conceptually this inserts zeros between samples and blurs with `2 × KERNEL`,
/// the standard Burt–Adelson synthesis filter.  It is written instead as a
/// direct gather from the source, because that is what makes the *borders*
/// right: for each output pixel only the taps landing on real samples are
/// evaluated, and the source index is clamped, so the weights always sum to one.
///
/// Doing it the literal way — materialise the zero-inserted image, then blur it
/// with clamped edges — mis-normalises at the border, because clamping
/// replicates whichever of the interleaved zeros happens to sit on the edge.
/// That error is invisible in a round-trip test: analysis and synthesis use the
/// same operator, so any border behaviour cancels exactly when nothing is
/// modified.  It only appears once the bands are actually remapped, and then it
/// is severe — measured at half brightness on the outermost pixels of a real
/// photograph, decaying over roughly a hundred pixels.
fn upsample_into(
    src: &[f32],
    sw: usize,
    sh: usize,
    dst: &mut [f32],
    dw: usize,
    dh: usize,
    ws: &mut Workspace,
) {
    /// A tap contributes only where the zero-inserted grid holds a real sample,
    /// i.e. where `out ± k` is even; the surviving taps carry twice the
    /// analysis weight, once per axis.
    #[inline]
    fn gather(get: impl Fn(usize) -> f32, out: usize, limit: usize) -> f32 {
        let mut acc = 0.0;
        for (k, &coeff) in KERNEL.iter().enumerate() {
            let p = out as isize + 2 - k as isize;
            if p.rem_euclid(2) == 0 {
                let s = (p / 2).clamp(0, limit as isize - 1) as usize;
                acc += 2.0 * coeff * get(s);
            }
        }
        acc
    }

    // Horizontal: sw × sh → dw × sh, into the staging buffer.
    let stage = &mut ws.stage[..dw * sh];
    stage.par_chunks_mut(dw).enumerate().for_each(|(y, row)| {
        let src_row = &src[y * sw..(y + 1) * sw];
        for (x, o) in row.iter_mut().enumerate() {
            *o = gather(|s| src_row[s], x, sw);
        }
    });

    // Vertical: dw × sh → dw × dh.
    let stage = &ws.stage[..dw * sh];
    dst[..dw * dh]
        .par_chunks_mut(dw)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                *o = gather(|s| stage[s * dw + x], y, sh);
            }
        });
}

/// Allocate the plane shapes of a `levels`-deep pyramid over a `w` × `h` base.
fn alloc_pyramid(w: usize, h: usize, levels: usize) -> Vec<Plane> {
    let mut planes = Vec::with_capacity(levels);
    let (mut lw, mut lh) = (w, h);
    for _ in 0..levels {
        planes.push(Plane::zeros(lw, lh));
        lw = lw.div_ceil(2);
        lh = lh.div_ceil(2);
    }
    planes
}

/// Fill `pyr` as the Gaussian pyramid of whatever is already in `pyr[0]`.
fn fill_gaussian(pyr: &mut [Plane], ws: &mut Workspace) {
    for l in 1..pyr.len() {
        let (coarser, finer) = pyr.split_at_mut(l);
        let src = &coarser[l - 1];
        let dst = &mut finer[0];
        downsample_into(&src.data, src.w, src.h, &mut dst.data, dst.w, ws);
    }
}

// ── The filter ────────────────────────────────────────────────────────────────

/// Paris et al.'s pointwise remapping, as a magnitude: given `a = |i - g|`,
/// how far the remapped sample sits from the reference intensity `g`.
///
/// Differences up to `threshold` are treated as detail and reshaped by the
/// exponent `alpha`; larger ones are treated as scene structure and rescaled by
/// the slope `beta`.  `alpha == beta == 1.0` returns `a` unchanged, which makes
/// the whole filter the identity.
///
/// Below [`NOISE_FLOOR`] the detail curve is blended back to a straight line,
/// which both leaves noise alone and keeps the function differentiable at
/// `a == 0` — see that constant for why the second property is essential.
fn remap_magnitude(a: f32, threshold: f32, alpha: f32, beta: f32) -> f32 {
    if a <= threshold {
        let x = a / threshold;
        let u = (a / NOISE_FLOOR).clamp(0.0, 1.0);
        let blend = u * u * (3.0 - 2.0 * u); // smoothstep
        threshold * (blend * x.powf(alpha) + (1.0 - blend) * x)
    } else {
        beta * (a - threshold) + threshold
    }
}

/// Entries in the tabulated remapping curve.  The curve is sampled over the
/// whole `[0, 1]` range of possible differences, so 4096 entries put the
/// spacing at about a sixteenth of an 8-bit level; with linear interpolation
/// on top, the table is far more accurate than the output can represent.
const REMAP_TABLE_SIZE: usize = 4096;

/// [`remap_magnitude`] tabulated over `a ∈ [0, 1]`.
///
/// The remapping depends on the reference intensity only through `i - g`, so a
/// single table serves every intensity bin.  Evaluated directly the curve costs
/// one `powf` per pixel per bin — 48 M calls on a 3 MP image at 16 bins — but
/// tabulating it is worth only a few percent, not the order-of-magnitude the
/// call count suggests: the pyramid's memory traffic dominates everything here.
struct RemapCurve {
    table: Vec<f32>,
}

impl RemapCurve {
    fn new(threshold: f32, alpha: f32, beta: f32) -> Self {
        let scale = 1.0 / (REMAP_TABLE_SIZE - 1) as f32;
        Self {
            table: (0..REMAP_TABLE_SIZE)
                .map(|i| remap_magnitude(i as f32 * scale, threshold, alpha, beta))
                .collect(),
        }
    }

    /// Remap sample `i` around reference intensity `g`.
    #[inline]
    fn apply(&self, i: f32, g: f32) -> f32 {
        let d = i - g;
        // Both inputs are luma in [0, 1], so `a` cannot leave the tabulated
        // range; clamping guards the index arithmetic rather than the maths.
        let pos = d.abs().clamp(0.0, 1.0) * (REMAP_TABLE_SIZE - 1) as f32;
        let idx = pos as usize;
        let frac = pos - idx as f32;
        let lo = self.table[idx];
        let hi = self.table[(idx + 1).min(REMAP_TABLE_SIZE - 1)];
        let magnitude = lo + (hi - lo) * frac;
        if d < 0.0 {
            g - magnitude
        } else {
            g + magnitude
        }
    }
}

/// Run the local Laplacian filter over a luminance plane.
///
/// Accumulates one intensity bin at a time.  For each bin the whole image is
/// remapped toward that reference intensity, its Gaussian pyramid is built, and
/// every Laplacian band is added into the output weighted by how close each
/// pixel's own local intensity is to that bin.  The weights are hat functions
/// that sum to one, so the accumulated result equals interpolating between the
/// two bracketing bins — but without holding every bin's pyramid at once.
fn local_laplacian(luma: &Plane, levels: usize, threshold: f32, alpha: f32, beta: f32) -> Plane {
    let curve = RemapCurve::new(threshold, alpha, beta);
    let mut ws = Workspace::new(luma.data.len());

    // The input's own pyramid, which supplies the reference intensity that
    // decides each pixel's interpolation weights.
    let mut gauss = alloc_pyramid(luma.w, luma.h, levels);
    gauss[0].data.copy_from_slice(&luma.data);
    fill_gaussian(&mut gauss, &mut ws);

    // Output Laplacian bands, zeroed; the residual is carried over untouched so
    // the image keeps its overall brightness.
    let mut out_bands: Vec<Plane> = (0..levels - 1)
        .map(|l| Plane::zeros(gauss[l].w, gauss[l].h))
        .collect();

    // Rebuilt in place for each bin rather than reallocated.
    let mut remapped = alloc_pyramid(luma.w, luma.h, levels);
    let mut coarse = Plane::zeros(luma.w, luma.h);

    let last_bin = (INTENSITY_BINS - 1) as f32;
    for bin in 0..INTENSITY_BINS {
        let gamma = bin as f32 / last_bin;

        remapped[0]
            .data
            .par_chunks_mut(luma.w)
            .zip(luma.data.par_chunks(luma.w))
            .for_each(|(dst, src)| {
                for (o, &i) in dst.iter_mut().zip(src) {
                    *o = curve.apply(i, gamma);
                }
            });
        fill_gaussian(&mut remapped, &mut ws);

        for (l, band) in out_bands.iter_mut().enumerate() {
            let (fine_w, fine_h) = (remapped[l].w, remapped[l].h);
            upsample_into(
                &remapped[l + 1].data,
                remapped[l + 1].w,
                remapped[l + 1].h,
                &mut coarse.data,
                fine_w,
                fine_h,
                &mut ws,
            );
            band.data
                .par_chunks_mut(fine_w)
                .zip(remapped[l].data.par_chunks(fine_w))
                .zip(coarse.data[..fine_w * fine_h].par_chunks(fine_w))
                .zip(gauss[l].data.par_chunks(fine_w))
                .for_each(|(((dst, fine), up), reference)| {
                    for (((o, &f), &c), &g) in dst.iter_mut().zip(fine).zip(up).zip(reference) {
                        // Hat weight: 1 at this bin's intensity, falling to 0
                        // at its neighbours.  Pixels outside [0,1] clamp to the
                        // end bins so nothing loses its contribution.
                        let weight = 1.0 - (g.clamp(0.0, 1.0) - gamma).abs() * last_bin;
                        if weight > 0.0 {
                            *o += weight * (f - c);
                        }
                    }
                });
        }
    }

    // Collapse: start from the untouched residual and add each band back.
    let mut out = gauss[levels - 1].clone();
    for band in out_bands.iter().rev() {
        upsample_into(
            &out.data,
            out.w,
            out.h,
            &mut coarse.data,
            band.w,
            band.h,
            &mut ws,
        );
        let n = band.w * band.h;
        let mut data = coarse.data[..n].to_vec();
        data.par_iter_mut()
            .zip(band.data.par_iter())
            .for_each(|(o, &b)| *o += b);
        out = Plane {
            data,
            w: band.w,
            h: band.h,
        };
    }
    out
}

/// Scale each pixel's colour by its luminance gain, in place.
///
/// Multiplying preserves the ratios between channels, so hue and relative
/// saturation survive a large tonal move.  Near black the ratio is meaningless,
/// so the change is applied additively instead.
fn apply_luma_gain(image: &mut Image, before: &[f32], after: &[f32]) {
    let row_stride = image.row_stride();
    let w = image.width as usize;
    image
        .data
        .par_chunks_mut(row_stride)
        .zip(before.par_chunks(w))
        .zip(after.par_chunks(w))
        .for_each(|((px_row, old_row), new_row)| {
            for ((p, &old), &new) in px_row.chunks_exact_mut(4).zip(old_row).zip(new_row) {
                if old > MIN_GAIN_LUMA {
                    let gain = new / old;
                    for c in p.iter_mut().take(3) {
                        *c = (*c as f32 * gain).clamp(0.0, 255.0) as u8;
                    }
                } else {
                    let shift = (new - old) * 255.0;
                    for c in p.iter_mut().take(3) {
                        *c = (*c as f32 + shift).clamp(0.0, 255.0) as u8;
                    }
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image_from(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Image {
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

    fn noise(x: u32, y: u32) -> u8 {
        let mut s = x.wrapping_mul(1973) ^ y.wrapping_mul(9277) ^ 0x5bf0_3635;
        s ^= s >> 13;
        s = s.wrapping_mul(0x9e37_79b9);
        (s >> 24) as u8
    }

    /// A backlit frame: bright top, dark bottom, texture throughout.
    fn backlit(w: u32, h: u32) -> Image {
        image_from(w, h, |x, y| {
            let base = if y < h / 2 { 215 } else { 45 };
            let v = (base + (noise(x, y) % 24) as i32 - 12).clamp(0, 255) as u8;
            [v, v, v]
        })
    }

    fn mean_luma(image: &Image, rows: std::ops::Range<u32>) -> f32 {
        let mut sum = 0.0;
        let mut n = 0.0;
        for y in rows {
            for x in 0..image.width {
                let p = image.pixel(x, y);
                sum += 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32;
                n += 1.0;
            }
        }
        sum / n
    }

    /// The pyramid must reconstruct its input: analysis followed by synthesis
    /// with no remapping is the identity, whatever the operators do at borders.
    /// Everything else in this module rests on that.
    #[test]
    fn pyramid_round_trips() {
        for (w, h) in [(64, 64), (65, 33), (129, 97), (40, 200)] {
            let img = image_from(w, h, |x, y| {
                let v = noise(x, y);
                [v, v, v]
            });
            let luma = Plane::luma_of(&img);
            let levels = pyramid_levels(w as usize, h as usize);
            assert!(levels >= 2, "{w}x{h} should build a real pyramid");

            // alpha = beta = 1 is the identity remapping.
            let out = local_laplacian(&luma, levels, DEFAULT_THRESHOLD, 1.0, 1.0);
            for (i, (&a, &b)) in luma.data.iter().zip(out.data.iter()).enumerate() {
                assert!(
                    (a - b).abs() < 1e-3,
                    "{w}x{h}: pixel {i} drifted {a} -> {b}"
                );
            }
        }
    }

    /// Borders must be treated like the rest of the image.
    ///
    /// This is deliberately separate from [`pyramid_round_trips`], which cannot
    /// catch border bugs at all: analysis and synthesis share the upsample
    /// operator, so whatever it does at the edges cancels exactly when the
    /// bands are untouched.  The error only appears once the filter actually
    /// modifies something — an earlier upsample here left the outermost pixels
    /// of a real photograph at half brightness while the interior was correct.
    ///
    /// A flat field makes it unmissable: the correct answer is "unchanged
    /// everywhere", so any border mishandling shows up as a rim.
    #[test]
    fn borders_are_not_darkened_or_brightened() {
        const V: u8 = 140;
        let flat = image_from(200, 150, |_, _| [V, V, V]);
        let out = LocalLaplacianOp::with_defaults(0.9, 0.5)
            .apply(flat)
            .unwrap();
        for y in 0..out.height {
            for x in 0..out.width {
                let p = out.pixel(x, y);
                assert!(
                    p[0].abs_diff(V) <= 2,
                    "flat field changed at ({x},{y}): {} vs {V}",
                    p[0]
                );
            }
        }

        // And on real content: the gain applied to the outermost ring must
        // track the gain applied just inside it.
        let src = backlit(256, 256);
        let out = LocalLaplacianOp::with_defaults(0.8, 0.0)
            .apply(src.deep_clone())
            .unwrap();
        let ring_gain = |d: u32| -> f32 {
            let (mut before, mut after) = (0.0f32, 0.0f32);
            let mut n = 0.0;
            for x in d..src.width - d {
                for y in [d, src.height - 1 - d] {
                    before += src.pixel(x, y)[1] as f32;
                    after += out.pixel(x, y)[1] as f32;
                    n += 1.0;
                }
            }
            (after / n) / (before / n)
        };
        let edge = ring_gain(0);
        let inside = ring_gain(24);
        assert!(
            (edge - inside).abs() < 0.06,
            "edge gain {edge:.3} should track interior gain {inside:.3}"
        );
    }

    #[test]
    fn identity_settings_leave_the_image_untouched() {
        let src = backlit(96, 96);
        let before = src.data.clone();
        let out = LocalLaplacianOp::with_defaults(0.0, 0.0)
            .apply(src)
            .unwrap();
        assert_eq!(out.data, before);
    }

    /// The reason this operation exists: a positive `tone` must bring the two
    /// halves of a backlit frame toward each other, which no global gamma can
    /// do — a gamma moves both halves the same direction.
    #[test]
    fn positive_tone_compresses_a_backlit_frame() {
        let src = backlit(128, 128);
        let top_before = mean_luma(&src, 0..64);
        let bottom_before = mean_luma(&src, 64..128);
        let gap_before = top_before - bottom_before;

        let out = LocalLaplacianOp::with_defaults(0.8, 0.0)
            .apply(src.deep_clone())
            .unwrap();
        let top_after = mean_luma(&out, 0..64);
        let bottom_after = mean_luma(&out, 64..128);
        let gap_after = top_after - bottom_after;

        assert!(
            gap_after < gap_before * 0.8,
            "tone should compress the sky-to-shadow gap: {gap_before:.1} -> {gap_after:.1}"
        );
        assert!(
            bottom_after > bottom_before,
            "the dark half should lift: {bottom_before:.1} -> {bottom_after:.1}"
        );
    }

    /// Negative `tone` is the inverse: it should pull the halves apart.
    #[test]
    fn negative_tone_expands_range() {
        let src = backlit(128, 128);
        let gap_before = mean_luma(&src, 0..64) - mean_luma(&src, 64..128);
        let out = LocalLaplacianOp::with_defaults(-0.8, 0.0)
            .apply(src.deep_clone())
            .unwrap();
        let gap_after = mean_luma(&out, 0..64) - mean_luma(&out, 64..128);
        assert!(
            gap_after > gap_before,
            "negative tone should expand: {gap_before:.1} -> {gap_after:.1}"
        );
    }

    /// `detail` must change fine texture without moving the overall level —
    /// that separation is what distinguishes this from a contrast slider.
    #[test]
    fn detail_changes_texture_not_overall_level() {
        // Texture amplitude ~30 levels (0.12): comfortably above NOISE_FLOOR,
        // so `detail` should act on it, and below the edge threshold, so it is
        // treated as texture rather than scene structure.
        let src = image_from(128, 128, |x, y| {
            let v = (128 + (noise(x, y) % 60) as i32 - 30).clamp(0, 255) as u8;
            [v, v, v]
        });
        let texture_of = |img: &Image| -> f32 {
            let mut sum = 0.0;
            for y in 0..img.height {
                for x in 1..img.width {
                    let a = img.pixel(x, y)[1] as f32;
                    let b = img.pixel(x - 1, y)[1] as f32;
                    sum += (a - b).abs();
                }
            }
            sum
        };
        let base_texture = texture_of(&src);
        let base_level = mean_luma(&src, 0..128);

        let boosted = LocalLaplacianOp::with_defaults(0.0, 0.9)
            .apply(src.deep_clone())
            .unwrap();
        assert!(
            texture_of(&boosted) > base_texture * 1.1,
            "detail should raise texture: {base_texture:.0} -> {:.0}",
            texture_of(&boosted)
        );
        assert!(
            (mean_luma(&boosted, 0..128) - base_level).abs() < 4.0,
            "detail must not shift the overall level"
        );

        let smoothed = LocalLaplacianOp::with_defaults(0.0, -0.9)
            .apply(src.deep_clone())
            .unwrap();
        assert!(
            texture_of(&smoothed) < base_texture * 0.9,
            "negative detail should reduce texture"
        );
    }

    /// A neutral image must stay neutral: scaling by luminance gain preserves
    /// channel ratios, so grey cannot acquire a colour cast.
    #[test]
    fn greys_stay_neutral() {
        let src = image_from(96, 96, |x, y| {
            let v = (30 + (x + y) * 180 / 192) as u8;
            [v, v, v]
        });
        let out = LocalLaplacianOp::with_defaults(0.9, 0.5)
            .apply(src)
            .unwrap();
        for p in out.data.chunks_exact(4) {
            assert!(
                p[0].abs_diff(p[1]) <= 1 && p[1].abs_diff(p[2]) <= 1,
                "grey went coloured: {:?}",
                &p[..3]
            );
        }
    }

    #[test]
    fn alpha_and_dimensions_are_preserved() {
        let mut src = image_from(70, 50, |x, y| {
            let v = noise(x, y);
            [v, v, v]
        });
        for (i, p) in src.data.chunks_exact_mut(4).enumerate() {
            p[3] = (i % 256) as u8;
        }
        let alphas: Vec<u8> = src.data.chunks_exact(4).map(|p| p[3]).collect();
        let out = LocalLaplacianOp::with_defaults(0.6, 0.3)
            .apply(src)
            .unwrap();
        assert_eq!((out.width, out.height), (70, 50));
        let got: Vec<u8> = out.data.chunks_exact(4).map(|p| p[3]).collect();
        assert_eq!(got, alphas, "alpha must be untouched");
    }

    /// Images too small for a pyramid, and degenerate sizes, must pass through
    /// rather than panic on the level arithmetic.
    #[test]
    fn tiny_and_degenerate_images_pass_through() {
        for (w, h) in [(0, 0), (1, 1), (4, 4), (16, 16), (3, 100)] {
            let src = image_from(w, h, |x, y| {
                let v = noise(x, y);
                [v, v, v]
            });
            let before = src.data.clone();
            let out = LocalLaplacianOp::with_defaults(0.8, 0.4)
                .apply(src)
                .unwrap();
            assert_eq!((out.width, out.height), (w, h), "{w}x{h} dimensions");
            if pyramid_levels(w as usize, h as usize) < 2 {
                assert_eq!(out.data, before, "{w}x{h} should be untouched");
            }
        }
    }

    #[test]
    fn remap_is_identity_at_unit_parameters() {
        for i in 0..=255 {
            let i = i as f32 / 255.0;
            for g in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
                let r = RemapCurve::new(DEFAULT_THRESHOLD, 1.0, 1.0).apply(i, g);
                assert!((r - i).abs() < 1e-5, "remap({i},{g}) = {r}");
            }
        }
    }

    #[test]
    fn parameters_are_clamped() {
        let op = LocalLaplacianOp::new(5.0, -5.0, 0.0);
        assert_eq!(op.tone, 1.0);
        assert_eq!(op.detail, -1.0);
        assert!(op.threshold > 0.0, "threshold must never be zero");
    }

    #[test]
    fn describe_mentions_active_controls() {
        assert!(
            LocalLaplacianOp::with_defaults(0.5, 0.0)
                .describe()
                .contains("tone")
        );
        assert!(
            LocalLaplacianOp::with_defaults(0.0, 0.0)
                .describe()
                .contains("none")
        );
    }
}
