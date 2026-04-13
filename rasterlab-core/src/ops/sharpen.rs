use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    error::{RasterError, RasterResult},
    image::Image,
    traits::operation::Operation,
};

/// Convolution-based sharpening with a configurable kernel strength.
///
/// Uses an unsharp-mask style approach:
/// ```text
/// output = clamp(source + strength * (source - gaussian_blur(source)))
/// ```
/// but for simplicity and speed the "blur" is approximated with a box-average of
/// the 8-neighbours, giving a single-pass 3×3 convolution kernel.
///
/// The effective kernel at `strength = 1.0` is:
/// ```text
/// [ 0  -1   0 ]
/// [-1   5  -1 ]
/// [ 0  -1   0 ]
/// ```
/// Higher `strength` values increase the centre weight linearly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharpenOp {
    /// Sharpening intensity.  `0.0` = no change, `1.0` = standard, `>1.0` = stronger.
    /// Clamped to `[0.0, 10.0]`.
    pub strength: f32,
    /// When `true`, sharpening is applied only to luminance (avoids colour fringing).
    pub luminance_only: bool,
}

impl SharpenOp {
    pub fn new(strength: f32) -> Self {
        Self {
            strength: strength.clamp(0.0, 10.0),
            luminance_only: false,
        }
    }

    pub fn luminance(strength: f32) -> Self {
        Self {
            strength: strength.clamp(0.0, 10.0),
            luminance_only: true,
        }
    }
}

#[typetag::serde]
impl Operation for SharpenOp {
    fn name(&self) -> &'static str {
        "sharpen"
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(self.clone())
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn apply(&self, image: Image) -> RasterResult<Image> {
        if self.strength <= 0.0 {
            return Ok(image);
        }
        if self.strength > 10.0 {
            return Err(RasterError::InvalidParams(
                "Sharpen strength must be ≤ 10.0".into(),
            ));
        }
        apply_sharpen(&image, self.strength, self.luminance_only)
    }

    fn describe(&self) -> String {
        if self.luminance_only {
            format!("Sharpen  {:.2}  (luminance)", self.strength)
        } else {
            format!("Sharpen  {:.2}", self.strength)
        }
    }
}

fn apply_sharpen(src: &Image, strength: f32, luma_only: bool) -> RasterResult<Image> {
    let w = src.width as usize;
    let h = src.height as usize;

    // Kernel:  centre = 1 + 4*strength / (1 + 4*strength) normalised so that flat
    // areas are unchanged.  We keep it un-normalised: centre weight = 1 + 4*s,
    // neighbour weight = -s, which preserves DC exactly.
    //
    //  0    -s     0
    // -s  1+4s    -s
    //  0    -s     0
    let s = strength;

    let mut out = Image::new(src.width, src.height);
    out.metadata = src.metadata.clone();

    out.data
        .par_chunks_mut(w * 4)
        .enumerate()
        .for_each(|(y, row)| {
            let yn = y.saturating_sub(1);
            let yp = (y + 1).min(h - 1);

            for x in 0..w {
                let xn = x.saturating_sub(1);
                let xp = (x + 1).min(w - 1);

                // Fetch the 5 required pixels
                let centre = fetch(src, x, y);
                let top = fetch(src, x, yn);
                let bottom = fetch(src, x, yp);
                let left = fetch(src, xn, y);
                let right = fetch(src, xp, y);

                let dst = x * 4;
                if luma_only {
                    // Convert centre to luma, sharpen, re-composite
                    let luma_c = luma(centre);
                    let luma_t = luma(top);
                    let luma_b = luma(bottom);
                    let luma_l = luma(left);
                    let luma_r = luma(right);

                    let sharpened_luma = ((1.0 + 4.0 * s) * luma_c
                        - s * (luma_t + luma_b + luma_l + luma_r))
                        .clamp(0.0, 255.0);

                    let luma_delta = sharpened_luma - luma_c;
                    for c in 0..3 {
                        let v = centre[c] as f32 + luma_delta;
                        row[dst + c] = v.clamp(0.0, 255.0) as u8;
                    }
                    row[dst + 3] = centre[3]; // alpha unchanged
                } else {
                    for c in 0..3 {
                        let v = (1.0 + 4.0 * s) * centre[c] as f32
                            - s * (top[c] as f32
                                + bottom[c] as f32
                                + left[c] as f32
                                + right[c] as f32);
                        row[dst + c] = v.clamp(0.0, 255.0) as u8;
                    }
                    row[dst + 3] = centre[3];
                }
            }
        });

    Ok(out)
}

#[inline(always)]
fn fetch(img: &Image, x: usize, y: usize) -> [u8; 4] {
    let off = (y * img.width as usize + x) * 4;
    [
        img.data[off],
        img.data[off + 1],
        img.data[off + 2],
        img.data[off + 3],
    ]
}

#[inline(always)]
fn luma(p: [u8; 4]) -> f32 {
    0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sharpen_zero_is_identity() {
        let src = Image::new(4, 4);
        let src_data = src.data.clone();
        let out = SharpenOp::new(0.0).apply(src).unwrap();
        assert_eq!(out.data, src_data);
    }

    #[test]
    fn sharpen_solid_unchanged() {
        // Sharpening a flat image should produce no change
        let mut src = Image::new(8, 8);
        src.data.chunks_mut(4).for_each(|p| {
            p[0] = 128;
            p[1] = 100;
            p[2] = 80;
            p[3] = 255;
        });
        let expected = src.pixel(4, 4);
        let out = SharpenOp::new(1.0).apply(src).unwrap();
        // Centre pixels (away from borders) should be unchanged; border pixels may differ slightly
        assert_eq!(out.pixel(4, 4), expected);
    }
}
