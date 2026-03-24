pub mod jpeg;
pub mod nef;
pub mod png;

pub use jpeg::JpegHandler;
pub use nef::NefHandler;
pub use png::PngHandler;

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
/// Returns a lower-case format identifier string (e.g. `"jpeg"`, `"png"`, `"nef"`).
pub fn detect_format(data: &[u8], hint_path: Option<&Path>) -> Option<String> {
    // Magic-byte checks (fast, no allocation)
    if data.len() >= 3 && &data[..3] == b"\xff\xd8\xff" {
        return Some("jpeg".into());
    }
    if data.len() >= 8 && &data[..8] == b"\x89PNG\r\n\x1a\n" {
        return Some("png".into());
    }
    // NEF / TIFF share the TIFF magic bytes (II or MM byte-order markers)
    if data.len() >= 4 && ((&data[..4] == b"II\x2a\x00") || (&data[..4] == b"MM\x00\x2a")) {
        // Distinguish NEF from generic TIFF via file extension
        if let Some(ext) = hint_path
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            && ext.eq_ignore_ascii_case("nef") {
                return Some("nef".into());
            }
        return Some("tiff".into());
    }

    // Extension fallback — normalise to lower-case
    hint_path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| match e.to_lowercase().as_str() {
            "jpg" | "jpeg" => "jpeg".into(),
            "png" => "png".into(),
            "nef" => "nef".into(),
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
    /// Create a registry pre-loaded with the built-in handlers (JPEG, PNG, NEF).
    pub fn with_builtins() -> Self {
        let reg = Self::default();
        reg.register(Arc::new(JpegHandler));
        reg.register(Arc::new(PngHandler));
        reg.register(Arc::new(NefHandler));
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

        // For RAW formats that override decode_file, delegate directly
        if fmt == "nef" {
            return handler.decode_file(path);
        }

        handler.decode(&data)
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

    /// Return all registered extensions.
    pub fn supported_extensions(&self) -> Vec<String> {
        let map = self.handlers.read().expect("FormatRegistry lock poisoned");
        map.keys().cloned().collect()
    }
}
