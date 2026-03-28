use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Adjust white balance via temperature and tint.
///
/// Both parameters use a multiplicative per-channel scale so that black
/// pixels remain black and the effect is proportional to pixel brightness
/// (consistent with how colour temperature behaves on real sensors).
///
/// * `temperature` — blue-orange axis.  Positive = warm (more red / less
///   blue, like tungsten light); negative = cool (more blue / less red,
///   like shade or overcast).  Range `[-1.0, 1.0]`.
/// * `tint` — green-magenta axis.  Positive = magenta shift (less green);
///   negative = green shift (more green).  Range `[-1.0, 1.0]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhiteBalanceOp {
    pub temperature: f32,
    pub tint: f32,
}

impl WhiteBalanceOp {
    pub fn new(temperature: f32, tint: f32) -> Self {
        Self {
            temperature: temperature.clamp(-1.0, 1.0),
            tint: tint.clamp(-1.0, 1.0),
        }
    }
}

#[typetag::serde]
impl Operation for WhiteBalanceOp {
    fn name(&self) -> &'static str {
        "white_balance"
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        if self.temperature.abs() < 1e-5 && self.tint.abs() < 1e-5 {
            return Ok(image);
        }

        let temp = self.temperature;
        let tint = self.tint;

        // Pre-compute per-channel multipliers once.
        // Temperature: R and B move in opposite directions (warm = R up / B down).
        // Tint: G moves (positive = magenta = less green).
        // ±30 % max on temperature, ±15 % max on tint — large enough to correct
        // significant colour casts without blowing out at moderate slider values.
        let r_scale = 1.0 + temp * 0.3;
        let g_scale = 1.0 - tint * 0.15;
        let b_scale = 1.0 - temp * 0.3;

        image.data.par_chunks_mut(4).for_each(|p| {
            p[0] = ((p[0] as f32 * r_scale).clamp(0.0, 255.0)) as u8;
            p[1] = ((p[1] as f32 * g_scale).clamp(0.0, 255.0)) as u8;
            p[2] = ((p[2] as f32 * b_scale).clamp(0.0, 255.0)) as u8;
            // alpha unchanged
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        format!(
            "White Balance  temp={:+.2}  tint={:+.2}",
            self.temperature, self.tint
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
        let src = grey(128);
        let out = WhiteBalanceOp::new(0.0, 0.0).apply(src.deep_clone()).unwrap();
        assert_eq!(out.data, src.data);
    }

    #[test]
    fn warm_raises_red_lowers_blue() {
        let src = grey(128);
        let out = WhiteBalanceOp::new(0.5, 0.0).apply(src.deep_clone()).unwrap();
        let p = &out.data[..4];
        assert!(p[0] > 128, "R should increase");
        assert_eq!(p[1], 128, "G should be unchanged");
        assert!(p[2] < 128, "B should decrease");
    }

    #[test]
    fn cool_lowers_red_raises_blue() {
        let src = grey(128);
        let out = WhiteBalanceOp::new(-0.5, 0.0).apply(src.deep_clone()).unwrap();
        let p = &out.data[..4];
        assert!(p[0] < 128, "R should decrease");
        assert_eq!(p[1], 128, "G should be unchanged");
        assert!(p[2] > 128, "B should increase");
    }

    #[test]
    fn positive_tint_lowers_green() {
        let src = grey(128);
        let out = WhiteBalanceOp::new(0.0, 0.5).apply(src.deep_clone()).unwrap();
        let p = &out.data[..4];
        assert_eq!(p[0], 128, "R unchanged");
        assert!(p[1] < 128, "G should decrease (magenta shift)");
        assert_eq!(p[2], 128, "B unchanged");
    }

    #[test]
    fn black_stays_black() {
        // Multiplicative scale must leave pure black untouched.
        let src = grey(0);
        let out = WhiteBalanceOp::new(1.0, 1.0).apply(src.deep_clone()).unwrap();
        out.data.chunks(4).for_each(|p| {
            assert_eq!(p[0], 0);
            assert_eq!(p[1], 0);
            assert_eq!(p[2], 0);
        });
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 128;
            p[1] = 128;
            p[2] = 128;
            p[3] = 99;
        });
        let out = WhiteBalanceOp::new(0.5, 0.3).apply(src.deep_clone()).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 99));
    }
}
