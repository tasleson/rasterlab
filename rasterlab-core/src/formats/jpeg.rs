use image::{ExtendedColorType, codecs::jpeg::JpegEncoder};

use crate::{
    error::{RasterError, RasterResult},
    image::Image,
    traits::format_handler::{EncodeOptions, FormatHandler},
};

pub struct JpegHandler;

impl FormatHandler for JpegHandler {
    fn extensions(&self) -> &[&'static str] {
        &["jpg", "jpeg"]
    }

    fn display_name(&self) -> &'static str {
        "JPEG"
    }

    fn decode(&self, data: &[u8]) -> RasterResult<Image> {
        let dyn_image = image::load_from_memory_with_format(data, image::ImageFormat::Jpeg)
            .map_err(|e| RasterError::decode("jpeg", e.to_string()))?;

        let rgba = dyn_image.to_rgba8();
        let (w, h) = rgba.dimensions();
        Image::from_rgba8(w, h, rgba.into_raw())
    }

    fn encode(&self, image: &Image, options: &EncodeOptions) -> RasterResult<Vec<u8>> {
        // JPEG does not support alpha — strip to RGB
        let rgb: Vec<u8> = image
            .data
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();

        let mut buf = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut buf, options.jpeg_quality);
        encoder
            .encode(&rgb, image.width, image.height, ExtendedColorType::Rgb8)
            .map_err(|e| RasterError::encode("jpeg", e.to_string()))?;

        Ok(buf)
    }
}
