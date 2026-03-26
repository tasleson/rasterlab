use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Color space conversion for the image pixels.
///
/// All conversions work in **linear light**.  The image is assumed to be in
/// the source color space already; the op converts it to the destination space
/// and re-encodes with the destination transfer function.
///
/// Supported conversions
/// ---------------------
/// * `SrgbToDisplayP3` — sRGB → Display P3
///   Maps sRGB primaries to Display P3 primaries (same D65 white point).
///   Gamut-clips values that fall outside P3.  This is the most common
///   conversion for preparing images for wide-gamut displays.
///
/// * `DisplayP3ToSrgb` — Display P3 → sRGB
///   Inverse of the above.  Out-of-sRGB-gamut values are clipped.
///
/// The actual pixel data stored in the `Image` byte buffer is always
/// 8-bit sRGB-ish (no embedded profile metadata), so this op re-maps
/// the numeric values as if they belong to the source space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColorSpaceConversion {
    SrgbToDisplayP3,
    DisplayP3ToSrgb,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorSpaceOp {
    pub conversion: ColorSpaceConversion,
}

impl ColorSpaceOp {
    pub fn new(conversion: ColorSpaceConversion) -> Self {
        Self { conversion }
    }
}

// ---------------------------------------------------------------------------
// Transfer functions
// ---------------------------------------------------------------------------

/// sRGB gamma → linear (exact piecewise formula).
#[inline]
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear → sRGB gamma.
#[inline]
fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

// ---------------------------------------------------------------------------
// 3×3 matrix × [r, g, b] in linear light
// ---------------------------------------------------------------------------

#[inline]
fn mat3_mul(m: &[f32; 9], r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    (
        m[0] * r + m[1] * g + m[2] * b,
        m[3] * r + m[4] * g + m[5] * b,
        m[6] * r + m[7] * g + m[8] * b,
    )
}

// ---------------------------------------------------------------------------
// Chromatic adaptation + primary conversion matrices (D65 white point)
//
// sRGB primaries:      R(0.64, 0.33) G(0.30, 0.60) B(0.15, 0.06)
// Display P3 primaries: R(0.680, 0.320) G(0.265, 0.690) B(0.150, 0.060)
//
// The matrices below convert XYZ ↔ each color space and are combined to give
// direct sRGB ↔ P3 3×3 transforms.  Values taken from ICC / colour-science.
// ---------------------------------------------------------------------------

// sRGB (linear) → Display P3 (linear)
const SRGB_TO_P3: [f32; 9] = [
    0.822_458, 0.177_542, 0.000_000, 0.033_194, 0.966_806, 0.000_000, 0.017_082, 0.072_397,
    0.910_521,
];

// Display P3 (linear) → sRGB (linear)
const P3_TO_SRGB: [f32; 9] = [
    1.224_94, -0.224_94, 0.000_00, -0.042_057, 1.042_057, 0.000_00, -0.019_637, -0.078_636,
    1.098_273,
];

#[typetag::serde]
impl Operation for ColorSpaceOp {
    fn name(&self) -> &'static str {
        "color_space"
    }

    fn apply(&self, image: &Image) -> RasterResult<Image> {
        let mat = match self.conversion {
            ColorSpaceConversion::SrgbToDisplayP3 => &SRGB_TO_P3,
            ColorSpaceConversion::DisplayP3ToSrgb => &P3_TO_SRGB,
        };

        let mut out = image.deep_clone();

        out.data
            .par_chunks_mut(4)
            .zip(image.data.par_chunks(4))
            .for_each(|(dst, src)| {
                // Decode gamma.
                let r = srgb_to_linear(src[0] as f32 / 255.0);
                let g = srgb_to_linear(src[1] as f32 / 255.0);
                let b = srgb_to_linear(src[2] as f32 / 255.0);

                // Apply primaries conversion in linear light.
                let (ro, go, bo) = mat3_mul(mat, r, g, b);

                // Re-encode with destination transfer function (sRGB-style).
                dst[0] = (linear_to_srgb(ro.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8;
                dst[1] = (linear_to_srgb(go.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8;
                dst[2] = (linear_to_srgb(bo.clamp(0.0, 1.0)) * 255.0 + 0.5) as u8;
                // alpha unchanged
            });

        Ok(out)
    }

    fn describe(&self) -> String {
        match self.conversion {
            ColorSpaceConversion::SrgbToDisplayP3 => "sRGB → Display P3".into(),
            ColorSpaceConversion::DisplayP3ToSrgb => "Display P3 → sRGB".into(),
        }
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
    fn round_trip_grey() {
        // Neutral grey should survive a round-trip (P3 and sRGB share D65 white).
        let src = solid(128, 128, 128);
        let fwd = ColorSpaceOp::new(ColorSpaceConversion::SrgbToDisplayP3)
            .apply(&src)
            .unwrap();
        let back = ColorSpaceOp::new(ColorSpaceConversion::DisplayP3ToSrgb)
            .apply(&fwd)
            .unwrap();
        for p in back.data.chunks(4) {
            assert!((p[0] as i16 - 128).abs() <= 2);
        }
    }

    #[test]
    fn white_preserved() {
        let src = solid(255, 255, 255);
        let out = ColorSpaceOp::new(ColorSpaceConversion::SrgbToDisplayP3)
            .apply(&src)
            .unwrap();
        for p in out.data.chunks(4) {
            assert_eq!(p[0], 255);
            assert_eq!(p[1], 255);
            assert_eq!(p[2], 255);
        }
    }

    #[test]
    fn black_preserved() {
        let src = solid(0, 0, 0);
        let out = ColorSpaceOp::new(ColorSpaceConversion::SrgbToDisplayP3)
            .apply(&src)
            .unwrap();
        for p in out.data.chunks(4) {
            assert_eq!(p[0], 0);
            assert_eq!(p[1], 0);
            assert_eq!(p[2], 0);
        }
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 100;
            p[1] = 150;
            p[2] = 200;
            p[3] = 77;
        });
        let out = ColorSpaceOp::new(ColorSpaceConversion::SrgbToDisplayP3)
            .apply(&src)
            .unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 77));
    }

    #[test]
    fn srgb_to_p3_shifts_primaries() {
        // sRGB red in P3 encoding should have its R slightly reduced and
        // G slightly increased (P3 has a wider green, so same display colour
        // requires less red-channel contribution).
        let src = solid(200, 20, 20);
        let out = ColorSpaceOp::new(ColorSpaceConversion::SrgbToDisplayP3)
            .apply(&src)
            .unwrap();
        // R should decrease (P3 red primary covers less linear red).
        assert!(out.data[0] < src.data[0]);
    }
}
