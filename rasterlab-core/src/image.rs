use crate::error::{RasterError, RasterResult};
use std::path::PathBuf;

/// Internal pixel representation.  Always `Rgba8` after decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PixelFormat {
    /// 4 bytes per pixel: R, G, B, A (u8 each, straight alpha, row-major).
    Rgba8,
}

/// EXIF-derived metadata kept alongside the pixel buffer.
#[derive(Debug, Clone, Default)]
pub struct ImageMetadata {
    /// Original file path (if loaded from disk).
    pub original_path: Option<PathBuf>,
    /// Camera make/model string from EXIF, if present.
    pub camera_model: Option<String>,
    /// ISO sensitivity from EXIF.
    pub iso: Option<u32>,
    /// Shutter speed as a human-readable fraction string (e.g. "1/250").
    pub shutter_speed: Option<String>,
    /// Aperture f-number.
    pub aperture: Option<f32>,
    /// ICC profile bytes, if embedded.
    pub icc_profile: Option<Vec<u8>>,
}

/// The central image type used throughout the engine.
///
/// Internally stored as a flat RGBA8 buffer (`width * height * 4` bytes).
/// Pixel at column `x`, row `y` starts at byte `(y * width + x) * 4`.
///
/// Intentionally does NOT derive `Clone` to prevent accidental large copies.
/// Use [`Image::deep_clone`] for explicit duplication.
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Raw pixel bytes (RGBA, row-major).
    pub data: Vec<u8>,
    pub metadata: ImageMetadata,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("format", &self.format)
            .field("data_len", &self.data.len())
            .finish()
    }
}

impl Image {
    /// Create a blank (transparent black) RGBA8 image.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            format: PixelFormat::Rgba8,
            data: vec![0u8; (width * height * 4) as usize],
            metadata: ImageMetadata::default(),
        }
    }

    /// Create from a pre-existing RGBA8 buffer.
    ///
    /// Returns `Err` if `data.len() != width * height * 4`.
    pub fn from_rgba8(width: u32, height: u32, data: Vec<u8>) -> RasterResult<Self> {
        let expected = (width as usize) * (height as usize) * 4;
        if data.len() != expected {
            return Err(RasterError::InvalidParams(format!(
                "Buffer length {} does not match expected {} ({}×{}×4)",
                data.len(),
                expected,
                width,
                height
            )));
        }
        Ok(Self {
            width,
            height,
            format: PixelFormat::Rgba8,
            data,
            metadata: ImageMetadata {
                ..Default::default()
            },
        })
    }

    /// Number of bytes per row.
    #[inline]
    pub fn row_stride(&self) -> usize {
        self.width as usize * 4
    }

    /// Byte offset of pixel `(x, y)`.
    #[inline]
    pub fn pixel_offset(&self, x: u32, y: u32) -> usize {
        (y as usize * self.width as usize + x as usize) * 4
    }

    /// Read pixel at `(x, y)` as `[R, G, B, A]`.
    #[inline]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let o = self.pixel_offset(x, y);
        [
            self.data[o],
            self.data[o + 1],
            self.data[o + 2],
            self.data[o + 3],
        ]
    }

    /// Write pixel at `(x, y)`.
    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, rgba: [u8; 4]) {
        let o = self.pixel_offset(x, y);
        self.data[o..o + 4].copy_from_slice(&rgba);
    }

    /// Sample with bilinear interpolation.  Out-of-bounds coordinates are clamped.
    pub fn sample_bilinear(&self, x: f32, y: f32) -> [u8; 4] {
        let x = x.clamp(0.0, self.width as f32 - 1.0);
        let y = y.clamp(0.0, self.height as f32 - 1.0);

        let x0 = x.floor() as u32;
        let y0 = y.floor() as u32;
        let x1 = (x0 + 1).min(self.width - 1);
        let y1 = (y0 + 1).min(self.height - 1);

        let fx = x - x.floor();
        let fy = y - y.floor();

        let p00 = self.pixel(x0, y0);
        let p10 = self.pixel(x1, y0);
        let p01 = self.pixel(x0, y1);
        let p11 = self.pixel(x1, y1);

        let mut out = [0u8; 4];
        for c in 0..4 {
            let v = p00[c] as f32 * (1.0 - fx) * (1.0 - fy)
                + p10[c] as f32 * fx * (1.0 - fy)
                + p01[c] as f32 * (1.0 - fx) * fy
                + p11[c] as f32 * fx * fy;
            out[c] = v.round().clamp(0.0, 255.0) as u8;
        }
        out
    }

    /// Explicit deep copy.  Prefer this over any implicit Clone.
    pub fn deep_clone(&self) -> Self {
        Self {
            width: self.width,
            height: self.height,
            format: self.format.clone(),
            data: self.data.clone(),
            metadata: self.metadata.clone(),
        }
    }

    /// Total number of pixels.
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }
}
