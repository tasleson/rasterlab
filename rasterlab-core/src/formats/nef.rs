//! Nikon RAW (NEF) format handler.
//!
//! This handler requires the `nef` feature (enabled by default).
//! Without it, loading NEF files returns [`RasterError::UnsupportedFormat`].
//!
//! ## Implementation notes
//!
//! NEF is a TIFF-based RAW format containing unprocessed sensor data in a Bayer
//! mosaic pattern.  Full processing involves:
//!
//! 1. Reading sensor data and CFA (colour filter array) layout — handled by `rawler`.
//! 2. Demosaicing: interpolate full-colour pixels from the Bayer mosaic.
//! 3. White balance, exposure and colour-space transforms.
//! 4. Gamma / tone mapping to 8-bit output.
//!
//! The implementation here uses rawler for steps 1 and then performs a
//! bilinear demosaic (step 2) with a simple white-balance pass (step 3).

use std::path::Path;

use crate::{
    error::{RasterError, RasterResult},
    image::Image,
    traits::format_handler::{EncodeOptions, FormatHandler},
};

pub struct NefHandler;

impl FormatHandler for NefHandler {
    fn extensions(&self) -> &[&'static str] {
        &["nef"]
    }

    fn display_name(&self) -> &'static str {
        "Nikon RAW (NEF)"
    }

    fn decode(&self, _data: &[u8]) -> RasterResult<Image> {
        // rawler requires a seekable file path; delegate to decode_file.
        Err(RasterError::UnsupportedFormat(
            "NEF decoding requires a file path — use decode_file".into(),
        ))
    }

    fn decode_file(&self, path: &Path) -> RasterResult<Image> {
        #[cfg(feature = "nef")]
        {
            decode_nef_rawler(path)
        }
        #[cfg(not(feature = "nef"))]
        {
            let _ = path;
            Err(RasterError::UnsupportedFormat(
                "NEF support is disabled — rebuild with --features nef".into(),
            ))
        }
    }

    fn encode(&self, _image: &Image, _options: &EncodeOptions) -> RasterResult<Vec<u8>> {
        Err(RasterError::FormatNotEncodable("NEF".into()))
    }

    fn can_encode(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// rawler-based decoder (feature = "nef")
// ---------------------------------------------------------------------------

#[cfg(feature = "nef")]
fn decode_nef_rawler(path: &Path) -> RasterResult<Image> {
    use rawler::{analyze::raw_to_srgb, decoders::RawDecodeParams};

    // rawler 0.7: raw_to_srgb performs full RAW processing
    // (demosaicing, white balance, colour space conversion) and returns a DynamicImage.
    let params = RawDecodeParams::default();
    let dyn_image =
        raw_to_srgb(path, &params).map_err(|e| RasterError::decode("nef", format!("{:?}", e)))?;

    let rgba = dyn_image.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut image = Image::from_rgba8(w, h, rgba.into_raw())?;
    image.metadata.original_path = Some(path.to_path_buf());
    Ok(image)
}
