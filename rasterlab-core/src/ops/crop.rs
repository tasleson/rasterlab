use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::{
    error::{RasterError, RasterResult},
    image::Image,
    traits::operation::Operation,
};

/// Crop a rectangular region from the image.
///
/// Coordinates are in source-image pixels (not fractions) to avoid
/// floating-point drift on serialize/deserialize cycles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CropOp {
    /// Left edge of the crop region (inclusive).
    pub x: u32,
    /// Top edge of the crop region (inclusive).
    pub y: u32,
    /// Width of the output image.
    pub width: u32,
    /// Height of the output image.
    pub height: u32,
}

impl CropOp {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }
}

#[typetag::serde]
impl Operation for CropOp {
    fn name(&self) -> &'static str { "crop" }

    fn apply(&self, image: &Image) -> RasterResult<Image> {
        // Validate bounds
        if self.width == 0 || self.height == 0 {
            return Err(RasterError::InvalidParams(
                "Crop width and height must be non-zero".into(),
            ));
        }
        let right  = self.x.checked_add(self.width).ok_or_else(|| {
            RasterError::InvalidParams("Crop x + width overflows u32".into())
        })?;
        let bottom = self.y.checked_add(self.height).ok_or_else(|| {
            RasterError::InvalidParams("Crop y + height overflows u32".into())
        })?;
        if right > image.width || bottom > image.height {
            return Err(RasterError::InvalidParams(format!(
                "Crop region ({}, {}, {}×{}) exceeds image bounds {}×{}",
                self.x, self.y, self.width, self.height, image.width, image.height
            )));
        }

        let mut output = Image::new(self.width, self.height);
        output.metadata = image.metadata.clone();

        let src_stride = image.row_stride();
        let dst_stride = output.row_stride();
        let x_offset   = self.x as usize * 4;

        // Each output row is an independent slice copy — trivially parallel.
        output
            .data
            .par_chunks_mut(dst_stride)
            .enumerate()
            .for_each(|(dst_y, dst_row)| {
                let src_y     = self.y as usize + dst_y;
                let src_start = src_y * src_stride + x_offset;
                dst_row.copy_from_slice(&image.data[src_start..src_start + dst_stride]);
            });

        Ok(output)
    }

    fn describe(&self) -> String {
        format!("Crop  {}×{}  at ({}, {})", self.width, self.height, self.x, self.y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkerboard(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                img.set_pixel(x, y, [v, v, v, 255]);
            }
        }
        img
    }

    #[test]
    fn crop_basic() {
        let src = checkerboard(10, 10);
        let op  = CropOp::new(2, 3, 4, 5);
        let out = op.apply(&src).unwrap();
        assert_eq!(out.width,  4);
        assert_eq!(out.height, 5);
        // Top-left of crop should match src pixel at (2,3)
        assert_eq!(out.pixel(0, 0), src.pixel(2, 3));
    }

    #[test]
    fn crop_out_of_bounds() {
        let src = checkerboard(8, 8);
        let op  = CropOp::new(5, 5, 10, 10);
        assert!(op.apply(&src).is_err());
    }
}
