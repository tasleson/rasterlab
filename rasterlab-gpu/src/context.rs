use std::sync::Arc;

use crate::kernels::*;

#[derive(Clone)]
pub struct GpuContext {
    pub(crate) device: Arc<wgpu::Device>,
    pub(crate) queue: Arc<wgpu::Queue>,
    pub(crate) limits: wgpu::Limits,
    pub(crate) brightness_contrast: Arc<BrightnessContrastKernel>,
    pub(crate) curves: Arc<CurvesKernel>,
    pub(crate) hue_shift: Arc<HueShiftKernel>,
    pub(crate) saturation: Arc<SaturationKernel>,
    pub(crate) vibrance: Arc<VibranceKernel>,
    pub(crate) white_balance: Arc<WhiteBalanceKernel>,
    pub(crate) noise_reduction_nlm: Arc<NoiseReductionNlmKernel>,
    pub(crate) sepia: Arc<SepiaKernel>,
    pub(crate) levels: Arc<LevelsKernel>,
    pub(crate) highlights_shadows: Arc<HighlightsShadowsKernel>,
    pub(crate) vignette: Arc<VignetteKernel>,
    pub(crate) shadow_exposure: Arc<ShadowExposureKernel>,
    pub(crate) split_tone: Arc<SplitToneKernel>,
    pub(crate) black_and_white: Arc<BlackAndWhiteKernel>,
    pub(crate) blur: Arc<BlurKernel>,
    pub(crate) color_balance: Arc<ColorBalanceKernel>,
    pub(crate) color_space: Arc<ColorSpaceKernel>,
    pub(crate) denoise: Arc<DenoiseKernel>,
    pub(crate) hsl_panel: Arc<HslPanelKernel>,
    pub(crate) sharpen: Arc<SharpenKernel>,
    pub(crate) faux_hdr: Arc<FauxHdrKernel>,
    pub(crate) clarity_texture: Arc<ClarityTextureKernel>,
}

impl GpuContext {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, limits: wgpu::Limits) -> Self {
        let brightness_contrast = Arc::new(BrightnessContrastKernel::new(&device));
        let curves = Arc::new(CurvesKernel::new(&device));
        let hue_shift = Arc::new(HueShiftKernel::new(&device));
        let saturation = Arc::new(SaturationKernel::new(&device));
        let vibrance = Arc::new(VibranceKernel::new(&device));
        let white_balance = Arc::new(WhiteBalanceKernel::new(&device));
        let noise_reduction_nlm = Arc::new(NoiseReductionNlmKernel::new(&device));
        let sepia = Arc::new(SepiaKernel::new(&device));
        let levels = Arc::new(LevelsKernel::new(&device));
        let highlights_shadows = Arc::new(HighlightsShadowsKernel::new(&device));
        let vignette = Arc::new(VignetteKernel::new(&device));
        let shadow_exposure = Arc::new(ShadowExposureKernel::new(&device));
        let split_tone = Arc::new(SplitToneKernel::new(&device));
        let black_and_white = Arc::new(BlackAndWhiteKernel::new(&device));
        let blur = Arc::new(BlurKernel::new(&device));
        let color_balance = Arc::new(ColorBalanceKernel::new(&device));
        let color_space = Arc::new(ColorSpaceKernel::new(&device));
        let denoise = Arc::new(DenoiseKernel::new(&device));
        let hsl_panel = Arc::new(HslPanelKernel::new(&device));
        let sharpen = Arc::new(SharpenKernel::new(&device));
        let faux_hdr = Arc::new(FauxHdrKernel::new(&device));
        let clarity_texture = Arc::new(ClarityTextureKernel::new(&device));
        Self {
            device: Arc::new(device),
            queue: Arc::new(queue),
            limits,
            brightness_contrast,
            curves,
            hue_shift,
            saturation,
            vibrance,
            white_balance,
            noise_reduction_nlm,
            sepia,
            levels,
            highlights_shadows,
            vignette,
            shadow_exposure,
            split_tone,
            black_and_white,
            blur,
            color_balance,
            color_space,
            denoise,
            hsl_panel,
            sharpen,
            faux_hdr,
            clarity_texture,
        }
    }

    pub fn limits(&self) -> &wgpu::Limits {
        &self.limits
    }
}
