use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

use super::hsl::{hsl_to_rgb, rgb_to_hsl};

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

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        if (self.saturation - 1.0).abs() < 1e-5 {
            return Ok(image);
        }

        let s_factor = self.saturation;

        image.data.par_chunks_mut(4).for_each(|p| {
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

        Ok(image)
    }

    fn describe(&self) -> String {
        format!("Saturation  {:.0}%", self.saturation * 100.0)
    }
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
        let out = SaturationOp::new(1.0).apply(src.deep_clone()).unwrap();
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
        let out = SaturationOp::new(0.0).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[0], p[1]));
        out.data.chunks(4).for_each(|p| assert_eq!(p[1], p[2]));
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(2, 2);
        src.data.chunks_mut(4).for_each(|p| {
            p[3] = 42;
        });
        let out = SaturationOp::new(2.0).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 42));
    }
}
