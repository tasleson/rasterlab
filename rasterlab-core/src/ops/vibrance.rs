use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

use super::hsl::{hsl_to_rgb, rgb_to_hsl};

/// Intelligent saturation boost that protects already-vivid colours.
///
/// Unlike plain saturation (which multiplies every pixel's chroma by the
/// same factor), vibrance applies a **saturation-weighted** boost: pixels
/// that are already highly saturated receive little or no boost, while
/// muted or pastel pixels are lifted more strongly.  The result is richer
/// colours without blowing out already-vivid primaries or distorting skin
/// tones.
///
/// Algorithm:
/// 1. Convert to HSL.
/// 2. Compute a boost weight = `(1 − S)²` — smoothly falls to zero as
///    current saturation approaches 1.
/// 3. New S = clamp(S + strength × weight, 0, 1).
/// 4. Convert back to RGB.
///
/// * `strength` — saturation increase.  `0.0` = no change; `1.0` = maximum
///   boost (muted colours approach full saturation while vivid colours are
///   barely touched).  Negative values gently desaturate muted colours.
///   Range `[-1.0, 1.0]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VibranceOp {
    pub strength: f32,
}

impl VibranceOp {
    pub fn new(strength: f32) -> Self {
        Self {
            strength: strength.clamp(-1.0, 1.0),
        }
    }
}

#[typetag::serde]
impl Operation for VibranceOp {
    fn name(&self) -> &'static str {
        "vibrance"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        if self.strength.abs() < 1e-5 {
            return Ok(image);
        }

        let strength = self.strength;

        image.data.par_chunks_mut(4).for_each(|p| {
            let r = p[0] as f32 / 255.0;
            let g = p[1] as f32 / 255.0;
            let b = p[2] as f32 / 255.0;

            let (h, s, l) = rgb_to_hsl(r, g, b);

            // Achromatic pixels (s=0) have no hue to boost; skip them.
            if s < 1e-6 {
                return;
            }

            // Boost weight: strongest for muted (s≈0), zero for vivid (s≈1).
            let weight = (1.0 - s).powi(2);
            let new_s = (s + strength * weight).clamp(0.0, 1.0);

            let (ro, go, bo) = hsl_to_rgb(h, new_s, l);
            p[0] = (ro * 255.0).clamp(0.0, 255.0) as u8;
            p[1] = (go * 255.0).clamp(0.0, 255.0) as u8;
            p[2] = (bo * 255.0).clamp(0.0, 255.0) as u8;
            // alpha unchanged
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        format!("Vibrance  {:+.0}%", self.strength * 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(r: u8, g: u8, b: u8) -> Image {
        let mut img = Image::new(4, 4);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = r;
            p[1] = g;
            p[2] = b;
            p[3] = 255;
        });
        img
    }

    #[test]
    fn zero_strength_is_identity() {
        let src = solid(180, 80, 40);
        let src_data = src.data.clone();
        let out = VibranceOp::new(0.0).apply(src).unwrap();
        for (a, b) in src_data.chunks(4).zip(out.data.chunks(4)) {
            assert!((a[0] as i16 - b[0] as i16).abs() <= 1);
        }
    }

    #[test]
    fn grey_unchanged() {
        // Grey has S=0, but after applying vibrance it should stay grey
        // (weight is high, but boosting S on grey has no visual effect).
        let src = solid(128, 128, 128);
        let src_data = src.data.clone();
        let out = VibranceOp::new(1.0).apply(src).unwrap();
        for (a, b) in src_data.chunks(4).zip(out.data.chunks(4)) {
            assert!((a[0] as i16 - b[0] as i16).abs() <= 1);
        }
    }

    #[test]
    fn muted_colour_boosted_more_than_vivid() {
        // Muted pixel: lower saturation → larger delta expected.
        // Vivid pixel: already saturated → smaller delta expected.
        let muted = solid(160, 130, 110); // low S
        let vivid = solid(255, 10, 10); // high S

        let muted_data = muted.data.clone();
        let vivid_data = vivid.data.clone();
        let muted_out = VibranceOp::new(1.0).apply(muted).unwrap();
        let vivid_out = VibranceOp::new(1.0).apply(vivid).unwrap();

        // Compare how much the max-min chroma range changed.
        let chroma = |p: &[u8]| p[0].abs_diff(p[1]).max(p[1].abs_diff(p[2])) as i32;

        let muted_delta = chroma(&muted_out.data[..4]) - chroma(&muted_data[..4]);
        let vivid_delta = chroma(&vivid_out.data[..4]) - chroma(&vivid_data[..4]);

        assert!(
            muted_delta > vivid_delta,
            "muted delta {} should exceed vivid delta {}",
            muted_delta,
            vivid_delta
        );
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 180;
            p[1] = 100;
            p[2] = 60;
            p[3] = 88;
        });
        let out = VibranceOp::new(0.8).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 88));
    }
}
