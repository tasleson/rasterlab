pub mod exif_util;
pub mod jpeg;
pub mod png;
pub mod raw;

pub use jpeg::JpegHandler;
pub use png::PngHandler;
pub use raw::RawHandler;

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, RwLock},
};

use crate::{
    error::{RasterError, RasterResult},
    image::Image,
    traits::format_handler::{EncodeOptions, FormatHandler},
};

// ---------------------------------------------------------------------------
// Format detection
// ---------------------------------------------------------------------------

/// Identify the format of `data` from its magic bytes, falling back to the
/// extension of `hint_path` if magic bytes are inconclusive.
///
/// Returns a lower-case extension string that matches a registered handler
/// (e.g. `"jpeg"`, `"png"`, `"nef"`, `"arw"`, `"cr2"`).
pub fn detect_format(data: &[u8], hint_path: Option<&Path>) -> Option<String> {
    // Normalised file extension — used both for disambiguation and fallback.
    let ext_lc: Option<String> = hint_path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase());

    // Magic-byte checks (fast, no allocation).
    if data.len() >= 3 && &data[..3] == b"\xff\xd8\xff" {
        return Some("jpeg".into());
    }
    if data.len() >= 8 && &data[..8] == b"\x89PNG\r\n\x1a\n" {
        return Some("png".into());
    }
    // Fujifilm RAF has its own magic.
    if data.len() >= 16 && &data[..16] == b"FUJIFILMCCD-RAW " {
        return Some("raf".into());
    }
    // TIFF magic (little-endian II or big-endian MM) — used by NEF, CR2, ARW,
    // ORF, RW2, PEF, DNG, SRW, 3FR, IIQ, ERF, and generic TIFF.
    if data.len() >= 4 && ((&data[..4] == b"II\x2a\x00") || (&data[..4] == b"MM\x00\x2a")) {
        // Route to the correct RAW handler via extension; unknown extensions
        // fall through to "tiff" (no registered handler — will return an error
        // that's more informative than a silent failure).
        if let Some(ref ext) = ext_lc
            && raw::RAW_EXTENSIONS.contains(&ext.as_str())
        {
            return Some(ext.clone());
        }
        return Some("tiff".into());
    }
    // ISO Base Media File Format magic (CR3, HEIF, MP4…).  CR3 uses "ftyp"
    // at byte 4; route to the raw handler so rawler can try to decode it.
    if data.len() >= 8
        && &data[4..8] == b"ftyp"
        && let Some(ref ext) = ext_lc
        && raw::RAW_EXTENSIONS.contains(&ext.as_str())
    {
        return Some(ext.clone());
    }

    // Extension-only fallback for formats without distinctive magic bytes.
    ext_lc.map(|e| match e.as_str() {
        "jpg" | "jpeg" => "jpeg".into(),
        "png" => "png".into(),
        other => other.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

/// Thread-safe registry of all available format handlers.
///
/// Pre-populated with the built-in handlers; plugins may register additional ones.
#[derive(Default)]
pub struct FormatRegistry {
    /// Map from lower-case extension to handler.
    handlers: RwLock<HashMap<String, Arc<dyn FormatHandler>>>,
}

impl FormatRegistry {
    /// Create a registry pre-loaded with the built-in handlers (JPEG, PNG, Camera RAW).
    pub fn with_builtins() -> Self {
        let reg = Self::default();
        reg.register(Arc::new(JpegHandler));
        reg.register(Arc::new(PngHandler));
        reg.register(Arc::new(RawHandler));
        reg
    }

    /// Register a handler for all extensions it claims to handle.
    pub fn register(&self, handler: Arc<dyn FormatHandler>) {
        let mut map = self.handlers.write().expect("FormatRegistry lock poisoned");
        for ext in handler.extensions() {
            map.insert(ext.to_string(), Arc::clone(&handler));
        }
    }

    /// Look up a handler by extension (case-insensitive).
    pub fn handler_for_extension(&self, ext: &str) -> Option<Arc<dyn FormatHandler>> {
        let map = self.handlers.read().expect("FormatRegistry lock poisoned");
        map.get(&ext.to_lowercase()).cloned()
    }

    /// Decode a file from disk, auto-detecting the format.
    pub fn decode_file(&self, path: &Path) -> RasterResult<Image> {
        let data = std::fs::read(path).map_err(RasterError::Io)?;
        let fmt = detect_format(&data, Some(path)).ok_or_else(|| {
            RasterError::UnsupportedFormat(format!(
                "Cannot determine format for '{}'",
                path.display()
            ))
        })?;

        let handler = self.handler_for_extension(&fmt).ok_or_else(|| {
            RasterError::UnsupportedFormat(format!("No handler registered for '{}'", fmt))
        })?;

        // Formats that need seekable file access (RAW) bypass the in-memory path.
        if handler.needs_file_path() {
            return handler.decode_file(path);
        }

        handler.decode(&data)
    }

    /// Decode an image from raw bytes, using `hint_path` for format detection.
    ///
    /// Equivalent to [`decode_file`](Self::decode_file) but the caller provides
    /// the bytes directly — useful when the source data is already in memory
    /// (e.g. the `ORIG` chunk of a `.rlab` project file).
    ///
    /// For formats that require a seekable file path (currently NEF/rawler), the
    /// bytes are written to a temporary file and decoded from there.
    pub fn decode_bytes(&self, data: &[u8], hint_path: Option<&Path>) -> RasterResult<Image> {
        let fmt = detect_format(data, hint_path).ok_or_else(|| {
            RasterError::UnsupportedFormat("Cannot determine image format from bytes".into())
        })?;

        let handler = self.handler_for_extension(&fmt).ok_or_else(|| {
            RasterError::UnsupportedFormat(format!("No handler registered for '{}'", fmt))
        })?;

        if handler.needs_file_path() {
            return self.decode_bytes_via_tempfile(&handler, data, hint_path);
        }

        handler.decode(data)
    }

    /// Encode an image to the format implied by `path`'s extension.
    pub fn encode_file(
        &self,
        image: &Image,
        path: &Path,
        options: &EncodeOptions,
    ) -> RasterResult<Vec<u8>> {
        let ext = path.extension().and_then(|e| e.to_str()).ok_or_else(|| {
            RasterError::UnsupportedFormat("Output path has no file extension".into())
        })?;

        let handler = self.handler_for_extension(ext).ok_or_else(|| {
            RasterError::UnsupportedFormat(format!("No handler for extension '{}'", ext))
        })?;

        if !handler.can_encode() {
            return Err(RasterError::FormatNotEncodable(
                handler.display_name().into(),
            ));
        }

        handler.encode(image, options)
    }

    /// Write bytes to a unique temp file and decode via `decode_file`.
    ///
    /// Used for handlers that require a seekable file path (RAW formats).
    /// The temp file is automatically deleted when the `NamedTempFile` drops.
    fn decode_bytes_via_tempfile(
        &self,
        handler: &Arc<dyn FormatHandler>,
        data: &[u8],
        hint_path: Option<&Path>,
    ) -> RasterResult<Image> {
        use std::io::Write;
        let ext = hint_path
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("raw");
        let suffix = format!(".{}", ext);
        let mut tmp = tempfile::Builder::new()
            .suffix(&suffix)
            .tempfile()
            .map_err(RasterError::Io)?;
        tmp.write_all(data).map_err(RasterError::Io)?;
        handler.decode_file(tmp.path())
    }

    /// Return all registered extensions.
    pub fn supported_extensions(&self) -> Vec<String> {
        let map = self.handlers.read().expect("FormatRegistry lock poisoned");
        map.keys().cloned().collect()
    }
}
