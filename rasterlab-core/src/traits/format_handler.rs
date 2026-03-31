use crate::{error::RasterResult, image::Image};
use std::path::Path;

/// Options controlling output encoding quality.
#[derive(Debug, Clone)]
pub struct EncodeOptions {
    /// JPEG quality 1–100 (default 90).
    pub jpeg_quality: u8,
    /// PNG compression level 0–9 (default 6).
    pub png_compression: u8,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            jpeg_quality: 90,
            png_compression: 6,
        }
    }
}

/// Codec for a specific image file format.
///
/// Implementations are registered in [`FormatRegistry`][crate::formats::FormatRegistry]
/// and selected automatically based on file extension and magic bytes.
///
/// # Adding a new format
///
/// 1. Create a struct implementing `FormatHandler`.
/// 2. Register it with [`FormatRegistry::register`][crate::formats::FormatRegistry::register].
///
/// The registry drives both the CLI `--input`/`--output` flags and the GUI open/save dialogs.
pub trait FormatHandler: Send + Sync {
    /// File extensions handled by this codec (lower-case, no dot).
    fn extensions(&self) -> &[&'static str];

    /// Decode raw file bytes into an [`Image`].
    ///
    /// The default implementation is called when the format requires only the
    /// raw bytes (JPEG, PNG, …).  Formats that need the original file path
    /// (e.g. for adjacent sidecar files) should override [`decode_file`].
    fn decode(&self, data: &[u8]) -> RasterResult<Image>;

    /// Decode from a file path.
    ///
    /// The default implementation reads the file and delegates to [`decode`].
    /// Override this for formats that benefit from direct file access (e.g. RAW).
    fn decode_file(&self, path: &Path) -> RasterResult<Image> {
        let data = std::fs::read(path).map_err(crate::error::RasterError::Io)?;
        self.decode(&data)
    }

    /// Encode an [`Image`] to bytes in this format.
    fn encode(&self, image: &Image, options: &EncodeOptions) -> RasterResult<Vec<u8>>;

    /// Whether this handler can produce output files.
    ///
    /// RAW format handlers (NEF, CR2, …) should return `false`; the system
    /// will redirect exports to a different format.
    fn can_encode(&self) -> bool {
        true
    }

    /// Whether this handler requires a filesystem path rather than raw bytes.
    ///
    /// RAW formats (NEF, CR2, …) that use libraries requiring seekable file
    /// access should return `true`.  The registry will call [`decode_file`]
    /// instead of [`decode`] for these handlers.
    fn needs_file_path(&self) -> bool {
        false
    }

    /// Human-readable display name (e.g. "JPEG", "Nikon RAW").
    fn display_name(&self) -> &'static str;
}
