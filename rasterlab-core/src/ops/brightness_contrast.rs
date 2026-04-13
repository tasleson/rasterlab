use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Adjust brightness and contrast using the standard Photoshop-style formulas.
///
/// Brightness is applied first as a simple additive offset, then contrast
/// is applied as a centred scale around the midpoint (128).
///
/// * `brightness` — additive offset in `[-1.0, 1.0]` (maps to `[-255, +255]`).
/// * `contrast`   — scale factor in `[-1.0, 1.0]`.
///   `0.0` = no change, `1.0` ≈ strong contrast boost, `-1.0` = flat grey.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrightnessContrastOp {
    pub brightness: f32,
    pub contrast: f32,
}

impl BrightnessContrastOp {
    pub fn new(brightness: f32, contrast: f32) -> Self {
        Self {
            brightness: brightness.clamp(-1.0, 1.0),
            contrast: contrast.clamp(-1.0, 1.0),
        }
    }
}

/// Build a lookup table so the per-pixel work is just a single array read.
fn build_lut(brightness: f32, contrast: f32) -> [u8; 256] {
    let b = brightness * 255.0;
    // Photoshop-style contrast factor: maps [-1,1] to a multiplicative scale.
    let c = contrast * 255.0;
    let cf = 259.0 * (c + 255.0) / (255.0 * (259.0 - c));

    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        let x = i as f32 + b; // brightness
        let x = cf * (x - 128.0) + 128.0; // contrast
        *v = x.clamp(0.0, 255.0) as u8;
    }
    lut
}

#[typetag::serde]
impl Operation for BrightnessContrastOp {
    fn name(&self) -> &'static str {
        "brightness_contrast"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        let lut = build_lut(self.brightness, self.contrast);

        image.data.par_chunks_mut(4).for_each(|p| {
            p[0] = lut[p[0] as usize];
            p[1] = lut[p[1] as usize];
            p[2] = lut[p[2] as usize];
            // alpha untouched
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        format!(
            "Brightness/Contrast  {:+.0}% / {:+.0}%",
            self.brightness * 100.0,
            self.contrast * 100.0,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_unchanged() {
        let mut src = Image::new(4, 4);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 100;
            p[1] = 150;
            p[2] = 200;
            p[3] = 255;
        });
        let out = BrightnessContrastOp::new(0.0, 0.0)
            .apply(src.deep_clone())
            .unwrap();
        assert_eq!(out.data, src.data);
    }

    #[test]
    fn brightness_boost_lightens() {
        let mut src = Image::new(2, 2);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 100;
            p[1] = 100;
            p[2] = 100;
            p[3] = 255;
        });
        let out = BrightnessContrastOp::new(0.2, 0.0).apply(src).unwrap();
        assert!(out.pixel(0, 0)[0] > 100);
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(2, 2);
        src.data.chunks_mut(4).for_each(|p| {
            p[3] = 77;
        });
        let out = BrightnessContrastOp::new(0.5, 0.5).apply(src).unwrap();
        out.data.chunks(4).for_each(|p| assert_eq!(p[3], 77));
    }
}
