use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Levels adjustment: remaps the tonal range of an image.
///
/// Each channel value is:
/// 1. Normalized into `[black_point, white_point]` → `[0, 1]`
/// 2. Gamma-corrected via `value ^ (1 / midtone)`
/// 3. Scaled back to `0–255`
///
/// `black_point` and `white_point` are in `[0.0, 1.0]` (fraction of 255).
/// `midtone` is a gamma multiplier: `> 1.0` brightens, `< 1.0` darkens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LevelsOp {
    pub black_point: f32,
    pub white_point: f32,
    pub midtone: f32,
}

impl LevelsOp {
    pub fn new(black_point: f32, white_point: f32, midtone: f32) -> Self {
        Self {
            black_point: black_point.clamp(0.0, 1.0),
            white_point: white_point.clamp(0.0, 1.0),
            midtone: midtone.clamp(0.01, 10.0),
        }
    }

    /// Build a lookup table (u8 → u8) for the levels curve.
    fn build_lut(&self) -> [u8; 256] {
        let black = self.black_point;
        let white = self.white_point;
        let gamma = 1.0 / self.midtone.max(0.01);

        // Guard against degenerate range
        let range = (white - black).abs().max(1.0 / 255.0);

        let mut lut = [0u8; 256];
        for (i, entry) in lut.iter_mut().enumerate() {
            let v = i as f32 / 255.0;
            let normalized = ((v - black) / range).clamp(0.0, 1.0);
            let corrected = normalized.powf(gamma);
            *entry = (corrected * 255.0).round().clamp(0.0, 255.0) as u8;
        }
        lut
    }
}

#[typetag::serde]
impl Operation for LevelsOp {
    fn name(&self) -> &'static str {
        "levels"
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        let lut = self.build_lut();

        image.data.par_chunks_mut(4).for_each(|p| {
            p[0] = lut[p[0] as usize];
            p[1] = lut[p[1] as usize];
            p[2] = lut[p[2] as usize];
            // p[3] (alpha) unchanged
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        format!(
            "Levels  black={:.3}  mid={:.2}  white={:.3}",
            self.black_point, self.midtone, self.white_point
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_levels() {
        let mut src = Image::new(4, 4);
        // Fill with a gradient
        for (i, chunk) in src.data.chunks_mut(4).enumerate() {
            let v = (i * 16).min(255) as u8;
            chunk[0] = v;
            chunk[1] = v;
            chunk[2] = v;
            chunk[3] = 255;
        }
        let op = LevelsOp::new(0.0, 1.0, 1.0);
        let src_data = src.data.clone();
        let out = op.apply(src).unwrap();
        // Identity should produce the same values (within rounding)
        for (s, d) in src_data.iter().zip(out.data.iter()) {
            assert!(
                (*s as i32 - *d as i32).abs() <= 1,
                "identity mismatch: {} vs {}",
                s,
                d
            );
        }
    }

    #[test]
    fn black_point_clips_darks() {
        let mut src = Image::new(2, 1);
        src.data[0] = 50;
        src.data[1] = 50;
        src.data[2] = 50;
        src.data[3] = 255;
        src.data[4] = 200;
        src.data[5] = 200;
        src.data[6] = 200;
        src.data[7] = 255;

        // Black point at ~100/255 ≈ 0.392 — pixel value 50 should clip to 0
        let op = LevelsOp::new(100.0 / 255.0, 1.0, 1.0);
        let out = op.apply(src).unwrap();
        assert_eq!(out.data[0], 0, "value below black_point should be 0");
        assert!(out.data[4] > 0, "value above black_point should be > 0");
    }
}
