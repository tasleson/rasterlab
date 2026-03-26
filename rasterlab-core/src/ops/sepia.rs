use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Apply a classic sepia-tone effect.
///
/// Each pixel is first converted to luminance (BT.709), then tinted with
/// the traditional warm brown sepia palette using the standard matrix
/// coefficients.  The `strength` parameter blends between the original
/// colour image (`0.0`) and full sepia (`1.0`), so partial application
/// gives a subtle warm toning effect.
///
/// * `strength` — blend from original to full sepia.  Range `[0.0, 1.0]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SepiaOp {
    pub strength: f32,
}

impl SepiaOp {
    pub fn new(strength: f32) -> Self {
        Self {
            strength: strength.clamp(0.0, 1.0),
        }
    }
}

#[typetag::serde]
impl Operation for SepiaOp {
    fn name(&self) -> &'static str {
        "sepia"
    }

    fn apply(&self, image: &Image) -> RasterResult<Image> {
        if self.strength < 1e-5 {
            return Ok(image.deep_clone());
        }

        let s = self.strength;
        let mut out = image.deep_clone();

        out.data
            .par_chunks_mut(4)
            .zip(image.data.par_chunks(4))
            .for_each(|(dst, src)| {
                let r = src[0] as f32;
                let g = src[1] as f32;
                let b = src[2] as f32;

                // Standard sepia matrix (linear RGB, values in [0, 255]).
                let sr = (r * 0.393 + g * 0.769 + b * 0.189).min(255.0);
                let sg = (r * 0.349 + g * 0.686 + b * 0.168).min(255.0);
                let sb = (r * 0.272 + g * 0.534 + b * 0.131).min(255.0);

                // Blend with original.
                dst[0] = (r + (sr - r) * s) as u8;
                dst[1] = (g + (sg - g) * s) as u8;
                dst[2] = (b + (sb - b) * s) as u8;
                // alpha unchanged
            });

        Ok(out)
    }

    fn describe(&self) -> String {
        format!("Sepia  {:.0}%", self.strength * 100.0)
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
        let src = solid(100, 150, 200);
        let out = SepiaOp::new(0.0).apply(&src).unwrap();
        assert_eq!(out.data, src.data);
    }

    #[test]
    fn full_sepia_has_warm_tone() {
        // Full sepia on mid-grey should produce R > G > B (warm brown).
        let src = solid(128, 128, 128);
        let out = SepiaOp::new(1.0).apply(&src).unwrap();
        let r = out.data[0];
        let g = out.data[1];
        let b = out.data[2];
        assert!(r > g, "sepia: R should exceed G");
        assert!(g > b, "sepia: G should exceed B");
    }

    #[test]
    fn black_stays_black() {
        let src = solid(0, 0, 0);
        let out = SepiaOp::new(1.0).apply(&src).unwrap();
        out.data
            .chunks(4)
            .for_each(|p| assert_eq!(&p[..3], &[0, 0, 0]));
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 100;
            p[1] = 100;
            p[2] = 100;
            p[3] = 42;
        });
        let out = SepiaOp::new(1.0).apply(&src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 42));
    }

    #[test]
    fn partial_strength_blends() {
        let src = solid(128, 128, 128);
        let full = SepiaOp::new(1.0).apply(&src).unwrap();
        let half = SepiaOp::new(0.5).apply(&src).unwrap();
        let orig_r = 128i16;
        let full_r = full.data[0] as i16;
        let half_r = half.data[0] as i16;
        assert!(half_r >= orig_r.min(full_r) && half_r <= orig_r.max(full_r));
    }
}
