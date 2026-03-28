use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Split toning: tint shadows and highlights with independent hue/saturation.
///
/// Luminance is used to smoothly blend between the two tints — pure blacks
/// receive only the shadow colour, pure whites only the highlight colour, and
/// midtones receive a mix of both.  The `balance` slider shifts the crossover
/// point toward shadows (negative) or highlights (positive).
///
/// * `shadow_hue`        — hue of the shadow tint, degrees `[0, 360)`.
/// * `shadow_sat`        — saturation of the shadow tint `[0.0, 1.0]`.
/// * `highlight_hue`     — hue of the highlight tint, degrees `[0, 360)`.
/// * `highlight_sat`     — saturation of the highlight tint `[0.0, 1.0]`.
/// * `balance`           — crossover bias `[-1.0, 1.0]`, 0 = neutral.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SplitToneOp {
    pub shadow_hue: f32,
    pub shadow_sat: f32,
    pub highlight_hue: f32,
    pub highlight_sat: f32,
    pub balance: f32,
}

impl Default for SplitToneOp {
    fn default() -> Self {
        Self {
            shadow_hue: 220.0, // cool blue shadows
            shadow_sat: 0.20,
            highlight_hue: 40.0, // warm gold highlights
            highlight_sat: 0.15,
            balance: 0.0,
        }
    }
}

impl SplitToneOp {
    pub fn new(
        shadow_hue: f32,
        shadow_sat: f32,
        highlight_hue: f32,
        highlight_sat: f32,
        balance: f32,
    ) -> Self {
        Self {
            shadow_hue: shadow_hue.rem_euclid(360.0),
            shadow_sat: shadow_sat.clamp(0.0, 1.0),
            highlight_hue: highlight_hue.rem_euclid(360.0),
            highlight_sat: highlight_sat.clamp(0.0, 1.0),
            balance: balance.clamp(-1.0, 1.0),
        }
    }
}

/// Convert a hue (degrees) at full saturation and value to an RGB triple in [0,1].
fn hue_to_rgb(hue: f32) -> (f32, f32, f32) {
    let h = hue.rem_euclid(360.0) / 60.0;
    let i = h.floor() as u32;
    let f = h - i as f32;
    let (r, g, b) = match i {
        0 => (1.0, f, 0.0),
        1 => (1.0 - f, 1.0, 0.0),
        2 => (0.0, 1.0, f),
        3 => (0.0, 1.0 - f, 1.0),
        4 => (f, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - f),
    };
    (r, g, b)
}

#[typetag::serde]
impl Operation for SplitToneOp {
    fn name(&self) -> &'static str {
        "split_tone"
    }

    fn apply(&self, image: &Image) -> RasterResult<Image> {
        if self.shadow_sat < 1e-4 && self.highlight_sat < 1e-4 {
            return Ok(image.deep_clone());
        }

        let (sh_r, sh_g, sh_b) = hue_to_rgb(self.shadow_hue);
        let (hi_r, hi_g, hi_b) = hue_to_rgb(self.highlight_hue);
        let shadow_sat = self.shadow_sat;
        let highlight_sat = self.highlight_sat;
        // balance shifts the midpoint: +1 pushes it toward highlights (luma 1.0),
        // -1 toward shadows (luma 0.0). We bias the luma value before weighting.
        let balance = self.balance * 0.5; // scale to keep effect subtle

        let mut out = image.deep_clone();
        out.data
            .par_chunks_mut(4)
            .zip(image.data.par_chunks(4))
            .for_each(|(dst, src)| {
                let r = src[0] as f32 / 255.0;
                let g = src[1] as f32 / 255.0;
                let b = src[2] as f32 / 255.0;

                // Perceptual luminance (BT.709).
                let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                // Bias luma by balance so the crossover shifts.
                let luma_b = (luma + balance).clamp(0.0, 1.0);

                // Shadow weight: strong in dark areas, falls off toward highlights.
                // Highlight weight: strong in bright areas, falls off toward shadows.
                // Using squared falloff gives a smoother, more photographic feel.
                let shadow_w = (1.0 - luma_b).powi(2) * shadow_sat;
                let highlight_w = luma_b.powi(2) * highlight_sat;

                // Lerp toward each tint colour, clamped to [0, 1].
                let nr = (r + (sh_r - r) * shadow_w + (hi_r - r) * highlight_w).clamp(0.0, 1.0);
                let ng = (g + (sh_g - g) * shadow_w + (hi_g - g) * highlight_w).clamp(0.0, 1.0);
                let nb = (b + (sh_b - b) * shadow_w + (hi_b - b) * highlight_w).clamp(0.0, 1.0);

                dst[0] = (nr * 255.0).round() as u8;
                dst[1] = (ng * 255.0).round() as u8;
                dst[2] = (nb * 255.0).round() as u8;
                // alpha unchanged
            });

        Ok(out)
    }

    fn describe(&self) -> String {
        format!(
            "Split Tone  sh {:.0}°/{:.0}%  hi {:.0}°/{:.0}%",
            self.shadow_hue,
            self.shadow_sat * 100.0,
            self.highlight_hue,
            self.highlight_sat * 100.0,
        )
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
    fn zero_sat_is_identity() {
        let src = solid(100, 150, 200);
        let op = SplitToneOp::new(220.0, 0.0, 40.0, 0.0, 0.0);
        let out = op.apply(&src).unwrap();
        assert_eq!(out.data, src.data);
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 50;
            p[1] = 50;
            p[2] = 50;
            p[3] = 77;
        });
        let op = SplitToneOp::default();
        let out = op.apply(&src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 77));
    }

    #[test]
    fn pure_black_receives_shadow_tint() {
        // Black pixels (luma = 0) should receive only shadow tint (hue 0° = red).
        let src = solid(0, 0, 0);
        let op = SplitToneOp::new(0.0, 1.0, 180.0, 0.0, 0.0);
        let out = op.apply(&src).unwrap();
        // With shadow hue = 0° (red), black should become reddish: R > B.
        // (black lerps toward red, so R increases most)
        assert!(out.data[0] > out.data[2], "black should become reddish");
    }

    #[test]
    fn pure_white_receives_highlight_tint() {
        // White pixels (luma = 1) should receive only highlight tint (hue 240° = blue).
        let src = solid(255, 255, 255);
        let op = SplitToneOp::new(0.0, 0.0, 240.0, 1.0, 0.0);
        let out = op.apply(&src).unwrap();
        // With highlight hue = 240° (blue), white lerps toward blue: B stays highest
        // relative to R.
        assert!(out.data[2] >= out.data[0], "white should lean blue");
    }
}
