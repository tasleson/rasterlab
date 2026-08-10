use std::path::Path;

use anyhow::{Context, Result};
use image as img_crate;
use rasterlab_core::{image::Image, verified_write::write_verified_atomic};

/// Encode `image` as a JPEG thumbnail at most `max_side` pixels wide.
/// Returns JPEG bytes.
pub fn generate_thumbnail(image: &Image, max_side: u32) -> Result<Vec<u8>> {
    let src = img_crate::RgbaImage::from_raw(image.width, image.height, image.data.clone())
        .ok_or_else(|| anyhow::anyhow!("image buffer size mismatch"))?;

    // Compute scale so the longer side fits within max_side
    let scale = if image.width >= image.height {
        max_side as f32 / image.width as f32
    } else {
        max_side as f32 / image.height as f32
    }
    .min(1.0); // never upscale

    let nw = ((image.width as f32 * scale).round() as u32).max(1);
    let nh = ((image.height as f32 * scale).round() as u32).max(1);

    let resized =
        img_crate::imageops::resize(&src, nw, nh, img_crate::imageops::FilterType::Triangle);

    let rgb: img_crate::RgbImage = img_crate::DynamicImage::ImageRgba8(resized).into_rgb8();

    let mut buf: Vec<u8> = Vec::new();
    let mut encoder = img_crate::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 85);
    encoder.encode_image(&rgb)?;
    Ok(buf)
}

/// Write a thumbnail to `path`, creating its directory, replacing whatever is
/// there in one step.
///
/// Staged and renamed like every other file the library writes: a thumbnail
/// truncated by a crash mid-write would survive as a torn image that nothing
/// repairs — the rebuild pass only regenerates thumbnails that are *missing*,
/// having no way to tell a damaged JPEG from an ugly one.
pub fn write_thumbnail(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    write_verified_atomic(path, bytes).with_context(|| format!("write {}", path.display()))
}
