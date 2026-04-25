pub mod blur;
pub mod brightness_contrast;
pub mod bw;
pub mod clarity_texture;
pub mod color_balance;
pub mod color_space;
pub mod crop;
pub mod curves;
pub mod denoise;
pub mod faux_hdr;
pub mod flip;
pub mod focus_stack;
pub mod grain;
pub mod hdr_merge;
pub mod heal;
pub mod highlights_shadows;
pub mod histogram;
mod hsl;
pub mod hsl_panel;
pub mod hue_shift;
pub mod levels;
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
pub mod vibrance;
pub mod vignette;
pub mod white_balance;

pub use blur::BlurOp;
pub use brightness_contrast::BrightnessContrastOp;
pub use bw::{BlackAndWhiteOp, BwMode};
pub use clarity_texture::ClarityTextureOp;
pub use color_balance::ColorBalanceOp;
pub use color_space::{ColorSpaceConversion, ColorSpaceOp};
pub use crop::CropOp;
pub use curves::CurvesOp;
pub use denoise::DenoiseOp;
pub use faux_hdr::FauxHdrOp;
pub use flip::FlipOp;
pub use focus_stack::FocusStackOp;
pub use grain::GrainOp;
pub use hdr_merge::HdrMergeOp;
pub use heal::{HealOp, HealSpot};
pub use highlights_shadows::HighlightsShadowsOp;
pub use histogram::{HistogramData, HistogramOp};
pub use hsl_panel::HslPanelOp;
pub use hue_shift::HueShiftOp;
pub use levels::LevelsOp;
pub use lut::LutOp;
pub use mask::{LinearMask, MaskShape, MaskedOp, RadialMask};
pub use noise_reduction::{NoiseReductionOp, NrMethod};
pub use panorama::PanoramaOp;
pub use perspective::PerspectiveOp;
pub use resize::{ResampleMode, ResizeOp};
pub use rotate::{RotateMode, RotateOp};
pub use saturation::SaturationOp;
pub use sepia::SepiaOp;
pub use shadow_exposure::ShadowExposureOp;
pub use sharpen::SharpenOp;
pub use split_tone::SplitToneOp;
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
