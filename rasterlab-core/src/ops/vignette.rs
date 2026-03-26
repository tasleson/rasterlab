use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Apply a radial vignette — darkens the corners/edges of the image.
///
/// The falloff is elliptical (follows the image aspect ratio) so it always
/// looks circular on screen regardless of image dimensions.  Distance is
/// normalized so that the corners of the image are at d = 1.0.
///
/// # Parameters
/// * `strength` — how much to darken; `0.0` = no effect, `1.0` = corners go black.
/// * `radius` — normalized distance at which the vignette begins to appear.
///   `0.0` = starts at the centre, `1.0` = starts at the very corners.
/// * `feather` — width of the smooth transition zone as a fraction of the
///   remaining distance from `radius` to the corners.
///   `0.0` = hard edge, `1.0` = feathers all the way from `radius` to the corners.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VignetteOp {
    /// Darkening amount.  Clamped to `[0.0, 1.0]`.
    pub strength: f32,
    /// Inner edge of the vignette, normalized to corners = 1.0.  Clamped to `[0.0, 1.0]`.
    pub radius: f32,
    /// Feathering width.  Clamped to `[0.0, 1.0]`.
    pub feather: f32,
}

impl VignetteOp {
    pub fn new(strength: f32, radius: f32, feather: f32) -> Self {
        Self {
            strength: strength.clamp(0.0, 1.0),
            radius: radius.clamp(0.0, 1.0),
            feather: feather.clamp(0.0, 1.0),
        }
    }
}

#[typetag::serde]
impl Operation for VignetteOp {
    fn name(&self) -> &'static str {
        "vignette"
    }

    fn apply(&self, image: &Image) -> RasterResult<Image> {
        let w = image.width as usize;
        let h = image.height as usize;

        // Half-dimensions for elliptical normalization.
        // Dividing by half_w/half_h maps each axis to [-1, 1], so the
        // ellipse exactly fits the image rectangle.  Dividing the resulting
        // Euclidean distance by sqrt(2) puts the corners at d = 1.0 and
        // all four edge midpoints at d ≈ 0.707, giving a uniform vignette
        // around the entire frame regardless of aspect ratio.
        let half_w = w as f32 / 2.0;
        let half_h = h as f32 / 2.0;
        let inv_sqrt2 = std::f32::consts::FRAC_1_SQRT_2; // 1/√2

        // Outer edge of the feather zone — guaranteed in [radius, 1.0].
        let inner = self.radius;
        let outer = inner + self.feather * (1.0 - inner);
        let zone = (outer - inner).max(1e-6); // avoid division by zero

        let mut out = image.deep_clone();

        out.data
            .par_chunks_mut(w * 4)
            .enumerate()
            .for_each(|(y, row)| {
                let dy = (y as f32 + 0.5 - half_h) / half_h;
                for x in 0..w {
                    let dx = (x as f32 + 0.5 - half_w) / half_w;
                    // Elliptical distance: corners land at sqrt(2), so
                    // multiply by 1/√2 to normalize corners to 1.0.
                    let d = (dx * dx + dy * dy).sqrt() * inv_sqrt2;

                    let t = ((d - inner) / zone).clamp(0.0, 1.0);
                    // Smoothstep for a pleasing falloff curve.
                    let t_smooth = t * t * (3.0 - 2.0 * t);
                    let factor = 1.0 - self.strength * t_smooth;

                    let off = x * 4;
                    row[off] = (row[off] as f32 * factor).clamp(0.0, 255.0) as u8;
                    row[off + 1] = (row[off + 1] as f32 * factor).clamp(0.0, 255.0) as u8;
                    row[off + 2] = (row[off + 2] as f32 * factor).clamp(0.0, 255.0) as u8;
                    // alpha unchanged
                }
            });

        Ok(out)
    }

    fn describe(&self) -> String {
        format!(
            "Vignette  {:.0}%  r={:.2}  f={:.2}",
            self.strength * 100.0,
            self.radius,
            self.feather,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white_image(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = 255;
            p[1] = 255;
            p[2] = 255;
            p[3] = 255;
        });
        img
    }

    #[test]
    fn zero_strength_is_identity() {
        let src = white_image(16, 16);
        let out = VignetteOp::new(0.0, 0.5, 0.5).apply(&src).unwrap();
        assert_eq!(out.data, src.data);
    }

    #[test]
    fn centre_pixel_unaffected() {
        // With radius > 0 the centre pixel should be untouched.
        let src = white_image(16, 16);
        let out = VignetteOp::new(1.0, 0.5, 0.5).apply(&src).unwrap();
        let [r, g, b, a] = out.pixel(8, 8);
        assert_eq!([r, g, b, a], [255, 255, 255, 255]);
    }

    #[test]
    fn corners_are_darkened() {
        let src = white_image(64, 64);
        let out = VignetteOp::new(1.0, 0.0, 1.0).apply(&src).unwrap();
        // Corner pixel should be darker than the centre.
        let [cr, ..] = out.pixel(32, 32);
        let [cor, ..] = out.pixel(0, 0);
        assert!(
            cor < cr,
            "corner ({cor}) should be darker than centre ({cr})"
        );
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(8, 8);
        src.data.chunks_mut(4).for_each(|p| {
            p[3] = 128;
        });
        let out = VignetteOp::new(1.0, 0.0, 1.0).apply(&src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 128));
    }
}
