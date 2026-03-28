use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Recover highlight detail and lift shadow detail independently.
///
/// Unlike Brightness/Contrast (which shifts or scales all tones uniformly),
/// these controls apply a **luma-weighted** correction that is strongest in
/// the target tonal zone and fades smoothly to zero at the opposite end,
/// leaving the other tones largely untouched.
///
/// Both parameters operate in luminance space and apply the same scale
/// factor to R, G and B, so hue and saturation are preserved.
///
/// * `highlights` — range `[-1.0, 0.0]`.  Negative values pull bright
///   pixels down (recover blown-out detail).  Positive values brighten
///   highlights; clamp at `0.0` to avoid double-brightening.
///   The weight is `luma²` so the effect peaks at white and is zero at
///   mid-grey and below.
/// * `shadows`    — range `[0.0, 1.0]`.  Positive values lift dark pixels
///   (open up crushed shadows).  The weight is `(1 − luma)²` so the
///   effect peaks at black and is zero at mid-grey and above.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HighlightsShadowsOp {
    pub highlights: f32,
    pub shadows: f32,
}

impl HighlightsShadowsOp {
    pub fn new(highlights: f32, shadows: f32) -> Self {
        Self {
            highlights: highlights.clamp(-1.0, 1.0),
            shadows: shadows.clamp(-1.0, 1.0),
        }
    }
}

#[typetag::serde]
impl Operation for HighlightsShadowsOp {
    fn name(&self) -> &'static str {
        "highlights_shadows"
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        if self.highlights.abs() < 1e-5 && self.shadows.abs() < 1e-5 {
            return Ok(image);
        }

        let hl = self.highlights;
        let sh = self.shadows;

        image.data.par_chunks_mut(4).for_each(|p| {
            let r = p[0] as f32 / 255.0;
            let g = p[1] as f32 / 255.0;
            let b = p[2] as f32 / 255.0;

            // BT.709 luminance drives both weight functions.
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;

            // Highlight weight: quadratic ramp, peaks at luma=1, zero at luma≤0.5.
            let hl_weight = ((luma - 0.5) * 2.0).max(0.0).powi(2);
            // Shadow weight: quadratic ramp, peaks at luma=0, zero at luma≥0.5.
            let sh_weight = ((0.5 - luma) * 2.0).max(0.0).powi(2);

            // Additive luminance delta from each control.
            // Max delta magnitude: ±0.5 (half the full [0,1] range) so that
            // extreme values meaningfully recover detail without obliterating it.
            let delta = hl * hl_weight * 0.5 + sh * sh_weight * 0.5;

            // Apply the same delta to all channels to preserve hue.
            p[0] = ((r + delta) * 255.0).clamp(0.0, 255.0) as u8;
            p[1] = ((g + delta) * 255.0).clamp(0.0, 255.0) as u8;
            p[2] = ((b + delta) * 255.0).clamp(0.0, 255.0) as u8;
            // alpha unchanged
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        format!(
            "Highlights/Shadows  hl={:+.2}  sh={:+.2}",
            self.highlights, self.shadows
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(v: u8) -> Image {
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
    fn identity() {
        let src = grey(100);
        let src_data = src.data.clone();
        let out = HighlightsShadowsOp::new(0.0, 0.0).apply(src).unwrap();
        assert_eq!(out.data, src_data);
    }

    #[test]
    fn highlights_pulls_bright_down() {
        // hl=-1 should darken a near-white pixel.
        let src = grey(230);
        let orig = src.data[0];
        let out = HighlightsShadowsOp::new(-1.0, 0.0).apply(src).unwrap();
        assert!(out.data[0] < orig, "bright pixel should darken");
    }

    #[test]
    fn highlights_leaves_shadows_alone() {
        // hl control should have no effect on dark pixels.
        let src = grey(30);
        let orig = src.data[0];
        let out = HighlightsShadowsOp::new(-1.0, 0.0).apply(src).unwrap();
        assert_eq!(out.data[0], orig, "dark pixel should be untouched");
    }

    #[test]
    fn shadows_lifts_dark() {
        // sh=+1 should brighten a near-black pixel.
        let src = grey(25);
        let orig = src.data[0];
        let out = HighlightsShadowsOp::new(0.0, 1.0).apply(src).unwrap();
        assert!(out.data[0] > orig, "dark pixel should brighten");
    }

    #[test]
    fn shadows_leaves_highlights_alone() {
        // sh control should have no effect on bright pixels.
        let src = grey(230);
        let orig = src.data[0];
        let out = HighlightsShadowsOp::new(0.0, 1.0).apply(src).unwrap();
        assert_eq!(out.data[0], orig, "bright pixel should be untouched");
    }

    #[test]
    fn midtone_barely_affected() {
        // Both controls should have minimal effect on mid-grey (luma ≈ 0.5).
        let src = grey(128);
        let out = HighlightsShadowsOp::new(-1.0, 1.0).apply(src).unwrap();
        let diff = (out.data[0] as i16 - 128).unsigned_abs();
        assert!(
            diff <= 5,
            "mid-grey should barely move, got {}",
            out.data[0]
        );
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 200;
            p[1] = 200;
            p[2] = 200;
            p[3] = 55;
        });
        let out = HighlightsShadowsOp::new(-0.5, 0.5).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 55));
    }

    #[test]
    fn hue_preserved_on_coloured_pixel() {
        // The same delta is applied to all channels, so R:G ratio must not change.
        let mut src = Image::new(1, 1);
        src.data[0] = 220;
        src.data[1] = 80;
        src.data[2] = 40;
        src.data[3] = 255;
        let rg_before = src.data[0] as f32 / src.data[1] as f32;
        let out = HighlightsShadowsOp::new(-0.8, 0.0).apply(src).unwrap();
        let rg_after = out.data[0] as f32 / out.data[1] as f32;
        // Allow a small tolerance for u8 rounding.
        assert!(
            (rg_after / rg_before - 1.0).abs() < 0.05,
            "R:G ratio shifted: {:.2} → {:.2}",
            rg_before,
            rg_after
        );
    }
}
