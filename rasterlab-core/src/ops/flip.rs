use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{error::RasterResult, image::Image, traits::operation::Operation};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FlipMode {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlipOp {
    pub mode: FlipMode,
}

impl FlipOp {
    pub fn horizontal() -> Self {
        Self {
            mode: FlipMode::Horizontal,
        }
    }
    pub fn vertical() -> Self {
        Self {
            mode: FlipMode::Vertical,
        }
    }
}

#[typetag::serde]
impl Operation for FlipOp {
    fn name(&self) -> &'static str {
        "flip"
    }

    fn apply(&self, mut image: Image) -> RasterResult<Image> {
        let w = image.width as usize;
        let h = image.height as usize;
        let row_bytes = w * 4;

        match self.mode {
            FlipMode::Horizontal => {
                // Reverse each row in-place: swap pixel pairs from both ends.
                image.data.par_chunks_mut(row_bytes).for_each(|row| {
                    let mut lo = 0usize;
                    let mut hi = w - 1;
                    while lo < hi {
                        for c in 0..4 {
                            row.swap(lo * 4 + c, hi * 4 + c);
                        }
                        lo += 1;
                        hi -= 1;
                    }
                });
            }
            FlipMode::Vertical => {
                // Swap rows from both ends toward the centre.
                for i in 0..h / 2 {
                    let j = h - 1 - i;
                    let (top_data, bot_data) = image.data.split_at_mut(j * row_bytes);
                    top_data[i * row_bytes..(i + 1) * row_bytes]
                        .swap_with_slice(&mut bot_data[..row_bytes]);
                }
            }
        }

        Ok(image)
    }

    fn describe(&self) -> String {
        match self.mode {
            FlipMode::Horizontal => "Flip Horizontal".into(),
            FlipMode::Vertical => "Flip Vertical".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient_image() -> Image {
        let mut img = Image::new(4, 4);
        for y in 0..4u32 {
            for x in 0..4u32 {
                img.set_pixel(x, y, [x as u8 * 60, y as u8 * 60, 0, 255]);
            }
        }
        img
    }

    #[test]
    fn horizontal_mirrors_x() {
        let src = gradient_image();
        let p_0_0 = src.pixel(0, 0);
        let p_3_0 = src.pixel(3, 0);
        let out = FlipOp::horizontal().apply(src).unwrap();
        // pixel at (0,0) should equal original (3,0)
        assert_eq!(out.pixel(0, 0), p_3_0);
        assert_eq!(out.pixel(3, 0), p_0_0);
    }

    #[test]
    fn vertical_mirrors_y() {
        let src = gradient_image();
        let p_0_0 = src.pixel(0, 0);
        let p_0_3 = src.pixel(0, 3);
        let out = FlipOp::vertical().apply(src).unwrap();
        assert_eq!(out.pixel(0, 0), p_0_3);
        assert_eq!(out.pixel(0, 3), p_0_0);
    }
}
