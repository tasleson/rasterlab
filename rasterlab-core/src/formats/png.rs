use image::{
    ExtendedColorType, ImageEncoder,
    codecs::png::{CompressionType, FilterType, PngEncoder},
};

use crate::{
    error::{RasterError, RasterResult},
    image::Image,
    traits::format_handler::{EncodeOptions, FormatHandler},
};

pub struct PngHandler;

impl FormatHandler for PngHandler {
    fn extensions(&self) -> &[&'static str] {
        &["png"]
    }

    fn display_name(&self) -> &'static str {
        "PNG"
    }

    fn decode(&self, data: &[u8]) -> RasterResult<Image> {
        let dyn_image = image::load_from_memory_with_format(data, image::ImageFormat::Png)
            .map_err(|e| RasterError::decode("png", e.to_string()))?;

        let rgba = dyn_image.to_rgba8();
        let (w, h) = rgba.dimensions();
        Image::from_rgba8(w, h, rgba.into_raw())
    }

    fn encode(&self, image: &Image, options: &EncodeOptions) -> RasterResult<Vec<u8>> {
        let compression = match options.png_compression {
            0..=2 => CompressionType::Fast,
            3..=6 => CompressionType::Default,
            _ => CompressionType::Best,
        };

        let mut buf = Vec::new();
        let encoder = PngEncoder::new_with_quality(&mut buf, compression, FilterType::Adaptive);
        encoder
            .write_image(
                &image.data,
                image.width,
                image.height,
                ExtendedColorType::Rgba8,
            )
            .map_err(|e| RasterError::encode("png", e.to_string()))?;

        Ok(buf)
    }
}
