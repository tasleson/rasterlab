use image::{ExtendedColorType, GenericImageView, codecs::jpeg::JpegEncoder};

use crate::{
    error::{RasterError, RasterResult},
    formats::exif_util,
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

        let (w, h) = dyn_image.dimensions();
        let mut metadata = exif_util::read_exif_from_bytes(data);

        // A standard JPEG decodes to RGB8, so the opaque-RGBA expansion and
        // the EXIF orientation transform are fused into one buffer pass
        // instead of the old two-pass RGB→RGBA→rotate.
        let (data, w, h) = match dyn_image {
            image::DynamicImage::ImageRgb8(rgb) => {
                exif_util::apply_orientation_rgb(rgb.into_raw(), w, h, metadata.orientation)
            }
            image::DynamicImage::ImageRgba8(rgba) => {
                exif_util::apply_orientation(rgba.into_raw(), w, h, metadata.orientation)
            }
            // Grayscale and other odd variants: the crate's converter.
            other => {
                let rgba = other.to_rgba8();
                let (w, h) = rgba.dimensions();
                exif_util::apply_orientation(rgba.into_raw(), w, h, metadata.orientation)
            }
        };

        // Apply EXIF orientation to pixels so downstream consumers always
        // see an upright image, then normalise the stored value (and the
        // raw_exif bytes used for metadata-preserving export) so a
        // re-exported file is not rotated twice by downstream viewers.
        if metadata.orientation != 1 {
            if let Some(ref mut bytes) = metadata.raw_exif {
                exif_util::normalize_tiff_orientation(bytes);
            }
            metadata.orientation = 1;
        }

        let mut image = Image::from_rgba8(w, h, data)?;
        image.metadata = metadata;
        Ok(image)
    }

    fn encode(&self, image: &Image, options: &EncodeOptions) -> RasterResult<Vec<u8>> {
        // JPEG does not support alpha — strip to RGB.
        let rgb: Vec<u8> = image
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();

        let mut buf = Vec::new();
        let mut encoder = JpegEncoder::new_with_quality(&mut buf, options.jpeg_quality);
        encoder
            .encode(&rgb, image.width, image.height, ExtendedColorType::Rgb8)
            .map_err(|e| RasterError::encode("jpeg", e.to_string()))?;

        // Re-attach original EXIF if requested and available.
        if options.preserve_metadata
            && let Some(ref exif_bytes) = image.metadata.raw_exif
        {
            buf = exif_util::attach_exif_to_jpeg(buf, exif_bytes);
        }

        Ok(buf)
    }
}
