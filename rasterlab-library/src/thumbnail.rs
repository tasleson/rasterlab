use anyhow::Result;
use image as img_crate;
use rasterlab_core::image::Image;

/// Encode `image` as a JPEG thumbnail at most `max_side` pixels wide.
/// Returns JPEG bytes.
pub fn generate_thumbnail(image: &Image, max_side: u32) -> Result<Vec<u8>> {
    let src = img_crate::RgbaImage::from_raw(
        image.width,
        image.height,
        image.data.clone(),
    )
    .ok_or_else(|| anyhow::anyhow!("image buffer size mismatch"))?;

    // Compute scale so the longer side fits within max_side
    let scale = if image.width >= image.height {
        max_side as f32 / image.width as f32
    } else {
        max_side as f32 / image.height as f32
    }
    .min(1.0); // never upscale

    let nw = ((image.width  as f32 * scale).round() as u32).max(1);
    let nh = ((image.height as f32 * scale).round() as u32).max(1);

    let resized = img_crate::imageops::resize(
        &src,
        nw,
        nh,
        img_crate::imageops::FilterType::Triangle,
    );

    let rgb: img_crate::RgbImage = img_crate::DynamicImage::ImageRgba8(resized).into_rgb8();

    let mut buf: Vec<u8> = Vec::new();
    let mut encoder = img_crate::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 85);
    encoder.encode_image(&rgb)?;
    Ok(buf)
}
