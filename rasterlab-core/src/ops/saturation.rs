use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Adjust colour saturation via HSL conversion.
///
/// * `saturation` — `0.0` = fully desaturated (greyscale), `1.0` = no change,
///   `2.0` = doubled saturation.  Clamped to `[0.0, 4.0]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaturationOp {
    pub saturation: f32,
}

impl SaturationOp {
    pub fn new(saturation: f32) -> Self {
        Self {
            saturation: saturation.clamp(0.0, 4.0),
        }
    }
}

#[typetag::serde]
impl Operation for SaturationOp {
    fn name(&self) -> &'static str {
        "saturation"
    }

    fn apply(&self, image: &Image) -> RasterResult<Image> {
        if (self.saturation - 1.0).abs() < 1e-5 {
            return Ok(image.deep_clone());
        }

        let s_factor = self.saturation;
        let mut out = image.deep_clone();

        out.data.par_chunks_mut(4).for_each(|p| {
            let r = p[0] as f32 / 255.0;
            let g = p[1] as f32 / 255.0;
            let b = p[2] as f32 / 255.0;

            let (h, s, l) = rgb_to_hsl(r, g, b);
            let new_s = (s * s_factor).clamp(0.0, 1.0);
            let (r2, g2, b2) = hsl_to_rgb(h, new_s, l);

            p[0] = (r2 * 255.0).clamp(0.0, 255.0) as u8;
            p[1] = (g2 * 255.0).clamp(0.0, 255.0) as u8;
            p[2] = (b2 * 255.0).clamp(0.0, 255.0) as u8;
            // alpha untouched
        });

        Ok(out)
    }

    fn describe(&self) -> String {
        format!("Saturation  {:.0}%", self.saturation * 100.0)
    }
}

// ---------------------------------------------------------------------------
// HSL ↔ RGB helpers
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

    #[test]
    fn identity() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 100;
            p[1] = 150;
            p[2] = 200;
            p[3] = 255;
        });
        let out = SaturationOp::new(1.0).apply(&src).unwrap();
        // Should be unchanged (within rounding)
        for (a, b) in src.data.chunks(4).zip(out.data.chunks(4)) {
            assert!((a[0] as i16 - b[0] as i16).abs() <= 1);
        }
    }

    #[test]
    fn zero_gives_greyscale() {
        let mut src = Image::new(2, 2);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 200;
            p[1] = 100;
            p[2] = 50;
            p[3] = 255;
        });
        let out = SaturationOp::new(0.0).apply(&src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[0], p[1]));
        out.data.chunks(4).for_each(|p| assert_eq!(p[1], p[2]));
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(2, 2);
        src.data.chunks_mut(4).for_each(|p| {
            p[3] = 42;
        });
        let out = SaturationOp::new(2.0).apply(&src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 42));
    }
}
