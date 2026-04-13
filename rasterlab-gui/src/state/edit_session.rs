//! Edit-an-existing-op support.  When the user clicks the pencil button on an
//! edit-stack row, we remember which op is being edited and which tool panel
//! section is bound to it.  While a session is active, Apply buttons in that
//! section call `replace_op` instead of `push_op`, and other tools / stack
//! rows are disabled so the user can only adjust the one op under edit.

use rasterlab_core::ops::{
    BlackAndWhiteOp, BlurOp, BrightnessContrastOp, ClarityTextureOp, ColorBalanceOp, CurvesOp,
    DenoiseOp, FauxHdrOp, GrainOp, HighlightsShadowsOp, HslPanelOp, HueShiftOp, LevelsOp,
    NoiseReductionOp, SaturationOp, SepiaOp, ShadowExposureOp, SharpenOp, SplitToneOp, VibranceOp,
    VignetteOp, WhiteBalanceOp,
};
use rasterlab_core::traits::operation::Operation;

use super::ToolState;

/// Which tool panel section is bound to the current edit session.  Also acts
/// as the classifier that decides whether a given op type is editable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditingTool {
    Levels,
    BlackAndWhite,
    BrightnessContrast,
    Saturation,
    Sepia,
    Sharpen,
    ClarityTexture,
    SplitTone,
    Curves,
    Vignette,
    Vibrance,
    HueShift,
    HighlightsShadows,
    ShadowExposure,
    WhiteBalance,
    FauxHdr,
    Grain,
    ColorBalance,
    HslPanel,
    Blur,
    Denoise,
    NoiseReduction,
}

/// Bookkeeping for an active edit session.
#[derive(Debug, Clone, Copy)]
pub struct EditSession {
    pub op_index: usize,
    pub tool: EditingTool,
}

/// Inspect `op` and, if it is one of the editable types, copy its parameters
/// into `tools` and return the matching tool kind.  Returns `None` when the op
/// is not a type we support editing for (geometric ops, file-based ops, etc.).
pub fn load_op_into_tools(op: &dyn Operation, tools: &mut ToolState) -> Option<EditingTool> {
    let any = op.as_any()?;

    if let Some(o) = any.downcast_ref::<LevelsOp>() {
        tools.levels_black = o.black_point;
        tools.levels_white = o.white_point;
        tools.levels_mid = o.midtone;
        return Some(EditingTool::Levels);
    }
    if let Some(o) = any.downcast_ref::<BlackAndWhiteOp>() {
        use rasterlab_core::ops::BwMode;
        match &o.mode {
            BwMode::Luminance => tools.bw_mode_idx = 0,
            BwMode::Average => tools.bw_mode_idx = 1,
            BwMode::Perceptual => tools.bw_mode_idx = 2,
            BwMode::ChannelMixer { r, g, b } => {
                tools.bw_mode_idx = 3;
                tools.bw_mixer_r = *r;
                tools.bw_mixer_g = *g;
                tools.bw_mixer_b = *b;
            }
        }
        return Some(EditingTool::BlackAndWhite);
    }
    if let Some(o) = any.downcast_ref::<BrightnessContrastOp>() {
        tools.bc_brightness = o.brightness;
        tools.bc_contrast = o.contrast;
        return Some(EditingTool::BrightnessContrast);
    }
    if let Some(o) = any.downcast_ref::<SaturationOp>() {
        tools.saturation = o.saturation;
        return Some(EditingTool::Saturation);
    }
    if let Some(o) = any.downcast_ref::<SepiaOp>() {
        tools.sepia_strength = o.strength;
        return Some(EditingTool::Sepia);
    }
    if let Some(o) = any.downcast_ref::<SharpenOp>() {
        tools.sharpen_strength = o.strength;
        return Some(EditingTool::Sharpen);
    }
    if let Some(o) = any.downcast_ref::<ClarityTextureOp>() {
        tools.clarity = o.clarity;
        tools.texture = o.texture;
        return Some(EditingTool::ClarityTexture);
    }
    if let Some(o) = any.downcast_ref::<SplitToneOp>() {
        tools.split_shadow_hue = o.shadow_hue;
        tools.split_shadow_sat = o.shadow_sat;
        tools.split_highlight_hue = o.highlight_hue;
        tools.split_highlight_sat = o.highlight_sat;
        tools.split_balance = o.balance;
        return Some(EditingTool::SplitTone);
    }
    if let Some(o) = any.downcast_ref::<CurvesOp>() {
        tools.curve_points = o.points.clone();
        return Some(EditingTool::Curves);
    }
    if let Some(o) = any.downcast_ref::<VignetteOp>() {
        tools.vignette_strength = o.strength;
        tools.vignette_radius = o.radius;
        tools.vignette_feather = o.feather;
        return Some(EditingTool::Vignette);
    }
    if let Some(o) = any.downcast_ref::<VibranceOp>() {
        tools.vibrance = o.strength;
        return Some(EditingTool::Vibrance);
    }
    if let Some(o) = any.downcast_ref::<HueShiftOp>() {
        tools.hue_degrees = o.degrees;
        return Some(EditingTool::HueShift);
    }
    if let Some(o) = any.downcast_ref::<HighlightsShadowsOp>() {
        tools.hl_highlights = o.highlights;
        tools.hl_shadows = o.shadows;
        return Some(EditingTool::HighlightsShadows);
    }
    if let Some(o) = any.downcast_ref::<ShadowExposureOp>() {
        tools.shadow_ev = o.ev;
        tools.shadow_falloff = o.falloff;
        return Some(EditingTool::ShadowExposure);
    }
    if let Some(o) = any.downcast_ref::<WhiteBalanceOp>() {
        tools.wb_temperature = o.temperature;
        tools.wb_tint = o.tint;
        return Some(EditingTool::WhiteBalance);
    }
    if let Some(o) = any.downcast_ref::<FauxHdrOp>() {
        tools.hdr_strength = o.strength;
        return Some(EditingTool::FauxHdr);
    }
    if let Some(o) = any.downcast_ref::<GrainOp>() {
        tools.grain_strength = o.strength;
        tools.grain_size = o.size;
        tools.grain_seed = o.seed;
        return Some(EditingTool::Grain);
    }
    if let Some(o) = any.downcast_ref::<ColorBalanceOp>() {
        tools.cb_cyan_red = o.cyan_red;
        tools.cb_magenta_green = o.magenta_green;
        tools.cb_yellow_blue = o.yellow_blue;
        return Some(EditingTool::ColorBalance);
    }
    if let Some(o) = any.downcast_ref::<HslPanelOp>() {
        tools.hsl_hue = o.hue;
        tools.hsl_sat = o.saturation;
        tools.hsl_lum = o.luminance;
        return Some(EditingTool::HslPanel);
    }
    if let Some(o) = any.downcast_ref::<BlurOp>() {
        tools.blur_radius = o.radius;
        return Some(EditingTool::Blur);
    }
    if let Some(o) = any.downcast_ref::<DenoiseOp>() {
        tools.denoise_strength = o.strength;
        tools.denoise_radius = o.radius;
        return Some(EditingTool::Denoise);
    }
    if let Some(o) = any.downcast_ref::<NoiseReductionOp>() {
        tools.nr_method = o.method.clone();
        tools.nr_luma = o.luma_strength;
        tools.nr_color = o.color_strength;
        tools.nr_detail = o.detail_preservation;
        return Some(EditingTool::NoiseReduction);
    }
    None
}
