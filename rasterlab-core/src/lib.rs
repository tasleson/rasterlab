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

pub mod error;
pub mod formats;
pub mod image;
pub mod ops;
pub mod pipeline;
pub mod plugin_loader;
pub mod traits;

// Convenience re-exports
pub use error::{RasterError, RasterResult};
pub use image::Image;
pub use pipeline::EditPipeline;
