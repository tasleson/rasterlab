//! # rasterlab-core
//!
//! The image processing engine.  All image editing logic lives here; the
//! CLI and GUI are thin shells on top.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use rasterlab_core::{
//!     formats::FormatRegistry,
//!     ops::{BlackAndWhiteOp, CropOp},
//!     pipeline::EditPipeline,
//!     traits::format_handler::EncodeOptions,
//! };
//! use std::path::Path;
//!
//! // Load an image
//! let registry = FormatRegistry::with_builtins();
//! let image    = registry.decode_file(Path::new("photo.jpg")).unwrap();
//!
//! // Build a non-destructive pipeline
//! let mut pipeline = EditPipeline::new(image);
//! pipeline.push_op(Box::new(CropOp::new(100, 100, 800, 600)));
//! pipeline.push_op(Box::new(BlackAndWhiteOp::luminance()));
//!
//! // Render and export
//! let rendered = pipeline.render().unwrap();
//! let bytes    = registry
//!     .encode_file(&rendered, Path::new("output.png"), &EncodeOptions::default())
//!     .unwrap();
//! std::fs::write("output.png", bytes).unwrap();
//! ```

pub mod analysis;
pub mod cancel;
pub mod degraded_read;
pub mod error;
pub mod formats;
pub mod image;
#[cfg(feature = "import-timing")]
pub mod import_timing;
pub mod library_meta;
pub mod ops;
pub mod panic_guard;
pub mod pipeline;
pub mod plugin_loader;
pub mod project;
pub mod render_cache;
pub mod traits;
pub mod verified_write;

// Convenience re-exports
pub use error::{RasterError, RasterResult};
pub use image::Image;
pub use pipeline::EditPipeline;

/// Measure an expression only in builds with the `import-timing` feature.
/// Nested phases report exclusive calling-thread wall time; no output is emitted.
#[cfg(feature = "import-timing")]
#[macro_export]
macro_rules! import_phase {
    ($name:literal, $body:expr) => {{
        let _phase = $crate::import_timing::Span::new($name);
        $body
    }};
}

/// With timing disabled, evaluate the original expression without instrumentation.
#[cfg(not(feature = "import-timing"))]
#[macro_export]
macro_rules! import_phase {
    ($name:literal, $body:expr) => {{ $body }};
}
