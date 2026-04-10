//! Broad camera RAW format handler (rawler-backed).
//!
//! A single handler covers all camera manufacturers supported by rawler 0.7:
//! Nikon (NEF/NRW), Canon (CR2/CR3), Sony (ARW/SR2/SRF), Fujifilm (RAF),
//! Panasonic (RW2/RAW), Olympus/OM (ORF), Pentax/Ricoh (PEF), Samsung (SRW),
//! Adobe DNG, and others (3FR, ERF, IIQ).
//!
//! `rawler::analyze::raw_to_srgb` is format-agnostic — it auto-detects the
//! camera format from the file and runs a full RAW processing pipeline
//! (demosaicing, white balance, colour-space conversion to sRGB).
//!
//! The handler requires the `raw` feature (enabled by default).

use std::path::Path;

use crate::{
    error::{RasterError, RasterResult},
    image::Image,
    traits::format_handler::{EncodeOptions, FormatHandler},
};

/// All camera RAW extensions handled by rawler.
pub const RAW_EXTENSIONS: &[&str] = &[
    // Nikon
    "nef", "nrw", // Canon
    "cr2", "cr3", // Sony
    "arw", "sr2", "srf", // Fujifilm
    "raf", // Panasonic / Leica (Panasonic-based)
    "rw2", // Olympus / OM System
    "orf", // Pentax / Ricoh
    "pef", // Samsung
    "srw", // Adobe / generic DNG (used by Leica, Hasselblad, many phones)
    "dng", // Hasselblad
    "3fr", // Phase One
    "iiq", // Epson
    "erf", // Panasonic also uses .raw for some models
    "raw",
];

pub struct RawHandler;

impl FormatHandler for RawHandler {
    fn extensions(&self) -> &[&'static str] {
        RAW_EXTENSIONS
    }

    fn display_name(&self) -> &'static str {
        "Camera RAW"
    }

    fn needs_file_path(&self) -> bool {
        true
    }

    fn decode(&self, _data: &[u8]) -> RasterResult<Image> {
        // rawler requires a seekable file path; delegate to decode_file.
        Err(RasterError::UnsupportedFormat(
            "RAW decoding requires a file path — use decode_file".into(),
        ))
    }

    fn decode_file(&self, path: &Path) -> RasterResult<Image> {
        #[cfg(feature = "raw")]
        {
            decode_raw_rawler(path)
        }
        #[cfg(not(feature = "raw"))]
        {
            let _ = path;
            Err(RasterError::UnsupportedFormat(
                "RAW support is disabled — rebuild with --features raw".into(),
            ))
        }
    }

    fn encode(&self, _image: &Image, _options: &EncodeOptions) -> RasterResult<Vec<u8>> {
        Err(RasterError::FormatNotEncodable("Camera RAW".into()))
    }

    fn can_encode(&self) -> bool {
        false
    }
}

// ---------------------------------------------------------------------------
// rawler-based decoder (feature = "raw")
// ---------------------------------------------------------------------------

#[cfg(feature = "raw")]
fn decode_raw_rawler(path: &Path) -> RasterResult<Image> {
    use rawler::{analyze::raw_to_srgb, decoders::RawDecodeParams};

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("raw")
        .to_lowercase();

    let params = RawDecodeParams::default();
    let dyn_image =
        raw_to_srgb(path, &params).map_err(|e| RasterError::decode(&ext, format!("{:?}", e)))?;

    let rgba = dyn_image.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut image = Image::from_rgba8(w, h, rgba.into_raw())?;

    // Populate EXIF metadata from the source file.
    image.metadata = crate::formats::exif_util::read_exif_from_file(path);
    image.metadata.original_path = Some(path.to_path_buf());

    Ok(image)
}
