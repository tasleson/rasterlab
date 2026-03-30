use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Rotate every pixel's hue by a fixed number of degrees.
///
/// The image is converted to HSL, the hue component is shifted by `degrees`
/// (wrapping at ±180°), and the result is converted back to RGB.  Lightness
/// and saturation are unchanged, so the shift only alters colour, not
/// brightness or vividness.
///
/// * `degrees` — amount to rotate hue.  `+90` shifts red→yellow→green;
///   `−90` shifts red→magenta→blue.  Full circle is ±180°; values outside
///   that range are wrapped automatically.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HueShiftOp {
    pub degrees: f32,
}

impl HueShiftOp {
    pub fn new(degrees: f32) -> Self {
        // Normalise to (-180, 180] so the stored value is canonical.
        let d = degrees.rem_euclid(360.0);
        let d = if d > 180.0 { d - 360.0 } else { d };
        Self { degrees: d }
    }
}

#[typetag::serde]
impl Operation for HueShiftOp {
    fn name(&self) -> &'static str {
        "hue_shift"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        if self.degrees.abs() < 1e-3 {
            return Ok(image);
        }

        // Convert degrees to the [0,1] hue fraction used by the HSL functions.
        let shift = self.degrees / 360.0;

        image.data.par_chunks_mut(4).for_each(|p| {
            let r = p[0] as f32 / 255.0;
            let g = p[1] as f32 / 255.0;
            let b = p[2] as f32 / 255.0;

            let (h, s, l) = rgb_to_hsl(r, g, b);
            // Wrap hue into [0, 1).
            let new_h = (h + shift).rem_euclid(1.0);
            let (ro, go, bo) = hsl_to_rgb(new_h, s, l);

            p[0] = (ro * 255.0).clamp(0.0, 255.0) as u8;
            p[1] = (go * 255.0).clamp(0.0, 255.0) as u8;
            p[2] = (bo * 255.0).clamp(0.0, 255.0) as u8;
            // alpha unchanged
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        format!("Hue Shift  {:+.1}°", self.degrees)
    }
}

// ---------------------------------------------------------------------------
// HSL ↔ RGB helpers (duplicated locally to keep the op self-contained)
// ---------------------------------------------------------------------------

fn rgb_to_hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) * 0.5;

    if (max - min).abs() < 1e-9 {
        return (0.0, 0.0, l);
    }

    let d = max - min;
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };

    let h = if (max - r).abs() < 1e-9 {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if (max - g).abs() < 1e-9 {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };

    (h / 6.0, s, l)
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    if s < 1e-9 {
        return (l, l, l);
    }
    let q = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let p = 2.0 * l - q;
    (
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0),
    )
}

fn hue_to_rgb(p: f32, q: f32, mut t: f32) -> f32 {
    if t < 0.0 {
        t += 1.0;
    }
    if t > 1.0 {
        t -= 1.0;
    }
    if t < 1.0 / 6.0 {
        return p + (q - p) * 6.0 * t;
    }
    if t < 0.5 {
        return q;
    }
    if t < 2.0 / 3.0 {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    p
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
    fn zero_degrees_is_identity() {
        let src = solid(200, 80, 40);
        let src_data = src.data.clone();
        let out = HueShiftOp::new(0.0).apply(src).unwrap();
        // Allow ±1 for round-trip HSL rounding.
        for (a, b) in src_data.chunks(4).zip(out.data.chunks(4)) {
            assert!((a[0] as i16 - b[0] as i16).abs() <= 1);
            assert!((a[1] as i16 - b[1] as i16).abs() <= 1);
            assert!((a[2] as i16 - b[2] as i16).abs() <= 1);
        }
    }

    #[test]
    fn grey_unchanged_by_shift() {
        // Grey pixels have no hue; shifting should leave them identical.
        let src = solid(128, 128, 128);
        let out = HueShiftOp::new(90.0).apply(src).unwrap();
        assert_eq!(out.data[0], 128);
        assert_eq!(out.data[1], 128);
        assert_eq!(out.data[2], 128);
    }

    #[test]
    fn full_rotation_returns_to_original() {
        let src = solid(200, 80, 40);
        let src_data = src.data.clone();
        let out = HueShiftOp::new(360.0).apply(src).unwrap();
        for (a, b) in src_data.chunks(4).zip(out.data.chunks(4)) {
            assert!((a[0] as i16 - b[0] as i16).abs() <= 1);
        }
    }

    #[test]
    fn red_shifts_toward_green_at_plus_120() {
        // Pure red shifted +120° should become green-ish.
        let src = solid(255, 0, 0);
        let out = HueShiftOp::new(120.0).apply(src).unwrap();
        assert!(
            out.data[1] > out.data[0],
            "green should dominate after +120°"
        );
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 200;
            p[1] = 100;
            p[2] = 50;
            p[3] = 77;
        });
        let out = HueShiftOp::new(45.0).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 77));
    }

    #[test]
    fn lightness_preserved_after_shift() {
        // HSL hue rotation preserves HSL lightness (not perceptual luma —
        // those differ because HSL is not a perceptually-uniform space).
        let src = solid(180, 100, 50);
        let out = HueShiftOp::new(90.0).apply(src).unwrap();

        let hsl_l = |r: f32, g: f32, b: f32| -> f32 { (r.max(g).max(b) + r.min(g).min(b)) * 0.5 };
        let l_in = hsl_l(180.0 / 255.0, 100.0 / 255.0, 50.0 / 255.0);
        let l_out = hsl_l(
            out.data[0] as f32 / 255.0,
            out.data[1] as f32 / 255.0,
            out.data[2] as f32 / 255.0,
        );
        assert!(
            (l_in - l_out).abs() < 0.02,
            "HSL lightness changed by {:.4}",
            (l_in - l_out).abs()
        );
    }
}
