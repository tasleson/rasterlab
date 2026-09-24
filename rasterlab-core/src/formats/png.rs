use image::{
    ExtendedColorType, GenericImageView, ImageEncoder,
    codecs::png::{CompressionType, FilterType, PngEncoder},
};

use crate::{
    error::{RasterError, RasterResult},
    formats::exif_util,
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

        let (w, h) = dyn_image.dimensions();
        // A straight 8-bit RGBA PNG moves the decoder's buffer without
        // copying; a 24-bit PNG expands to opaque RGBA in one parallel pass.
        // (The old `to_rgba8()` cloned every pixel in both cases.)
        let buf = match dyn_image {
            image::DynamicImage::ImageRgba8(rgba) => rgba.into_raw(),
            image::DynamicImage::ImageRgb8(rgb) => exif_util::rgb8_to_rgba8(&rgb),
            // Paletted, 8/16-bit and LA variants: the crate's converter.
            other => other.to_rgba8().into_raw(),
        };
        Image::from_rgba8(w, h, buf)
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
