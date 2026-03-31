use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Single-image faux HDR via luminance-based exposure fusion.
///
/// Three virtual exposure brackets are synthesised from the single input:
///
/// * +1 stop  (pixels × 2)   — lifts shadow detail into the well-exposed range
/// * 0 stops  (original)     — preserves already-well-exposed midtones
/// * −1 stop  (pixels × 0.5) — pulls overblown highlights back into range
///
/// A **luma-based** well-exposedness weight (Gaussian on BT.709 luminance,
/// σ = 0.35) is computed for each bracket, and the three weighted
/// contributions are normalised and blended.  Using luminance rather than a
/// per-channel Gaussian product is critical: a per-channel approach assigns
/// near-zero weight to every bracket for saturated colours (whose minority
/// channels sit far from the 0.5 peak), producing severe colour shifts.
/// Luma-based weighting derives a single scalar tone-mapping factor that is
/// applied identically to R, G and B, so the R:G:B ratio — and therefore
/// hue and saturation — is exactly preserved.
///
/// * `strength` — blend amount: `0.0` = original unchanged, `1.0` = full
///   fused result.  Values around `0.7`–`0.9` give a natural look.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FauxHdrOp {
    pub strength: f32,
}

impl FauxHdrOp {
    pub fn new(strength: f32) -> Self {
        Self {
            strength: strength.clamp(0.0, 1.0),
        }
    }
}

/// Gaussian well-exposedness weight based on a single luminance value.
///
/// Peaks at luma = 0.5 (ideal exposure), falls toward 0 and 1.
/// σ = 0.35 gives a broad, gradual curve that influences the full tonal
/// range without abrupt transitions.
#[inline]
fn well_exposedness(luma: f32) -> f32 {
    const SIGMA: f32 = 0.35;
    (-0.5 * ((luma - 0.5) / SIGMA).powi(2)).exp()
}

#[typetag::serde]
impl Operation for FauxHdrOp {
    fn name(&self) -> &'static str {
        "faux_hdr"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        if self.strength < 1e-5 {
            return Ok(image);
        }

        let s = self.strength;

        image.data.par_chunks_mut(4).for_each(|p| {
            let r = p[0] as f32 / 255.0;
            let g = p[1] as f32 / 255.0;
            let b = p[2] as f32 / 255.0;

            // BT.709 luminance — used for all weight calculations.
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;

            // Luma of each synthetic bracket.
            // The +1 stop bracket clips at 1.0 (matching sensor saturation).
            let luma_over = (luma * 2.0).min(1.0); // +1 stop
            let luma_under = luma * 0.5; // −1 stop

            // Well-exposedness weight for each bracket.
            let w0 = well_exposedness(luma_over); // +1 stop
            let w1 = well_exposedness(luma); //  0 stops
            let w2 = well_exposedness(luma_under); // −1 stop
            let sum = w0 + w1 + w2 + 1e-6; // epsilon avoids ÷0

            // Fused luma: weighted average of bracket lumas.
            let luma_fused = (w0 * luma_over + w1 * luma + w2 * luma_under) / sum;

            // Derive a uniform per-pixel tone-mapping scale.
            // Applying the same factor to R, G and B preserves the R:G:B
            // ratio exactly, so hue and saturation are unchanged.
            // The min(4.0) cap prevents extreme brightening of near-black
            // pixels where floating-point division is imprecise.
            let scale = if luma > 1e-6 {
                (luma_fused / luma).min(4.0)
            } else {
                1.0
            };

            let fr = r * scale;
            let fg = g * scale;
            let fb = b * scale;

            // Blend fused result with the original based on strength.
            p[0] = ((r + (fr - r) * s) * 255.0).clamp(0.0, 255.0) as u8;
            p[1] = ((g + (fg - g) * s) * 255.0).clamp(0.0, 255.0) as u8;
            p[2] = ((b + (fb - b) * s) * 255.0).clamp(0.0, 255.0) as u8;
            // alpha unchanged
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        format!("Faux HDR  {:.0}%", self.strength * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey_image(v: u8) -> Image {
        let mut img = Image::new(4, 4);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = v;
            p[1] = v;
            p[2] = v;
            p[3] = 255;
        });
        img
    }

    #[test]
    fn zero_strength_is_identity() {
        let src = grey_image(80);
        let src_data = src.data.clone();
        let out = FauxHdrOp::new(0.0).apply(src).unwrap();
        assert_eq!(out.data, src_data);
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 100;
            p[1] = 100;
            p[2] = 100;
            p[3] = 77;
        });
        let out = FauxHdrOp::new(1.0).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 77));
    }

    #[test]
    fn dark_pixels_are_lifted() {
        // Dark pixels lean toward the +1-stop bracket → should be brighter.
        let src = grey_image(20);
        let orig = src.data[0];
        let out = FauxHdrOp::new(1.0).apply(src).unwrap();
        assert!(
            out.data[0] > orig,
            "dark pixel should be lifted: {} → {}",
            orig,
            out.data[0]
        );
    }

    #[test]
    fn bright_pixels_are_pulled_down() {
        // Bright pixels lean toward the −1-stop bracket → should be darker.
        let src = grey_image(240);
        let orig = src.data[0];
        let out = FauxHdrOp::new(1.0).apply(src).unwrap();
        assert!(
            out.data[0] < orig,
            "bright pixel should be pulled down: {} → {}",
            orig,
            out.data[0]
        );
    }

    #[test]
    fn midtone_pixel_barely_moves() {
        // Mid-grey is already well-exposed; fused result should be very close.
        let src = grey_image(128);
        let out = FauxHdrOp::new(1.0).apply(src).unwrap();
        let diff = (out.data[0] as i16 - 128i16).unsigned_abs();
        assert!(
            diff <= 5,
            "mid-grey should barely move, got {}",
            out.data[0]
        );
    }

    #[test]
    fn partial_strength_blends_toward_original() {
        // Half-strength output sits between original and full-strength result.
        let orig = grey_image(20).data[0] as i32;
        let full = FauxHdrOp::new(1.0).apply(grey_image(20)).unwrap().data[0] as i32;
        let half = FauxHdrOp::new(0.5).apply(grey_image(20)).unwrap().data[0] as i32;
        assert!(
            half > orig && half < full,
            "half-strength {} not between {} and {}",
            half,
            orig,
            full
        );
    }

    #[test]
    fn saturated_colour_hue_preserved() {
        // A vivid red pixel should not shift hue after fusion.
        // With per-channel Gaussian weights this test would fail due to
        // desaturation; luma-based weighting must keep R:G ratio stable.
        let mut src = Image::new(1, 1);
        src.data[0] = 200; // R
        src.data[1] = 20; // G
        src.data[2] = 20; // B
        src.data[3] = 255;
        let out = FauxHdrOp::new(1.0).apply(src).unwrap();
        let r_out = out.data[0] as f32;
        let g_out = out.data[1] as f32;
        let b_out = out.data[2] as f32;
        // R:G and R:B ratios should be preserved within ±5%.
        let rg_orig = 200.0 / 20.0;
        let rb_orig = 200.0 / 20.0;
        let rg_out = r_out / g_out.max(1.0);
        let rb_out = r_out / b_out.max(1.0);
        assert!(
            (rg_out / rg_orig - 1.0).abs() < 0.05,
            "R:G ratio shifted: {:.2} → {:.2}",
            rg_orig,
            rg_out
        );
        assert!(
            (rb_out / rb_orig - 1.0).abs() < 0.05,
            "R:B ratio shifted: {:.2} → {:.2}",
            rb_orig,
            rb_out
        );
    }
}
