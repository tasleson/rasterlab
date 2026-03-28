use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

/// Algorithm used to compute the luminance value for each pixel.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "algorithm")]
pub enum BwMode {
    /// BT.709 luma coefficients — matches sRGB display white point.
    /// `L = 0.2126·R + 0.7152·G + 0.0722·B`
    Luminance,

    /// Simple arithmetic mean.
    /// `L = (R + G + B) / 3`
    Average,

    /// BT.601 perceptual weights — historically used for NTSC/PAL.
    /// `L = 0.299·R + 0.587·G + 0.114·B`
    Perceptual,

    /// User-defined channel mix.  Weights need not sum to 1 (they are applied as-is
    /// and the result is clamped to `[0, 255]`).
    ChannelMixer { r: f32, g: f32, b: f32 },
}

impl BwMode {
    #[inline]
    fn to_gray(&self, r: f32, g: f32, b: f32) -> f32 {
        match self {
            BwMode::Luminance => 0.2126 * r + 0.7152 * g + 0.0722 * b,
            BwMode::Average => (r + g + b) / 3.0,
            BwMode::Perceptual => 0.299 * r + 0.587 * g + 0.114 * b,
            BwMode::ChannelMixer {
                r: rw,
                g: gw,
                b: bw,
            } => rw * r + gw * g + bw * b,
        }
    }
}

/// Convert the image to black and white using a selectable luminance algorithm.
///
/// Alpha channel is preserved.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlackAndWhiteOp {
    pub mode: BwMode,
}

impl BlackAndWhiteOp {
    pub fn luminance() -> Self {
        Self {
            mode: BwMode::Luminance,
        }
    }
    pub fn average() -> Self {
        Self {
            mode: BwMode::Average,
        }
    }
    pub fn perceptual() -> Self {
        Self {
            mode: BwMode::Perceptual,
        }
    }
    pub fn channel_mixer(r: f32, g: f32, b: f32) -> Self {
        Self {
            mode: BwMode::ChannelMixer { r, g, b },
        }
    }
}

#[typetag::serde]
impl Operation for BlackAndWhiteOp {
    fn name(&self) -> &'static str {
        "black_and_white"
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        image.data.par_chunks_mut(4).for_each(|pixel| {
            let gray = self
                .mode
                .to_gray(pixel[0] as f32, pixel[1] as f32, pixel[2] as f32)
                .clamp(0.0, 255.0) as u8;
            pixel[0] = gray;
            pixel[1] = gray;
            pixel[2] = gray;
            // pixel[3] (alpha) untouched
        });

        Ok(image)
    }

    fn describe(&self) -> String {
        let alg = match &self.mode {
            BwMode::Luminance => "BT.709 Luminance",
            BwMode::Average => "Average",
            BwMode::Perceptual => "BT.601 Perceptual",
            BwMode::ChannelMixer { .. } => "Channel Mixer",
        };
        format!("B&W  ({})", alg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pixel_image(r: u8, g: u8, b: u8) -> Image {
        let mut img = Image::new(2, 2);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = r;
            p[1] = g;
            p[2] = b;
            p[3] = 255;
        });
        img
    }

    #[test]
    fn pure_red_to_luma() {
        let src = make_pixel_image(255, 0, 0);
        let out = BlackAndWhiteOp::luminance().apply(src).unwrap();
        let [r, g, b, _] = out.pixel(0, 0);
        // 0.2126 * 255 ≈ 54
        assert_eq!(r, 54);
        assert_eq!(g, 54);
        assert_eq!(b, 54);
    }

    #[test]
    fn pure_white_all_modes() {
        let src = make_pixel_image(255, 255, 255);
        for op in [
            BlackAndWhiteOp::luminance(),
            BlackAndWhiteOp::average(),
            BlackAndWhiteOp::perceptual(),
        ] {
            let out = op.apply(src.deep_clone()).unwrap();
            assert_eq!(out.pixel(0, 0), [255, 255, 255, 255]);
        }
    }

    #[test]
    fn alpha_preserved() {
        let mut src = Image::new(1, 1);
        src.set_pixel(0, 0, [100, 150, 200, 128]);
        let out = BlackAndWhiteOp::luminance().apply(src).unwrap();
        assert_eq!(out.pixel(0, 0)[3], 128);
    }
}
