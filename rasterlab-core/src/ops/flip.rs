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

    fn apply(&self, image: &Image) -> RasterResult<Image> {
        let w = image.width as usize;
        let h = image.height as usize;
        let row_bytes = w * 4;
        let mut out = Image::new(image.width, image.height);
        out.metadata = image.metadata.clone();

        match self.mode {
            FlipMode::Horizontal => {
                // Parallel: reverse each row independently.
                out.data
                    .par_chunks_mut(row_bytes)
                    .zip(image.data.par_chunks(row_bytes))
                    .for_each(|(dst, src)| {
                        for x in 0..w {
                            let src_off = (w - 1 - x) * 4;
                            let dst_off = x * 4;
                            dst[dst_off..dst_off + 4].copy_from_slice(&src[src_off..src_off + 4]);
                        }
                    });
            }
            FlipMode::Vertical => {
                // Copy rows from bottom to top.
                for y in 0..h {
                    let src_start = (h - 1 - y) * row_bytes;
                    let dst_start = y * row_bytes;
                    out.data[dst_start..dst_start + row_bytes]
                        .copy_from_slice(&image.data[src_start..src_start + row_bytes]);
                }
            }
        }

        Ok(out)
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
        let out = FlipOp::horizontal().apply(&src).unwrap();
        // pixel at (0,0) should equal original (3,0)
        assert_eq!(out.pixel(0, 0), src.pixel(3, 0));
        assert_eq!(out.pixel(3, 0), src.pixel(0, 0));
    }

    #[test]
    fn vertical_mirrors_y() {
        let src = gradient_image();
        let out = FlipOp::vertical().apply(&src).unwrap();
        assert_eq!(out.pixel(0, 0), src.pixel(0, 3));
        assert_eq!(out.pixel(0, 3), src.pixel(0, 0));
    }
}
