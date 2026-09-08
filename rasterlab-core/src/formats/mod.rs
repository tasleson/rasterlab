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

        if handler.supports_shared_bytes() {
            return handler.decode_shared_bytes(Arc::new(data.to_vec()), hint_path);
        }
        if handler.needs_file_path() {
            return self.decode_bytes_via_tempfile(&handler, data, hint_path);
        }

        handler.decode(data)
    }

    /// Decode an already-owned source buffer without cloning it for a codec
    /// that supports shared in-memory input. Other codecs retain the existing
    /// borrowed-byte or path-only fallback behavior.
    pub fn decode_shared_bytes(
        &self,
        data: Arc<Vec<u8>>,
        hint_path: Option<&Path>,
    ) -> RasterResult<Image> {
        let fmt = detect_format(&data, hint_path).ok_or_else(|| {
            RasterError::UnsupportedFormat("Cannot determine image format from bytes".into())
        })?;
        let handler = self.handler_for_extension(&fmt).ok_or_else(|| {
            RasterError::UnsupportedFormat(format!("No handler registered for '{}'", fmt))
        })?;
        if handler.supports_shared_bytes() {
            return handler.decode_shared_bytes(data, hint_path);
        }
        if handler.needs_file_path() {
            return self.decode_bytes_via_tempfile(&handler, &data, hint_path);
        }
        handler.decode(&data)
    }

    /// Decode an already-owned source buffer for a library import.
    ///
    /// This preserves ordinary decode behavior, but gives codecs an explicit
    /// import-only path for avoiding editor/export-only metadata allocations.
    pub fn decode_import_shared_bytes(
        &self,
        data: Arc<Vec<u8>>,
        hint_path: Option<&Path>,
    ) -> RasterResult<Image> {
        let fmt = detect_format(&data, hint_path).ok_or_else(|| {
            RasterError::UnsupportedFormat("Cannot determine image format from bytes".into())
        })?;
        let handler = self.handler_for_extension(&fmt).ok_or_else(|| {
            RasterError::UnsupportedFormat(format!("No handler registered for '{}'", fmt))
        })?;
        if handler.supports_shared_bytes() {
            return handler.decode_import_shared_bytes(data, hint_path);
        }
        if handler.needs_file_path() {
            return self.decode_bytes_via_tempfile(&handler, &data, hint_path);
        }
        handler.decode(&data)
    }

    /// Decode an image file, transparently unwrapping a `.rlab` container.
    ///
    /// Managed-library photos exist only as `.rlab` files whose `ORIG` chunk
    /// holds the verbatim original, so the multi-frame ops can take a library
    /// photo as a source frame without the caller first exporting it. Edits
    /// recorded in the project are ignored: the frame is the unedited original,
    /// which is what those ops fuse.
    pub fn decode_source_file(&self, path: &Path) -> RasterResult<Image> {
        if !crate::project::is_rlab_path(path) {
            return self.decode_file(path);
        }
        let rlab = crate::project::RlabFile::read(path)?;
        let hint = rlab.meta.source_path.as_deref().map(Path::new);
        self.decode_bytes(&rlab.original_bytes, hint)
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

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct SharedBytesHandler {
        received_ptr: Mutex<Option<usize>>,
        import_decode: Mutex<bool>,
    }

    impl FormatHandler for SharedBytesHandler {
        fn extensions(&self) -> &[&'static str] {
            &["shared"]
        }

        fn decode(&self, _data: &[u8]) -> RasterResult<Image> {
            unreachable!("shared bytes must use the ownership-preserving path")
        }

        fn decode_shared_bytes(
            &self,
            data: Arc<Vec<u8>>,
            hint_path: Option<&Path>,
        ) -> RasterResult<Image> {
            assert_eq!(hint_path, Some(Path::new("source.shared")));
            *self.received_ptr.lock().unwrap() = Some(Arc::as_ptr(&data) as usize);
            Image::from_rgba8(1, 1, vec![1, 2, 3, 255])
        }

        fn supports_shared_bytes(&self) -> bool {
            true
        }

        fn decode_import_shared_bytes(
            &self,
            data: Arc<Vec<u8>>,
            hint_path: Option<&Path>,
        ) -> RasterResult<Image> {
            *self.import_decode.lock().unwrap() = true;
            self.decode_shared_bytes(data, hint_path)
        }

        fn encode(&self, _image: &Image, _options: &EncodeOptions) -> RasterResult<Vec<u8>> {
            Err(RasterError::FormatNotEncodable("shared test".into()))
        }

        fn display_name(&self) -> &'static str {
            "shared test"
        }
    }

    #[test]
    fn shared_decode_preserves_the_callers_buffer_allocation() {
        let registry = FormatRegistry::default();
        let handler = Arc::new(SharedBytesHandler {
            received_ptr: Mutex::new(None),
            import_decode: Mutex::new(false),
        });
        registry.register(handler.clone());
        let bytes = Arc::new(b"already-owned source bytes".to_vec());
        let expected_ptr = Arc::as_ptr(&bytes) as usize;

        let image = registry
            .decode_shared_bytes(bytes, Some(Path::new("source.shared")))
            .unwrap();

        assert_eq!(image.data, vec![1, 2, 3, 255]);
        assert_eq!(*handler.received_ptr.lock().unwrap(), Some(expected_ptr));
        assert!(!*handler.import_decode.lock().unwrap());
    }

    #[test]
    fn import_decode_uses_the_import_specific_handler_path() {
        let registry = FormatRegistry::default();
        let handler = Arc::new(SharedBytesHandler {
            received_ptr: Mutex::new(None),
            import_decode: Mutex::new(false),
        });
        registry.register(handler.clone());
        let bytes = Arc::new(b"already-owned source bytes".to_vec());
        let expected_ptr = Arc::as_ptr(&bytes) as usize;

        let image = registry
            .decode_import_shared_bytes(bytes, Some(Path::new("source.shared")))
            .unwrap();

        assert_eq!(image.data, vec![1, 2, 3, 255]);
        assert_eq!(*handler.received_ptr.lock().unwrap(), Some(expected_ptr));
        assert!(*handler.import_decode.lock().unwrap());
    }
}
