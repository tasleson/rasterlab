//! GPU kernels for RasterLab operations.
//!
//! This crate intentionally stays below the GUI/rendering layer. It owns no
//! windows or egui textures; callers provide a `wgpu::Device` and `wgpu::Queue`.

mod common;
mod context;
mod error;
mod image;
mod kernels;
mod ops;
mod pipeline;
mod shaders;

#[cfg(test)]
mod tests;

pub use context::GpuContext;
pub use error::GpuError;
pub use image::GpuImage;
pub use ops::{apply_one, apply_one_to_image, supports};
pub use pipeline::{GpuPipeline, GpuTimings};
