use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

// ---------------------------------------------------------------------------
// Mask shapes
// ---------------------------------------------------------------------------

/// A linear gradient mask.  Full effect on one side of the centre line,
/// fading to zero on the other over a configurable transition zone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearMask {
    /// Midpoint of the gradient in normalised [0, 1] image coordinates.
    pub cx: f32,
    pub cy: f32,
    /// Direction the gradient runs, in degrees.
    /// 0° = left → right, 90° = top → bottom.
    pub angle_deg: f32,
    /// Width of the transition zone as a fraction of the image diagonal.
    pub feather: f32,
    /// Swap which side receives the full effect and which receives none.
    pub invert: bool,
}

impl Default for LinearMask {
    fn default() -> Self {
        Self {
            cx: 0.5,
            cy: 0.5,
            angle_deg: 90.0,
            feather: 0.5,
            invert: false,
        }
    }
}

/// A radial (elliptical) gradient mask.  Full effect inside the radius,
/// fading to zero beyond it over the feather zone.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RadialMask {
    /// Centre of the ellipse in normalised [0, 1] image coordinates.
    pub cx: f32,
    pub cy: f32,
    /// Inner radius where the effect is 100%, as a fraction of the image
    /// diagonal.
    pub radius: f32,
    /// Transition width beyond `radius`, as a fraction of `radius` itself.
    pub feather: f32,
    /// Swap inside/outside — full effect outside the ellipse, none inside.
    pub invert: bool,
}

impl Default for RadialMask {
    fn default() -> Self {
        Self {
            cx: 0.5,
            cy: 0.5,
            radius: 0.3,
            feather: 0.5,
            invert: false,
        }
    }
}

/// Which kind of mask to use.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MaskShape {
    Linear(LinearMask),
    Radial(RadialMask),
}

impl MaskShape {
    /// Returns the mask opacity in `[0.0, 1.0]` at the normalised image
    /// coordinate `(nx, ny)`.  `1.0` means the inner op is fully applied;
    /// `0.0` means the original pixel is kept unchanged.
    pub fn eval(&self, nx: f32, ny: f32) -> f32 {
        match self {
            MaskShape::Linear(m) => eval_linear(m, nx, ny),
            MaskShape::Radial(m) => eval_radial(m, nx, ny),
        }
    }
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

fn eval_linear(m: &LinearMask, nx: f32, ny: f32) -> f32 {
    let rad = m.angle_deg.to_radians();
    let (cos_a, sin_a) = (rad.cos(), rad.sin());

    // Signed distance along the gradient direction from the midpoint.
    let proj = (nx - m.cx) * cos_a + (ny - m.cy) * sin_a;

    let half = (m.feather * 0.5).max(1e-4);
    let t = ((proj + half) / (half * 2.0)).clamp(0.0, 1.0);
    let opacity = smoothstep(t);

    if m.invert { 1.0 - opacity } else { opacity }
}

fn eval_radial(m: &RadialMask, nx: f32, ny: f32) -> f32 {
    let dist = ((nx - m.cx).powi(2) + (ny - m.cy).powi(2)).sqrt();

    let inner = m.radius;
    let outer = inner + inner * m.feather.max(1e-4);
    let zone = (outer - inner).max(1e-4);

    // t=0 inside the radius (full effect), t=1 at/beyond the outer edge.
    let t = ((dist - inner) / zone).clamp(0.0, 1.0);
    let opacity = 1.0 - smoothstep(t);

    if m.invert { 1.0 - opacity } else { opacity }
}

// ---------------------------------------------------------------------------
// MaskedOp
// ---------------------------------------------------------------------------

/// Wraps any operation with a spatial mask.
///
/// The inner operation is applied to the full image, then each pixel is
/// blended back with the original using the mask opacity:
/// `result = original + (adjusted - original) * opacity`.
///
/// Operations that change image dimensions (crop, resize) are let through
/// unchanged — spatial masks are undefined when geometry changes.
#[derive(Serialize, Deserialize)]
pub struct MaskedOp {
    pub inner: Box<dyn Operation>,
    pub mask: MaskShape,
}

impl std::fmt::Debug for MaskedOp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MaskedOp")
            .field("inner", &self.inner.name())
            .field("mask", &self.mask)
            .finish()
    }
}

impl Clone for MaskedOp {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone_box(),
            mask: self.mask.clone(),
        }
    }
}

#[typetag::serde]
impl Operation for MaskedOp {
    fn name(&self) -> &'static str {
        "masked"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn apply(&self, image: Image) -> RasterResult<Image> {
        let w = image.width;
        let h = image.height;

        // Keep original pixels for blending.
        let before = image.deep_clone();
        let mut after = self.inner.apply(image)?;

        // Dimension-changing ops (crop, resize) pass through unblended.
        if after.width != w || after.height != h {
            return Ok(after);
        }

        // Per-row blend: result = before + (after - before) * opacity.
        // Row-level chunks keep L1 accumulators cache-hot; the mask eval
        // is pure arithmetic (~10 ops/pixel) so this is compute-bound and
        // benefits from Rayon parallelism.
        after
            .data
            .par_chunks_mut(w as usize * 4)
            .enumerate()
            .for_each(|(y, after_row)| {
                let ny = (y as f32 + 0.5) / h as f32;
                let row_start = y * w as usize * 4;
                let before_row = &before.data[row_start..row_start + w as usize * 4];
                for x in 0..w as usize {
                    let nx = (x as f32 + 0.5) / w as f32;
                    let opacity = self.mask.eval(nx, ny);
                    let off = x * 4;
                    for c in 0..3 {
                        let b = before_row[off + c] as f32;
                        let a = after_row[off + c] as f32;
                        after_row[off + c] = (b + (a - b) * opacity).round() as u8;
                    }
                    // Alpha is left as whatever the inner op produced.
                }
            });

        Ok(after)
    }

    fn describe(&self) -> String {
        let shape = match &self.mask {
            MaskShape::Linear(_) => "linear",
            MaskShape::Radial(_) => "radial",
        };
        format!("{} ({})", self.inner.describe(), shape)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::BrightnessContrastOp;

    fn grey_image(w: u32, h: u32, v: u8) -> Image {
        let mut img = Image::new(w, h);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = v;
            p[1] = v;
            p[2] = v;
            p[3] = 255;
        });
        img
    }

    #[test]
    fn full_opacity_matches_inner_op() {
        // A radial mask that covers the whole image (huge radius) should
        // produce the same result as the inner op applied without masking.
        let src = grey_image(32, 32, 128);
        let expected = grey_image(32, 32, 128);
        let expected = BrightnessContrastOp::new(0.2, 0.0).apply(expected).unwrap();

        let masked = MaskedOp {
            inner: Box::new(BrightnessContrastOp::new(0.2, 0.0)),
            mask: MaskShape::Radial(RadialMask {
                cx: 0.5,
                cy: 0.5,
                radius: 10.0, // much larger than the image
                feather: 0.0,
                invert: false,
            }),
        };
        let out = masked.apply(src).unwrap();
        assert_eq!(out.data, expected.data);
    }

    #[test]
    fn zero_opacity_is_identity() {
        // A radial mask with very small radius + zero feather should leave
        // the image unchanged at the corners (opacity ≈ 0).
        let src = grey_image(64, 64, 128);
        let masked = MaskedOp {
            inner: Box::new(BrightnessContrastOp::new(0.5, 0.0)),
            mask: MaskShape::Radial(RadialMask {
                cx: 0.5,
                cy: 0.5,
                radius: 0.001,
                feather: 0.0,
                invert: false,
            }),
        };
        let out = masked.apply(src).unwrap();
        // Corner pixel at (0, 0) should be essentially unchanged.
        let [r, ..] = out.pixel(0, 0);
        assert!(
            (r as i32 - 128).abs() <= 1,
            "corner pixel {r} should be ≈128"
        );
    }

    #[test]
    fn linear_midpoint_is_half_blend() {
        // At the midpoint of a linear mask the opacity should be ≈0.5.
        let mask = LinearMask {
            cx: 0.5,
            cy: 0.5,
            angle_deg: 0.0,
            feather: 0.5,
            invert: false,
        };
        let o = eval_linear(&mask, 0.5, 0.5);
        assert!(
            (o - 0.5).abs() < 0.05,
            "midpoint opacity {o} should be ≈0.5"
        );
    }
}
