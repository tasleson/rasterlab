pub mod airplane_window;
mod align;
pub mod blur;
pub mod brightness_contrast;
pub mod bw;
pub mod channel_levels;
pub mod clarity_texture;
pub mod color_balance;
pub mod color_space;
pub mod crop;
pub mod curves;
pub mod denoise;
pub mod faux_hdr;
pub mod flip;
pub mod focus_stack;
mod frames;
pub mod grain;
pub mod hdr_merge;
pub mod heal;
pub mod highlights_shadows;
pub mod histogram;
mod hsl;
pub mod hsl_panel;
pub mod hue_shift;
pub mod levels;
pub mod local_laplacian;
pub mod lut;
pub mod mask;
pub mod noise_reduction;
pub mod panorama;
pub mod perspective;
pub mod resize;
pub mod rotate;
pub mod saturation;
pub mod sepia;
pub mod shadow_exposure;
pub mod sharpen;
pub mod split_tone;
pub mod sprocket_film;
pub mod vibrance;
pub mod vignette;
pub mod white_balance;

use rayon::prelude::*;

pub use airplane_window::AirplaneWindowCorrectionOp;
pub use blur::BlurOp;
pub use brightness_contrast::BrightnessContrastOp;
pub use bw::{BlackAndWhiteOp, BwMode};
pub use channel_levels::{ChannelLevelsOp, ChannelRange};
pub use clarity_texture::ClarityTextureOp;
pub use color_balance::ColorBalanceOp;
pub use color_space::{ColorSpaceConversion, ColorSpaceOp};
pub use crop::CropOp;
pub use curves::CurvesOp;
pub use denoise::DenoiseOp;
pub use faux_hdr::FauxHdrOp;
pub use flip::{FlipMode, FlipOp};
pub use focus_stack::{FocusStackOp, FrameAlignment};
pub use grain::GrainOp;
pub use hdr_merge::HdrMergeOp;
pub use heal::{HealOp, HealSpot};
pub use highlights_shadows::HighlightsShadowsOp;
pub use histogram::{HistogramData, HistogramOp};
pub use hsl_panel::HslPanelOp;
pub use hue_shift::HueShiftOp;
pub use levels::LevelsOp;
pub use local_laplacian::LocalLaplacianOp;
pub use lut::LutOp;
pub use mask::{LinearMask, MaskShape, MaskedOp, RadialMask};
pub use noise_reduction::{NoiseReductionOp, NrMethod};
pub use panorama::PanoramaOp;
pub use perspective::{PerspectiveOp, auto_crop_rect};
pub use resize::{ResampleMode, ResizeOp};
pub use rotate::{RotateMode, RotateOp};
pub use saturation::SaturationOp;
pub use sepia::SepiaOp;
pub use shadow_exposure::ShadowExposureOp;
pub use sharpen::SharpenOp;
pub use split_tone::SplitToneOp;
pub use sprocket_film::{FilmStock, SprocketFilmOp};
pub use vibrance::VibranceOp;
pub use vignette::VignetteOp;
pub use white_balance::WhiteBalanceOp;

// ── Shared pixel utilities ────────────────────────────────────────────────────

/// sRGB gamma → linear (exact IEC 61966-2-1 piecewise formula).
#[inline]
pub(super) fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear → sRGB gamma.
#[inline]
pub(super) fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Apply a per-pixel RGBA mutation using row-level rayon tasks.
///
/// For cheap color transforms, `par_chunks_mut(4)` creates one work item per
/// pixel and can spend more time in scheduling/dispatch than useful math.
/// Chunking by row keeps cache-friendly parallelism while leaving a tight
/// serial inner loop for the compiler to optimize.
pub(super) fn for_each_pixel_row_parallel<F>(image: &mut crate::image::Image, f: F)
where
    F: Fn(&mut [u8]) + Sync + Send,
{
    let row_stride = image.row_stride();
    if row_stride == 0 {
        return;
    }

    image.data.par_chunks_mut(row_stride).for_each(|row| {
        for pixel in row.chunks_exact_mut(4) {
            f(pixel);
        }
    });
}

/// Rec. 709 luminance per pixel, on the same 0–255 scale as the channels it
/// came from.  Shared by the ops that measure structure rather than colour:
/// focus stacking, panorama feature detection, and frame alignment.
pub(super) fn luma_f32(image: &crate::image::Image) -> Vec<f32> {
    image
        .data
        .chunks_exact(4)
        .map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32)
        .collect()
}

/// Bilinear sample from `image` at float coordinates `(sx, sy)`, clamped to border.
#[inline]
pub(super) fn bilinear_sample(image: &crate::image::Image, sx: f32, sy: f32) -> [u8; 4] {
    let w = image.width as usize;
    let h = image.height as usize;
    let x0 = (sx.floor() as isize).clamp(0, w as isize - 1) as usize;
    let y0 = (sy.floor() as isize).clamp(0, h as isize - 1) as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let tx = (sx - sx.floor()).clamp(0.0, 1.0);
    let ty = (sy - sy.floor()).clamp(0.0, 1.0);

    let p00 = &image.data[(y0 * w + x0) * 4..][..4];
    let p10 = &image.data[(y0 * w + x1) * 4..][..4];
    let p01 = &image.data[(y1 * w + x0) * 4..][..4];
    let p11 = &image.data[(y1 * w + x1) * 4..][..4];

    let mut out = [0u8; 4];
    for i in 0..4 {
        let top = p00[i] as f32 + (p10[i] as f32 - p00[i] as f32) * tx;
        let bot = p01[i] as f32 + (p11[i] as f32 - p01[i] as f32) * tx;
        out[i] = (top + (bot - top) * ty).clamp(0.0, 255.0) as u8;
    }
    out
}

#[cfg(test)]
pub(super) mod test_utils {
    use crate::image::Image;

    pub fn solid(r: u8, g: u8, b: u8) -> Image {
        let mut img = Image::new(4, 4);
        img.data.chunks_mut(4).for_each(|p| {
            p[0] = r;
            p[1] = g;
            p[2] = b;
            p[3] = 255;
        });
        img
    }

    pub fn grey(v: u8) -> Image {
        solid(v, v, v)
    }

    /// A scene with structure at every scale, for the ops that measure detail
    /// rather than colour.  Deterministic, so a failure is reproducible, and
    /// non-periodic, so nothing can align or match it to the wrong place.
    pub fn textured_scene(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f32, y as f32);
                let slow = 40.0 + 0.05 * fx + 0.08 * fy;
                let ripple =
                    50.0 * (fx * 0.11).sin() * (fy * 0.07).cos() + 30.0 * ((fx + fy) * 0.031).sin();
                let hash = (x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(40_503)) >> 16;
                let v = (slow + ripple + (hash % 64) as f32).clamp(0.0, 255.0);
                let o = img.pixel_offset(x, y);
                img.data[o] = v as u8;
                img.data[o + 1] = (v * 0.8 + 20.0).clamp(0.0, 255.0) as u8;
                img.data[o + 2] = (255.0 - v).clamp(0.0, 255.0) as u8;
                img.data[o + 3] = 255;
            }
        }
        img
    }

    /// `src` as it would look at `scale`× magnification about the frame centre,
    /// shifted by `(tx, ty)` pixels — a lens breathing and the camera drifting,
    /// in one step.  Bilinear and edge-clamped, written out here rather than
    /// reused from the ops so a test never resamples with the code it is
    /// checking.
    pub fn magnified(src: &Image, scale: f32, tx: f32, ty: f32) -> Image {
        let (w, h) = (src.width, src.height);
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        let mut out = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                // Continuous coordinates, so `scale` is about the centre of the
                // frame rather than the centre of the top-left pixel.
                let sx = cx + (x as f32 + 0.5 - cx - tx) / scale - 0.5;
                let sy = cy + (y as f32 + 0.5 - cy - ty) / scale - 0.5;
                let o = out.pixel_offset(x, y);
                out.data[o..o + 4].copy_from_slice(&super::bilinear_sample(src, sx, sy));
            }
        }
        out
    }

    /// One frame's left half beside another's right half — a focus bracket's
    /// defining property, that each frame is sharp where the others are not.
    pub fn split_halves(left: &Image, right: &Image) -> Image {
        assert_eq!((left.width, left.height), (right.width, right.height));
        let mut out = left.deep_clone();
        for y in 0..left.height {
            for x in left.width / 2..left.width {
                let o = left.pixel_offset(x, y);
                out.data[o..o + 4].copy_from_slice(&right.data[o..o + 4]);
            }
        }
        out
    }

    /// Square box blur over the whole frame, standing in for defocus.
    pub fn box_blurred(src: &Image, radius: u32) -> Image {
        let (w, h) = (src.width as i64, src.height as i64);
        let r = radius as i64;
        let mut out = Image::new(src.width, src.height);
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0f32; 3];
                let mut n = 0.0f32;
                for dy in -r..=r {
                    for dx in -r..=r {
                        let o = src.pixel_offset(
                            (x + dx).clamp(0, w - 1) as u32,
                            (y + dy).clamp(0, h - 1) as u32,
                        );
                        for (a, c) in acc.iter_mut().zip(&src.data[o..o + 3]) {
                            *a += *c as f32;
                        }
                        n += 1.0;
                    }
                }
                let o = out.pixel_offset(x as u32, y as u32);
                for (c, a) in out.data[o..o + 3].iter_mut().zip(acc) {
                    *c = (a / n) as u8;
                }
                out.data[o + 3] = 255;
            }
        }
        out
    }

    /// Mean absolute RGB difference over the interior, ignoring `inset` pixels
    /// of border where resampling and window-based measures run out of data.
    pub fn mean_abs_diff(a: &Image, b: &Image, inset: u32) -> f32 {
        assert_eq!((a.width, a.height), (b.width, b.height));
        let mut sum = 0.0f64;
        let mut n = 0u64;
        for y in inset..a.height - inset {
            for x in inset..a.width - inset {
                let o = a.pixel_offset(x, y);
                for c in 0..3 {
                    sum += (a.data[o + c] as i32 - b.data[o + c] as i32).unsigned_abs() as f64;
                    n += 1;
                }
            }
        }
        (sum / n as f64) as f32
    }
}
