//! GPU kernels for RasterLab operations.
//!
//! This crate intentionally stays below the GUI/rendering layer. It owns no
//! windows or egui textures; callers provide a `wgpu::Device` and `wgpu::Queue`.

use std::{
    sync::{Arc, mpsc},
    time::Instant,
};

use bytemuck::{Pod, Zeroable};
use rasterlab_core::{
    Image,
    image::ImageMetadata,
    ops::{
        BlackAndWhiteOp, BlurOp, BrightnessContrastOp, BwMode, ClarityTextureOp, ColorBalanceOp,
        ColorSpaceConversion, ColorSpaceOp, CurvesOp, DenoiseOp, FauxHdrOp, HighlightsShadowsOp,
        HslPanelOp, HueShiftOp, LevelsOp, NoiseReductionOp, NrMethod, SaturationOp, SepiaOp,
        ShadowExposureOp, SharpenOp, SplitToneOp, VibranceOp, VignetteOp, WhiteBalanceOp,
    },
    traits::operation::Operation,
};
use thiserror::Error;
use wgpu::util::DeviceExt;

const WORKGROUP_SIZE_X: u32 = 16;
const WORKGROUP_SIZE_Y: u32 = 16;

#[derive(Debug, Error)]
pub enum GpuError {
    #[error("invalid image buffer: got {actual} bytes, expected {expected}")]
    InvalidImageBuffer { actual: usize, expected: usize },
    #[error("unsupported operation '{0}'")]
    UnsupportedOperation(&'static str),
    #[error("buffer map failed: {0}")]
    BufferMap(String),
    #[error("device poll failed: {0}")]
    Poll(String),
    #[error("readback channel closed")]
    ReadbackChannelClosed,
    #[error("image conversion failed: {0}")]
    ImageConversion(String),
    #[error("GPU pipeline has already been consumed")]
    PipelineConsumed,
}

#[derive(Clone)]
pub struct GpuContext {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    limits: wgpu::Limits,
    brightness_contrast: Arc<BrightnessContrastKernel>,
    curves: Arc<CurvesKernel>,
    hue_shift: Arc<HueShiftKernel>,
    saturation: Arc<SaturationKernel>,
    vibrance: Arc<VibranceKernel>,
    white_balance: Arc<WhiteBalanceKernel>,
    noise_reduction_nlm: Arc<NoiseReductionNlmKernel>,
    sepia: Arc<SepiaKernel>,
    levels: Arc<LevelsKernel>,
    highlights_shadows: Arc<HighlightsShadowsKernel>,
    vignette: Arc<VignetteKernel>,
    shadow_exposure: Arc<ShadowExposureKernel>,
    split_tone: Arc<SplitToneKernel>,
    black_and_white: Arc<BlackAndWhiteKernel>,
    blur: Arc<BlurKernel>,
    color_balance: Arc<ColorBalanceKernel>,
    color_space: Arc<ColorSpaceKernel>,
    denoise: Arc<DenoiseKernel>,
    hsl_panel: Arc<HslPanelKernel>,
    sharpen: Arc<SharpenKernel>,
    faux_hdr: Arc<FauxHdrKernel>,
    clarity_texture: Arc<ClarityTextureKernel>,
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

pub struct GpuImage {
    width: u32,
    height: u32,
    buffer: wgpu::Buffer,
}

impl GpuImage {
    pub fn from_image(ctx: &GpuContext, image: &Image) -> Result<Self, GpuError> {
        let expected = expected_rgba_len(image.width, image.height);
        if image.data.len() != expected {
            return Err(GpuError::InvalidImageBuffer {
                actual: image.data.len(),
                expected,
            });
        }
        let buffer = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("rasterlab gpu image upload"),
                contents: &image.data,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_SRC
                    | wgpu::BufferUsages::COPY_DST,
            });
        Ok(Self {
            width: image.width,
            height: image.height,
            buffer,
        })
    }

    pub fn into_image(self, ctx: &GpuContext) -> Result<Image, GpuError> {
        let bytes = self.read_rgba8(ctx)?;
        Image::from_rgba8(self.width, self.height, bytes)
            .map_err(|e| GpuError::ImageConversion(e.to_string()))
    }

    pub fn read_rgba8(&self, ctx: &GpuContext) -> Result<Vec<u8>, GpuError> {
        let byte_len = expected_rgba_len(self.width, self.height) as u64;
        let staging = ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rasterlab gpu image readback"),
            size: byte_len,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut encoder = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("rasterlab gpu readback encoder"),
            });
        encoder.copy_buffer_to_buffer(&self.buffer, 0, &staging, 0, byte_len);
        ctx.queue.submit(Some(encoder.finish()));

        let slice = staging.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result.map_err(|e| e.to_string()));
        });
        ctx.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| GpuError::Poll(e.to_string()))?;
        rx.recv()
            .map_err(|_| GpuError::ReadbackChannelClosed)?
            .map_err(GpuError::BufferMap)?;

        let data = slice.get_mapped_range().to_vec();
        staging.unmap();
        Ok(data)
    }
}

pub fn supports(op: &dyn Operation) -> bool {
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<BrightnessContrastOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<CurvesOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<HueShiftOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<SaturationOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<VibranceOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<WhiteBalanceOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<NoiseReductionOp>())
        .is_some_and(|op| op.method == NrMethod::NonLocalMeans)
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<SepiaOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<LevelsOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<HighlightsShadowsOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<VignetteOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<ShadowExposureOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<SplitToneOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<BlackAndWhiteOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<BlurOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<ColorBalanceOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<ColorSpaceOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<DenoiseOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<HslPanelOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<SharpenOp>())
        .is_some()
    {
        return true;
    }
    if op
        .as_any()
        .and_then(|any| any.downcast_ref::<FauxHdrOp>())
        .is_some()
    {
        return true;
    }
    op.as_any()
        .and_then(|any| any.downcast_ref::<ClarityTextureOp>())
        .is_some()
}

pub fn apply_one(
    ctx: &GpuContext,
    op: &dyn Operation,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<BrightnessContrastOp>())
    {
        apply_brightness_contrast(ctx, op, image)
    } else if let Some(op) = op.as_any().and_then(|any| any.downcast_ref::<CurvesOp>()) {
        apply_curves(ctx, op, image)
    } else if let Some(op) = op.as_any().and_then(|any| any.downcast_ref::<HueShiftOp>()) {
        apply_hue_shift(ctx, op, image)
    } else if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<SaturationOp>())
    {
        apply_saturation(ctx, op, image)
    } else if let Some(op) = op.as_any().and_then(|any| any.downcast_ref::<VibranceOp>()) {
        apply_vibrance(ctx, op, image)
    } else if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<WhiteBalanceOp>())
    {
        apply_white_balance(ctx, op, image)
    } else if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<NoiseReductionOp>())
        .filter(|op| op.method == NrMethod::NonLocalMeans)
    {
        apply_noise_reduction_nlm(ctx, op, image)
    } else if let Some(op) = op.as_any().and_then(|any| any.downcast_ref::<SepiaOp>()) {
        apply_sepia(ctx, op, image)
    } else if let Some(op) = op.as_any().and_then(|any| any.downcast_ref::<LevelsOp>()) {
        apply_levels(ctx, op, image)
    } else if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<HighlightsShadowsOp>())
    {
        apply_highlights_shadows(ctx, op, image)
    } else if let Some(op) = op.as_any().and_then(|any| any.downcast_ref::<VignetteOp>()) {
        apply_vignette(ctx, op, image)
    } else if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<ShadowExposureOp>())
    {
        apply_shadow_exposure(ctx, op, image)
    } else if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<SplitToneOp>())
    {
        apply_split_tone(ctx, op, image)
    } else if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<BlackAndWhiteOp>())
    {
        apply_black_and_white(ctx, op, image)
    } else if let Some(op) = op.as_any().and_then(|any| any.downcast_ref::<BlurOp>()) {
        apply_blur(ctx, op, image)
    } else if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<ColorBalanceOp>())
    {
        apply_color_balance(ctx, op, image)
    } else if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<ColorSpaceOp>())
    {
        apply_color_space(ctx, op, image)
    } else if let Some(op) = op.as_any().and_then(|any| any.downcast_ref::<DenoiseOp>()) {
        apply_denoise(ctx, op, image)
    } else if let Some(op) = op.as_any().and_then(|any| any.downcast_ref::<HslPanelOp>()) {
        apply_hsl_panel(ctx, op, image)
    } else if let Some(op) = op.as_any().and_then(|any| any.downcast_ref::<SharpenOp>()) {
        apply_sharpen(ctx, op, image)
    } else if let Some(op) = op.as_any().and_then(|any| any.downcast_ref::<FauxHdrOp>()) {
        apply_faux_hdr(ctx, op, image)
    } else if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<ClarityTextureOp>())
    {
        apply_clarity_texture(ctx, op, image)
    } else {
        Err(GpuError::UnsupportedOperation(op.name()))
    }
}

pub fn apply_one_to_image(
    ctx: &GpuContext,
    op: &dyn Operation,
    image: &Image,
) -> Result<(Image, GpuTimings), GpuError> {
    let mut pipeline = GpuPipeline::from_image(ctx, image)?;
    pipeline.apply_op(ctx, op)?;
    pipeline.into_image(ctx)
}

pub struct GpuPipeline {
    image: Option<GpuImage>,
    metadata: ImageMetadata,
    timings: GpuTimings,
    op_count: usize,
}

impl GpuPipeline {
    pub fn from_image(ctx: &GpuContext, image: &Image) -> Result<Self, GpuError> {
        let start = Instant::now();
        let gpu_image = GpuImage::from_image(ctx, image)?;
        let upload = start.elapsed();

        Ok(Self {
            image: Some(gpu_image),
            metadata: image.metadata.clone(),
            timings: GpuTimings {
                upload,
                dispatch: Default::default(),
                readback: Default::default(),
            },
            op_count: 0,
        })
    }

    pub fn apply_op(&mut self, ctx: &GpuContext, op: &dyn Operation) -> Result<(), GpuError> {
        let gpu_image = self.image.take().ok_or(GpuError::PipelineConsumed)?;
        let dispatch_start = Instant::now();
        let gpu_image = apply_one(ctx, op, gpu_image)?;
        self.timings.dispatch += dispatch_start.elapsed();
        self.op_count += 1;
        self.image = Some(gpu_image);
        Ok(())
    }

    pub fn apply_ops<'a, I>(&mut self, ctx: &GpuContext, ops: I) -> Result<(), GpuError>
    where
        I: IntoIterator<Item = &'a dyn Operation>,
    {
        for op in ops {
            self.apply_op(ctx, op)?;
        }
        Ok(())
    }

    pub fn into_image(mut self, ctx: &GpuContext) -> Result<(Image, GpuTimings), GpuError> {
        let gpu_image = self.image.take().ok_or(GpuError::PipelineConsumed)?;
        let readback_start = Instant::now();
        let mut out = gpu_image.into_image(ctx)?;
        self.timings.readback += readback_start.elapsed();
        out.metadata = self.metadata;

        Ok((out, self.timings))
    }

    pub fn timings(&self) -> GpuTimings {
        self.timings
    }

    pub fn op_count(&self) -> usize {
        self.op_count
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct GpuTimings {
    pub upload: std::time::Duration,
    pub dispatch: std::time::Duration,
    pub readback: std::time::Duration,
}

struct BrightnessContrastKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct CurvesKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct HueShiftKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct SaturationKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct VibranceKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct WhiteBalanceKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct SepiaKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct LevelsKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct HighlightsShadowsKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct VignetteKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct ShadowExposureKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct SplitToneKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct BlackAndWhiteKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct BlurKernel {
    h_pipeline: wgpu::ComputePipeline,
    v_pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct ColorBalanceKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct ColorSpaceKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct DenoiseKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct HslPanelKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct SharpenKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct FauxHdrKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
}

struct ClarityTextureKernel {
    three_bind_layout: wgpu::BindGroupLayout,
    extract_luma_pipeline: wgpu::ComputePipeline,
    box_blur_h_pipeline: wgpu::ComputePipeline,
    box_blur_v_pipeline: wgpu::ComputePipeline,
    four_bind_layout: wgpu::BindGroupLayout,
    apply_detail_pipeline: wgpu::ComputePipeline,
}

impl BrightnessContrastKernel {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rasterlab brightness_contrast shader"),
            source: wgpu::ShaderSource::Wgsl(BRIGHTNESS_CONTRAST_WGSL.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rasterlab brightness_contrast bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rasterlab brightness_contrast pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab brightness_contrast pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl CurvesKernel {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rasterlab curves shader"),
            source: wgpu::ShaderSource::Wgsl(CURVES_WGSL.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rasterlab curves bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rasterlab curves pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab curves pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl HueShiftKernel {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rasterlab hue_shift shader"),
            source: wgpu::ShaderSource::Wgsl(HUE_SHIFT_WGSL.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rasterlab hue_shift bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rasterlab hue_shift pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab hue_shift pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl SaturationKernel {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rasterlab saturation shader"),
            source: wgpu::ShaderSource::Wgsl(SATURATION_WGSL.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rasterlab saturation bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rasterlab saturation pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab saturation pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl VibranceKernel {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rasterlab vibrance shader"),
            source: wgpu::ShaderSource::Wgsl(VIBRANCE_WGSL.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rasterlab vibrance bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rasterlab vibrance pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab vibrance pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl WhiteBalanceKernel {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rasterlab white_balance shader"),
            source: wgpu::ShaderSource::Wgsl(WHITE_BALANCE_WGSL.into()),
        });
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("rasterlab white_balance bind group layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: true },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rasterlab white_balance pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab white_balance pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

fn make_3binding_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

fn make_4binding_layout(device: &wgpu::Device, label: &str) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: false },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 3,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ],
    })
}

fn make_simple_pipeline(
    device: &wgpu::Device,
    wgsl: &str,
    bind_group_layout: &wgpu::BindGroupLayout,
    shader_label: &str,
    pipeline_label: &str,
) -> wgpu::ComputePipeline {
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(shader_label),
        source: wgpu::ShaderSource::Wgsl(wgsl.into()),
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(pipeline_label),
        bind_group_layouts: &[Some(bind_group_layout)],
        immediate_size: 0,
    });
    device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some(pipeline_label),
        layout: Some(&layout),
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    })
}

impl SepiaKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = make_3binding_layout(device, "rasterlab sepia bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            SEPIA_WGSL,
            &bind_group_layout,
            "rasterlab sepia shader",
            "rasterlab sepia pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl LevelsKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = make_4binding_layout(device, "rasterlab levels bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            LEVELS_WGSL,
            &bind_group_layout,
            "rasterlab levels shader",
            "rasterlab levels pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl HighlightsShadowsKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab highlights_shadows bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            HIGHLIGHTS_SHADOWS_WGSL,
            &bind_group_layout,
            "rasterlab highlights_shadows shader",
            "rasterlab highlights_shadows pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl VignetteKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab vignette bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            VIGNETTE_WGSL,
            &bind_group_layout,
            "rasterlab vignette shader",
            "rasterlab vignette pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl ShadowExposureKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab shadow_exposure bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            SHADOW_EXPOSURE_WGSL,
            &bind_group_layout,
            "rasterlab shadow_exposure shader",
            "rasterlab shadow_exposure pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl SplitToneKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab split_tone bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            SPLIT_TONE_WGSL,
            &bind_group_layout,
            "rasterlab split_tone shader",
            "rasterlab split_tone pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl BlackAndWhiteKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab black_and_white bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            BLACK_AND_WHITE_WGSL,
            &bind_group_layout,
            "rasterlab black_and_white shader",
            "rasterlab black_and_white pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl BlurKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = make_3binding_layout(device, "rasterlab blur bind group layout");
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rasterlab blur shader"),
            source: wgpu::ShaderSource::Wgsl(BLUR_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rasterlab blur pipeline layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let h_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab blur h_pipeline"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("main_h"),
            compilation_options: Default::default(),
            cache: None,
        });
        let v_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab blur v_pipeline"),
            layout: Some(&layout),
            module: &shader,
            entry_point: Some("main_v"),
            compilation_options: Default::default(),
            cache: None,
        });
        Self {
            h_pipeline,
            v_pipeline,
            bind_group_layout,
        }
    }
}

impl ColorBalanceKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab color_balance bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            COLOR_BALANCE_WGSL,
            &bind_group_layout,
            "rasterlab color_balance shader",
            "rasterlab color_balance pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl ColorSpaceKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab color_space bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            COLOR_SPACE_WGSL,
            &bind_group_layout,
            "rasterlab color_space shader",
            "rasterlab color_space pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl DenoiseKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = make_3binding_layout(device, "rasterlab denoise bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            DENOISE_WGSL,
            &bind_group_layout,
            "rasterlab denoise shader",
            "rasterlab denoise pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl HslPanelKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab hsl_panel bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            HSL_PANEL_WGSL,
            &bind_group_layout,
            "rasterlab hsl_panel shader",
            "rasterlab hsl_panel pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl SharpenKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout = make_3binding_layout(device, "rasterlab sharpen bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            SHARPEN_WGSL,
            &bind_group_layout,
            "rasterlab sharpen shader",
            "rasterlab sharpen pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl FauxHdrKernel {
    fn new(device: &wgpu::Device) -> Self {
        let bind_group_layout =
            make_3binding_layout(device, "rasterlab faux_hdr bind group layout");
        let pipeline = make_simple_pipeline(
            device,
            FAUX_HDR_WGSL,
            &bind_group_layout,
            "rasterlab faux_hdr shader",
            "rasterlab faux_hdr pipeline",
        );
        Self {
            pipeline,
            bind_group_layout,
        }
    }
}

impl ClarityTextureKernel {
    fn new(device: &wgpu::Device) -> Self {
        let three_bind_layout = make_3binding_layout(device, "rasterlab clarity 3-bind layout");
        let four_bind_layout = make_4binding_layout(device, "rasterlab clarity 4-bind layout");
        let extract_luma_pipeline = make_simple_pipeline(
            device,
            CLARITY_EXTRACT_LUMA_WGSL,
            &three_bind_layout,
            "rasterlab clarity extract_luma shader",
            "rasterlab clarity extract_luma pipeline",
        );
        let box_blur_h_pipeline = make_simple_pipeline(
            device,
            CLARITY_BOX_BLUR_H_WGSL,
            &three_bind_layout,
            "rasterlab clarity box_blur_h shader",
            "rasterlab clarity box_blur_h pipeline",
        );
        let box_blur_v_pipeline = make_simple_pipeline(
            device,
            CLARITY_BOX_BLUR_V_WGSL,
            &three_bind_layout,
            "rasterlab clarity box_blur_v shader",
            "rasterlab clarity box_blur_v pipeline",
        );
        let apply_detail_pipeline = make_simple_pipeline(
            device,
            CLARITY_APPLY_DETAIL_WGSL,
            &four_bind_layout,
            "rasterlab clarity apply_detail shader",
            "rasterlab clarity apply_detail pipeline",
        );
        Self {
            three_bind_layout,
            extract_luma_pipeline,
            box_blur_h_pipeline,
            box_blur_v_pipeline,
            four_bind_layout,
            apply_detail_pipeline,
        }
    }
}

struct NoiseReductionNlmKernel {
    nlm_pipeline: wgpu::ComputePipeline,
    nlm_bind_group_layout: wgpu::BindGroupLayout,
    detail_pipeline: wgpu::ComputePipeline,
    detail_bind_group_layout: wgpu::BindGroupLayout,
}

impl NoiseReductionNlmKernel {
    fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("rasterlab noise_reduction_nlm shader"),
            source: wgpu::ShaderSource::Wgsl(NOISE_REDUCTION_NLM_WGSL.into()),
        });
        let nlm_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("rasterlab noise_reduction_nlm bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let nlm_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("rasterlab noise_reduction_nlm pipeline layout"),
            bind_group_layouts: &[Some(&nlm_bind_group_layout)],
            immediate_size: 0,
        });
        let nlm_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab noise_reduction_nlm pipeline"),
            layout: Some(&nlm_pipeline_layout),
            module: &shader,
            entry_point: Some("nlm_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let detail_bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("rasterlab noise_reduction_detail bind group layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: false },
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 3,
                        visibility: wgpu::ShaderStages::COMPUTE,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: None,
                        },
                        count: None,
                    },
                ],
            });
        let detail_pipeline_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("rasterlab noise_reduction_detail pipeline layout"),
                bind_group_layouts: &[Some(&detail_bind_group_layout)],
                immediate_size: 0,
            });
        let detail_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("rasterlab noise_reduction_detail pipeline"),
            layout: Some(&detail_pipeline_layout),
            module: &shader,
            entry_point: Some("detail_main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        Self {
            nlm_pipeline,
            nlm_bind_group_layout,
            detail_pipeline,
            detail_bind_group_layout,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct LutParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
}

fn apply_brightness_contrast(
    ctx: &GpuContext,
    op: &BrightnessContrastOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab brightness_contrast output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = LutParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab brightness_contrast params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let lut = build_brightness_contrast_lut(op.brightness, op.contrast);
    let lut_u32: [u32; 256] = lut.map(u32::from);
    let lut_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab brightness_contrast lut"),
            contents: bytemuck::cast_slice(&lut_u32),
            usage: wgpu::BufferUsages::STORAGE,
        });
    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rasterlab brightness_contrast bind group"),
        layout: &ctx.brightness_contrast.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: image.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: lut_buffer.as_entire_binding(),
            },
        ],
    });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab brightness_contrast encoder"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rasterlab brightness_contrast pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.brightness_contrast.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        let groups_x = params.width.div_ceil(WORKGROUP_SIZE_X);
        let groups_y = params.height.div_ceil(WORKGROUP_SIZE_Y);
        pass.dispatch_workgroups(groups_x, groups_y, 1);
    }
    ctx.queue.submit(Some(encoder.finish()));
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GpuError::Poll(e.to_string()))?;

    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_curves(ctx: &GpuContext, op: &CurvesOp, image: GpuImage) -> Result<GpuImage, GpuError> {
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab curves output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = LutParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab curves params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let lut = CurvesOp::build_lut(&op.points);
    let lut_u32: [u32; 256] = lut.map(u32::from);
    let lut_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab curves lut"),
            contents: bytemuck::cast_slice(&lut_u32),
            usage: wgpu::BufferUsages::STORAGE,
        });
    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rasterlab curves bind group"),
        layout: &ctx.curves.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: image.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: lut_buffer.as_entire_binding(),
            },
        ],
    });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab curves encoder"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rasterlab curves pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.curves.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        let groups_x = params.width.div_ceil(WORKGROUP_SIZE_X);
        let groups_y = params.height.div_ceil(WORKGROUP_SIZE_Y);
        pass.dispatch_workgroups(groups_x, groups_y, 1);
    }
    ctx.queue.submit(Some(encoder.finish()));
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GpuError::Poll(e.to_string()))?;

    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct NrNlmParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    luma_h2: f32,
    color_h2: f32,
    detail: f32,
    _pad2: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SaturationParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    saturation: f32,
    _pad2: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VibranceParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    strength: f32,
    _pad2: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct HueShiftParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    shift: f32,
    _pad2: [f32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct WhiteBalanceParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    r_scale: f32,
    g_scale: f32,
    b_scale: f32,
    _pad2: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SepiaParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    strength: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct HighlightsShadowsParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    highlights: f32,
    shadows: f32,
    _pad2: f32,
    _pad3: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct VignetteParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    strength: f32,
    inner: f32,
    zone: f32,
    _pad2: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ShadowExposureParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    ev: f32,
    falloff: f32,
    _pad2: f32,
    _pad3: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SplitToneParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    sh_r: f32,
    sh_g: f32,
    sh_b: f32,
    shadow_sat: f32,
    hi_r: f32,
    hi_g: f32,
    hi_b: f32,
    highlight_sat: f32,
    balance: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
}

fn apply_hue_shift(
    ctx: &GpuContext,
    op: &HueShiftOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if op.degrees.abs() < 1e-3 {
        return Ok(image);
    }

    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab hue_shift output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = HueShiftParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        shift: op.degrees / 360.0,
        _pad2: [0.0; 3],
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab hue_shift params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rasterlab hue_shift bind group"),
        layout: &ctx.hue_shift.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: image.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab hue_shift encoder"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rasterlab hue_shift pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.hue_shift.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        let groups_x = params.width.div_ceil(WORKGROUP_SIZE_X);
        let groups_y = params.height.div_ceil(WORKGROUP_SIZE_Y);
        pass.dispatch_workgroups(groups_x, groups_y, 1);
    }
    ctx.queue.submit(Some(encoder.finish()));
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GpuError::Poll(e.to_string()))?;

    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_saturation(
    ctx: &GpuContext,
    op: &SaturationOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if (op.saturation - 1.0).abs() < 1e-5 {
        return Ok(image);
    }

    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab saturation output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = SaturationParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        saturation: op.saturation.clamp(0.0, 4.0),
        _pad2: [0.0; 3],
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab saturation params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rasterlab saturation bind group"),
        layout: &ctx.saturation.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: image.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab saturation encoder"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rasterlab saturation pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.saturation.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        let groups_x = params.width.div_ceil(WORKGROUP_SIZE_X);
        let groups_y = params.height.div_ceil(WORKGROUP_SIZE_Y);
        pass.dispatch_workgroups(groups_x, groups_y, 1);
    }
    ctx.queue.submit(Some(encoder.finish()));
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GpuError::Poll(e.to_string()))?;

    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_vibrance(
    ctx: &GpuContext,
    op: &VibranceOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if op.strength.abs() < 1e-5 {
        return Ok(image);
    }

    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab vibrance output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = VibranceParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        strength: op.strength.clamp(-1.0, 1.0),
        _pad2: [0.0; 3],
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab vibrance params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rasterlab vibrance bind group"),
        layout: &ctx.vibrance.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: image.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab vibrance encoder"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rasterlab vibrance pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.vibrance.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        let groups_x = params.width.div_ceil(WORKGROUP_SIZE_X);
        let groups_y = params.height.div_ceil(WORKGROUP_SIZE_Y);
        pass.dispatch_workgroups(groups_x, groups_y, 1);
    }
    ctx.queue.submit(Some(encoder.finish()));
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GpuError::Poll(e.to_string()))?;

    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_white_balance(
    ctx: &GpuContext,
    op: &WhiteBalanceOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if op.temperature.abs() < 1e-5 && op.tint.abs() < 1e-5 {
        return Ok(image);
    }

    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab white_balance output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let temp = op.temperature.clamp(-1.0, 1.0);
    let tint = op.tint.clamp(-1.0, 1.0);
    let params = WhiteBalanceParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        r_scale: 1.0 + temp * 0.3,
        g_scale: 1.0 - tint * 0.15,
        b_scale: 1.0 - temp * 0.3,
        _pad2: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab white_balance params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rasterlab white_balance bind group"),
        layout: &ctx.white_balance.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: image.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab white_balance encoder"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rasterlab white_balance pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.white_balance.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        let groups_x = params.width.div_ceil(WORKGROUP_SIZE_X);
        let groups_y = params.height.div_ceil(WORKGROUP_SIZE_Y);
        pass.dispatch_workgroups(groups_x, groups_y, 1);
    }
    ctx.queue.submit(Some(encoder.finish()));
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GpuError::Poll(e.to_string()))?;

    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

#[allow(clippy::too_many_arguments)]
fn dispatch_3binding(
    ctx: &GpuContext,
    pipeline: &wgpu::ComputePipeline,
    layout: &wgpu::BindGroupLayout,
    label: &str,
    input: &wgpu::Buffer,
    output: &wgpu::Buffer,
    params: &wgpu::Buffer,
    width: u32,
    height: u32,
) -> Result<(), GpuError> {
    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: input.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params.as_entire_binding(),
            },
        ],
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some(label),
            timestamp_writes: None,
        });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            width.div_ceil(WORKGROUP_SIZE_X),
            height.div_ceil(WORKGROUP_SIZE_Y),
            1,
        );
    }
    ctx.queue.submit(Some(encoder.finish()));
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GpuError::Poll(e.to_string()))?;
    Ok(())
}

fn hue_to_rgb_f32(hue: f32) -> (f32, f32, f32) {
    let h = hue.rem_euclid(360.0) / 60.0;
    let i = h.floor() as u32;
    let f = h - i as f32;
    match i {
        0 => (1.0, f, 0.0),
        1 => (1.0 - f, 1.0, 0.0),
        2 => (0.0, 1.0, f),
        3 => (0.0, 1.0 - f, 1.0),
        4 => (f, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - f),
    }
}

fn apply_sepia(ctx: &GpuContext, op: &SepiaOp, image: GpuImage) -> Result<GpuImage, GpuError> {
    if op.strength < 1e-5 {
        return Ok(image);
    }
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab sepia output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = SepiaParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        strength: op.strength.clamp(0.0, 1.0),
        _pad2: 0.0,
        _pad3: 0.0,
        _pad4: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab sepia params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_3binding(
        ctx,
        &ctx.sepia.pipeline,
        &ctx.sepia.bind_group_layout,
        "rasterlab sepia",
        &image.buffer,
        &output,
        &params_buffer,
        params.width,
        params.height,
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_levels(ctx: &GpuContext, op: &LevelsOp, image: GpuImage) -> Result<GpuImage, GpuError> {
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab levels output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = LutParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab levels params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let lut = op.build_lut();
    let lut_u32: [u32; 256] = lut.map(u32::from);
    let lut_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab levels lut"),
            contents: bytemuck::cast_slice(&lut_u32),
            usage: wgpu::BufferUsages::STORAGE,
        });
    let bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rasterlab levels bind group"),
        layout: &ctx.levels.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: image.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: lut_buffer.as_entire_binding(),
            },
        ],
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab levels encoder"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rasterlab levels pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.levels.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(
            params.width.div_ceil(WORKGROUP_SIZE_X),
            params.height.div_ceil(WORKGROUP_SIZE_Y),
            1,
        );
    }
    ctx.queue.submit(Some(encoder.finish()));
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GpuError::Poll(e.to_string()))?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_highlights_shadows(
    ctx: &GpuContext,
    op: &HighlightsShadowsOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if op.highlights.abs() < 1e-5 && op.shadows.abs() < 1e-5 {
        return Ok(image);
    }
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab highlights_shadows output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = HighlightsShadowsParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        highlights: op.highlights.clamp(-1.0, 1.0),
        shadows: op.shadows.clamp(-1.0, 1.0),
        _pad2: 0.0,
        _pad3: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab highlights_shadows params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_3binding(
        ctx,
        &ctx.highlights_shadows.pipeline,
        &ctx.highlights_shadows.bind_group_layout,
        "rasterlab highlights_shadows",
        &image.buffer,
        &output,
        &params_buffer,
        params.width,
        params.height,
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_vignette(
    ctx: &GpuContext,
    op: &VignetteOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if op.strength < 1e-5 {
        return Ok(image);
    }
    let inner = op.radius;
    let outer = inner + op.feather * (1.0 - inner);
    let zone = (outer - inner).max(1e-6);
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab vignette output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = VignetteParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        strength: op.strength.clamp(0.0, 1.0),
        inner,
        zone,
        _pad2: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab vignette params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_3binding(
        ctx,
        &ctx.vignette.pipeline,
        &ctx.vignette.bind_group_layout,
        "rasterlab vignette",
        &image.buffer,
        &output,
        &params_buffer,
        params.width,
        params.height,
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_shadow_exposure(
    ctx: &GpuContext,
    op: &ShadowExposureOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if op.ev.abs() < 1e-5 {
        return Ok(image);
    }
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab shadow_exposure output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = ShadowExposureParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        ev: op.ev.clamp(-3.0, 3.0),
        falloff: op.falloff.clamp(0.5, 4.0),
        _pad2: 0.0,
        _pad3: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab shadow_exposure params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_3binding(
        ctx,
        &ctx.shadow_exposure.pipeline,
        &ctx.shadow_exposure.bind_group_layout,
        "rasterlab shadow_exposure",
        &image.buffer,
        &output,
        &params_buffer,
        params.width,
        params.height,
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_split_tone(
    ctx: &GpuContext,
    op: &SplitToneOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if op.shadow_sat < 1e-4 && op.highlight_sat < 1e-4 {
        return Ok(image);
    }
    let (sh_r, sh_g, sh_b) = hue_to_rgb_f32(op.shadow_hue);
    let (hi_r, hi_g, hi_b) = hue_to_rgb_f32(op.highlight_hue);
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab split_tone output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = SplitToneParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        sh_r,
        sh_g,
        sh_b,
        shadow_sat: op.shadow_sat,
        hi_r,
        hi_g,
        hi_b,
        highlight_sat: op.highlight_sat,
        balance: op.balance * 0.5,
        _pad2: 0.0,
        _pad3: 0.0,
        _pad4: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab split_tone params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_3binding(
        ctx,
        &ctx.split_tone.pipeline,
        &ctx.split_tone.bind_group_layout,
        "rasterlab split_tone",
        &image.buffer,
        &output,
        &params_buffer,
        params.width,
        params.height,
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_black_and_white(
    ctx: &GpuContext,
    op: &BlackAndWhiteOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    let (mode, rw, gw, bw) = match &op.mode {
        BwMode::Luminance => (0u32, 0.0f32, 0.0, 0.0),
        BwMode::Average => (1, 0.0, 0.0, 0.0),
        BwMode::Perceptual => (2, 0.0, 0.0, 0.0),
        BwMode::ChannelMixer { r, g, b } => (3, *r, *g, *b),
    };
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab black_and_white output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = BlackAndWhiteParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        mode,
        _pad2: 0,
        _pad3: 0,
        _pad4: 0,
        rw,
        gw,
        bw,
        _pad5: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab black_and_white params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_3binding(
        ctx,
        &ctx.black_and_white.pipeline,
        &ctx.black_and_white.bind_group_layout,
        "rasterlab black_and_white",
        &image.buffer,
        &output,
        &params_buffer,
        params.width,
        params.height,
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_blur(ctx: &GpuContext, op: &BlurOp, image: GpuImage) -> Result<GpuImage, GpuError> {
    let sigma = op.radius.clamp(0.1, 100.0);
    let kernel_radius = (sigma * 3.0).ceil().clamp(1.0, 300.0) as u32;
    let byte_len = expected_rgba_len(image.width, image.height) as u64;

    let intermediate = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab blur intermediate"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab blur output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = BlurParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        kernel_radius,
        sigma,
        _pad: 0.0,
        _pad2: 0.0,
        _pad3: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab blur params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });

    let h_bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rasterlab blur h bind group"),
        layout: &ctx.blur.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: image.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: intermediate.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });
    let v_bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rasterlab blur v bind group"),
        layout: &ctx.blur.bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: intermediate.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab blur encoder"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rasterlab blur h pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.blur.h_pipeline);
        pass.set_bind_group(0, &h_bind_group, &[]);
        pass.dispatch_workgroups(
            params.width.div_ceil(WORKGROUP_SIZE_X),
            params.height.div_ceil(WORKGROUP_SIZE_Y),
            1,
        );
    }
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rasterlab blur v pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.blur.v_pipeline);
        pass.set_bind_group(0, &v_bind_group, &[]);
        pass.dispatch_workgroups(
            params.width.div_ceil(WORKGROUP_SIZE_X),
            params.height.div_ceil(WORKGROUP_SIZE_Y),
            1,
        );
    }
    ctx.queue.submit(Some(encoder.finish()));
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GpuError::Poll(e.to_string()))?;

    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_color_balance(
    ctx: &GpuContext,
    op: &ColorBalanceOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if op.is_identity() {
        return Ok(image);
    }
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab color_balance output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = ColorBalanceParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        cr0: op.cyan_red[0],
        cr1: op.cyan_red[1],
        cr2: op.cyan_red[2],
        _pad2: 0.0,
        mg0: op.magenta_green[0],
        mg1: op.magenta_green[1],
        mg2: op.magenta_green[2],
        _pad3: 0.0,
        yb0: op.yellow_blue[0],
        yb1: op.yellow_blue[1],
        yb2: op.yellow_blue[2],
        _pad4: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab color_balance params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_3binding(
        ctx,
        &ctx.color_balance.pipeline,
        &ctx.color_balance.bind_group_layout,
        "rasterlab color_balance",
        &image.buffer,
        &output,
        &params_buffer,
        params.width,
        params.height,
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_color_space(
    ctx: &GpuContext,
    op: &ColorSpaceOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    let mat = match op.conversion {
        ColorSpaceConversion::SrgbToDisplayP3 => &SRGB_TO_P3,
        ColorSpaceConversion::DisplayP3ToSrgb => &P3_TO_SRGB,
    };
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab color_space output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = ColorSpaceParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        m0: mat[0],
        m1: mat[1],
        m2: mat[2],
        _pad2: 0.0,
        m3: mat[3],
        m4: mat[4],
        m5: mat[5],
        _pad3: 0.0,
        m6: mat[6],
        m7: mat[7],
        m8: mat[8],
        _pad4: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab color_space params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_3binding(
        ctx,
        &ctx.color_space.pipeline,
        &ctx.color_space.bind_group_layout,
        "rasterlab color_space",
        &image.buffer,
        &output,
        &params_buffer,
        params.width,
        params.height,
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_denoise(ctx: &GpuContext, op: &DenoiseOp, image: GpuImage) -> Result<GpuImage, GpuError> {
    let sigma_r = op.strength.clamp(0.01, 1.0);
    let r = op.radius.clamp(1, 10) as f32;
    let sigma_s = r.max(1.0) * 0.5;
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab denoise output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = DenoiseParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        radius: op.radius.clamp(1, 10),
        sigma_r2: 2.0 * sigma_r * sigma_r,
        sigma_s2: 2.0 * sigma_s * sigma_s,
        _pad: 0.0,
        _pad2: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab denoise params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_3binding(
        ctx,
        &ctx.denoise.pipeline,
        &ctx.denoise.bind_group_layout,
        "rasterlab denoise",
        &image.buffer,
        &output,
        &params_buffer,
        params.width,
        params.height,
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_hsl_panel(
    ctx: &GpuContext,
    op: &HslPanelOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if op.is_identity() {
        return Ok(image);
    }
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab hsl_panel output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = HslPanelParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        hue: op.hue,
        sat: op.saturation,
        lum: op.luminance,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab hsl_panel params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_3binding(
        ctx,
        &ctx.hsl_panel.pipeline,
        &ctx.hsl_panel.bind_group_layout,
        "rasterlab hsl_panel",
        &image.buffer,
        &output,
        &params_buffer,
        params.width,
        params.height,
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_sharpen(ctx: &GpuContext, op: &SharpenOp, image: GpuImage) -> Result<GpuImage, GpuError> {
    if op.strength <= 0.0 {
        return Ok(image);
    }
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab sharpen output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = SharpenParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        luminance_only: if op.luminance_only { 1u32 } else { 0u32 },
        strength: op.strength,
        _pad: 0.0,
        _pad2: 0.0,
        _pad3: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab sharpen params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_3binding(
        ctx,
        &ctx.sharpen.pipeline,
        &ctx.sharpen.bind_group_layout,
        "rasterlab sharpen",
        &image.buffer,
        &output,
        &params_buffer,
        params.width,
        params.height,
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_noise_reduction_nlm(
    ctx: &GpuContext,
    op: &NoiseReductionOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let ycc_byte_len = image.width as u64 * image.height as u64 * 16;
    let denoised_ycc = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab noise_reduction_nlm ycbcr intermediate"),
        size: ycc_byte_len,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab noise_reduction_nlm output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let luma_h = (op.luma_strength * 25.0).max(1e-4);
    let color_h = (op.color_strength * 25.0).max(1e-4);
    let params = NrNlmParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        luma_h2: luma_h * luma_h,
        color_h2: color_h * color_h,
        detail: op.detail_preservation.clamp(0.0, 1.0),
        _pad2: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab noise_reduction_nlm params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let nlm_bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rasterlab noise_reduction_nlm bind group"),
        layout: &ctx.noise_reduction_nlm.nlm_bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: image.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: denoised_ycc.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });
    let detail_bind_group = ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("rasterlab noise_reduction_detail bind group"),
        layout: &ctx.noise_reduction_nlm.detail_bind_group_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: image.buffer.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: denoised_ycc.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: output.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: params_buffer.as_entire_binding(),
            },
        ],
    });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab noise_reduction_nlm encoder"),
        });
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rasterlab noise_reduction_nlm pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.noise_reduction_nlm.nlm_pipeline);
        pass.set_bind_group(0, &nlm_bind_group, &[]);
        pass.dispatch_workgroups(image.width.div_ceil(8), image.height.div_ceil(8), 1);
    }
    {
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("rasterlab noise_reduction_detail pass"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&ctx.noise_reduction_nlm.detail_pipeline);
        pass.set_bind_group(0, &detail_bind_group, &[]);
        pass.dispatch_workgroups(image.width.div_ceil(8), image.height.div_ceil(8), 1);
    }
    ctx.queue.submit(Some(encoder.finish()));
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GpuError::Poll(e.to_string()))?;

    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlackAndWhiteParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    mode: u32,
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
    rw: f32,
    gw: f32,
    bw: f32,
    _pad5: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ColorBalanceParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    cr0: f32,
    cr1: f32,
    cr2: f32,
    _pad2: f32,
    mg0: f32,
    mg1: f32,
    mg2: f32,
    _pad3: f32,
    yb0: f32,
    yb1: f32,
    yb2: f32,
    _pad4: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ColorSpaceParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    m0: f32,
    m1: f32,
    m2: f32,
    _pad2: f32,
    m3: f32,
    m4: f32,
    m5: f32,
    _pad3: f32,
    m6: f32,
    m7: f32,
    m8: f32,
    _pad4: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct DenoiseParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    radius: u32,
    sigma_r2: f32,
    sigma_s2: f32,
    _pad: f32,
    _pad2: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct HslPanelParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    hue: [f32; 8],
    sat: [f32; 8],
    lum: [f32; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlurParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    kernel_radius: u32,
    sigma: f32,
    _pad: f32,
    _pad2: f32,
    _pad3: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct SharpenParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    luminance_only: u32,
    strength: f32,
    _pad: f32,
    _pad2: f32,
    _pad3: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct FauxHdrParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    strength: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ClarityLumaParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ClarityBlurParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    radius: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct ClarityDetailParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    midtone_weight: u32,
    amount: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
}

const SRGB_TO_P3: [f32; 9] = [
    0.822_458, 0.177_542, 0.000_000, 0.033_194, 0.966_806, 0.000_000, 0.017_082, 0.072_397,
    0.910_521,
];
const P3_TO_SRGB: [f32; 9] = [
    1.224_94, -0.224_94, 0.000_00, -0.042_057, 1.042_057, 0.000_00, -0.019_637, -0.078_636,
    1.098_273,
];

fn expected_rgba_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

fn build_brightness_contrast_lut(brightness: f32, contrast: f32) -> [u8; 256] {
    let b = brightness.clamp(-1.0, 1.0) * 255.0;
    let c = contrast.clamp(-1.0, 1.0) * 255.0;
    let cf = 259.0 * (c + 255.0) / (255.0 * (259.0 - c));

    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        let x = i as f32 + b;
        let x = cf * (x - 128.0) + 128.0;
        *v = x.clamp(0.0, 255.0) as u8;
    }
    lut
}

const BRIGHTNESS_CONTRAST_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<u32>;

fn channel(byte: u32) -> u32 {
    return lut[byte] & 0xffu;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) {
        return;
    }

    let px = input_pixels[i];
    let r = channel(px & 0xffu);
    let g = channel((px >> 8u) & 0xffu);
    let b = channel((px >> 16u) & 0xffu);
    let a = px & 0xff000000u;
    output_pixels[i] = r | (g << 8u) | (b << 16u) | a;
}
"#;

const CURVES_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<u32>;

fn channel(byte: u32) -> u32 {
    return lut[byte] & 0xffu;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) {
        return;
    }

    let px = input_pixels[i];
    let r = channel(px & 0xffu);
    let g = channel((px >> 8u) & 0xffu);
    let b = channel((px >> 16u) & 0xffu);
    let a = px & 0xff000000u;
    output_pixels[i] = r | (g << 8u) | (b << 16u) | a;
}
"#;

const HUE_SHIFT_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    shift: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn hue_to_rgb(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) {
        t = t + 1.0;
    }
    if (t > 1.0) {
        t = t - 1.0;
    }
    if (t < 1.0 / 6.0) {
        return p + (q - p) * 6.0 * t;
    }
    if (t < 0.5) {
        return q;
    }
    if (t < 2.0 / 3.0) {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    return p;
}

fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let max_c = max(max(rgb.r, rgb.g), rgb.b);
    let min_c = min(min(rgb.r, rgb.g), rgb.b);
    let l = (max_c + min_c) * 0.5;

    if (abs(max_c - min_c) < 1e-9) {
        return vec3<f32>(0.0, 0.0, l);
    }

    let d = max_c - min_c;
    let s = select(d / (max_c + min_c), d / (2.0 - max_c - min_c), l > 0.5);

    var h: f32;
    if (abs(max_c - rgb.r) < 1e-9) {
        h = (rgb.g - rgb.b) / d;
        if (rgb.g < rgb.b) {
            h = h + 6.0;
        }
    } else if (abs(max_c - rgb.g) < 1e-9) {
        h = (rgb.b - rgb.r) / d + 2.0;
    } else {
        h = (rgb.r - rgb.g) / d + 4.0;
    }

    return vec3<f32>(h / 6.0, s, l);
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;
    if (s < 1e-9) {
        return vec3<f32>(l, l, l);
    }
    let q = select(l + s - l * s, l * (1.0 + s), l < 0.5);
    let p = 2.0 * l - q;
    return vec3<f32>(
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0)
    );
}

fn unpack_rgb(px: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(px & 0xffu) / 255.0,
        f32((px >> 8u) & 0xffu) / 255.0,
        f32((px >> 16u) & 0xffu) / 255.0
    );
}

fn pack_rgba(rgb: vec3<f32>, alpha: u32) -> u32 {
    let scaled = clamp(rgb * 255.0, vec3<f32>(0.0), vec3<f32>(255.0));
    let r = u32(scaled.r);
    let g = u32(scaled.g);
    let b = u32(scaled.b);
    return r | (g << 8u) | (b << 16u) | alpha;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) {
        return;
    }

    let px = input_pixels[i];
    let hsl = rgb_to_hsl(unpack_rgb(px));
    let hue = hsl.x + params.shift;
    let wrapped_hue = hue - floor(hue);
    let rgb = hsl_to_rgb(vec3<f32>(wrapped_hue, hsl.y, hsl.z));
    output_pixels[i] = pack_rgba(rgb, px & 0xff000000u);
}
"#;

const SATURATION_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    saturation: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn hue_to_rgb(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) {
        t = t + 1.0;
    }
    if (t > 1.0) {
        t = t - 1.0;
    }
    if (t < 1.0 / 6.0) {
        return p + (q - p) * 6.0 * t;
    }
    if (t < 0.5) {
        return q;
    }
    if (t < 2.0 / 3.0) {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    return p;
}

fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let max_c = max(max(rgb.r, rgb.g), rgb.b);
    let min_c = min(min(rgb.r, rgb.g), rgb.b);
    let l = (max_c + min_c) * 0.5;

    if (abs(max_c - min_c) < 1e-9) {
        return vec3<f32>(0.0, 0.0, l);
    }

    let d = max_c - min_c;
    let s = select(d / (max_c + min_c), d / (2.0 - max_c - min_c), l > 0.5);

    var h: f32;
    if (abs(max_c - rgb.r) < 1e-9) {
        h = (rgb.g - rgb.b) / d;
        if (rgb.g < rgb.b) {
            h = h + 6.0;
        }
    } else if (abs(max_c - rgb.g) < 1e-9) {
        h = (rgb.b - rgb.r) / d + 2.0;
    } else {
        h = (rgb.r - rgb.g) / d + 4.0;
    }

    return vec3<f32>(h / 6.0, s, l);
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;
    if (s < 1e-9) {
        return vec3<f32>(l, l, l);
    }
    let q = select(l + s - l * s, l * (1.0 + s), l < 0.5);
    let p = 2.0 * l - q;
    return vec3<f32>(
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0)
    );
}

fn unpack_rgb(px: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(px & 0xffu) / 255.0,
        f32((px >> 8u) & 0xffu) / 255.0,
        f32((px >> 16u) & 0xffu) / 255.0
    );
}

fn pack_rgba(rgb: vec3<f32>, alpha: u32) -> u32 {
    let scaled = clamp(rgb * 255.0, vec3<f32>(0.0), vec3<f32>(255.0));
    let r = u32(scaled.r);
    let g = u32(scaled.g);
    let b = u32(scaled.b);
    return r | (g << 8u) | (b << 16u) | alpha;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) {
        return;
    }

    let px = input_pixels[i];
    let hsl = rgb_to_hsl(unpack_rgb(px));
    let new_s = clamp(hsl.y * params.saturation, 0.0, 1.0);
    let rgb = hsl_to_rgb(vec3<f32>(hsl.x, new_s, hsl.z));
    output_pixels[i] = pack_rgba(rgb, px & 0xff000000u);
}
"#;

const VIBRANCE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    strength: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn hue_to_rgb(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) {
        t = t + 1.0;
    }
    if (t > 1.0) {
        t = t - 1.0;
    }
    if (t < 1.0 / 6.0) {
        return p + (q - p) * 6.0 * t;
    }
    if (t < 0.5) {
        return q;
    }
    if (t < 2.0 / 3.0) {
        return p + (q - p) * (2.0 / 3.0 - t) * 6.0;
    }
    return p;
}

fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let max_c = max(max(rgb.r, rgb.g), rgb.b);
    let min_c = min(min(rgb.r, rgb.g), rgb.b);
    let l = (max_c + min_c) * 0.5;

    if (abs(max_c - min_c) < 1e-9) {
        return vec3<f32>(0.0, 0.0, l);
    }

    let d = max_c - min_c;
    let s = select(d / (max_c + min_c), d / (2.0 - max_c - min_c), l > 0.5);

    var h: f32;
    if (abs(max_c - rgb.r) < 1e-9) {
        h = (rgb.g - rgb.b) / d;
        if (rgb.g < rgb.b) {
            h = h + 6.0;
        }
    } else if (abs(max_c - rgb.g) < 1e-9) {
        h = (rgb.b - rgb.r) / d + 2.0;
    } else {
        h = (rgb.r - rgb.g) / d + 4.0;
    }

    return vec3<f32>(h / 6.0, s, l);
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;
    if (s < 1e-9) {
        return vec3<f32>(l, l, l);
    }
    let q = select(l + s - l * s, l * (1.0 + s), l < 0.5);
    let p = 2.0 * l - q;
    return vec3<f32>(
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0)
    );
}

fn unpack_rgb(px: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(px & 0xffu) / 255.0,
        f32((px >> 8u) & 0xffu) / 255.0,
        f32((px >> 16u) & 0xffu) / 255.0
    );
}

fn pack_rgba(rgb: vec3<f32>, alpha: u32) -> u32 {
    let scaled = clamp(rgb * 255.0, vec3<f32>(0.0), vec3<f32>(255.0));
    let r = u32(scaled.r);
    let g = u32(scaled.g);
    let b = u32(scaled.b);
    return r | (g << 8u) | (b << 16u) | alpha;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) {
        return;
    }

    let px = input_pixels[i];
    let hsl = rgb_to_hsl(unpack_rgb(px));
    if (hsl.y < 1e-6) {
        output_pixels[i] = px;
        return;
    }

    let weight = (1.0 - hsl.y) * (1.0 - hsl.y);
    let new_s = clamp(hsl.y + params.strength * weight, 0.0, 1.0);
    let rgb = hsl_to_rgb(vec3<f32>(hsl.x, new_s, hsl.z));
    output_pixels[i] = pack_rgba(rgb, px & 0xff000000u);
}
"#;

const WHITE_BALANCE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    r_scale: f32,
    g_scale: f32,
    b_scale: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn scaled_channel(byte: u32, scale: f32) -> u32 {
    return u32(clamp(f32(byte) * scale, 0.0, 255.0));
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) {
        return;
    }

    let px = input_pixels[i];
    let r = scaled_channel(px & 0xffu, params.r_scale);
    let g = scaled_channel((px >> 8u) & 0xffu, params.g_scale);
    let b = scaled_channel((px >> 16u) & 0xffu, params.b_scale);
    let a = px & 0xff000000u;
    output_pixels[i] = r | (g << 8u) | (b << 16u) | a;
}
"#;

const SEPIA_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    strength: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu);
    let g = f32((px >> 8u) & 0xffu);
    let b = f32((px >> 16u) & 0xffu);
    let a = px & 0xff000000u;

    let sr = min(r * 0.393 + g * 0.769 + b * 0.189, 255.0);
    let sg = min(r * 0.349 + g * 0.686 + b * 0.168, 255.0);
    let sb = min(r * 0.272 + g * 0.534 + b * 0.131, 255.0);

    let s = params.strength;
    let nr = u32(r + (sr - r) * s);
    let ng = u32(g + (sg - g) * s);
    let nb = u32(b + (sb - b) * s);
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
}
"#;

const LEVELS_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<u32>;

fn channel(byte: u32) -> u32 {
    return lut[byte] & 0xffu;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = channel(px & 0xffu);
    let g = channel((px >> 8u) & 0xffu);
    let b = channel((px >> 16u) & 0xffu);
    let a = px & 0xff000000u;
    output_pixels[i] = r | (g << 8u) | (b << 16u) | a;
}
"#;

const HIGHLIGHTS_SHADOWS_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    highlights: f32,
    shadows: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let hl_weight = pow(max((luma - 0.5) * 2.0, 0.0), 2.0);
    let sh_weight = pow(max((0.5 - luma) * 2.0, 0.0), 2.0);
    let delta = params.highlights * hl_weight * 0.5 + params.shadows * sh_weight * 0.5;

    let nr = u32(clamp((r + delta) * 255.0, 0.0, 255.0));
    let ng = u32(clamp((g + delta) * 255.0, 0.0, 255.0));
    let nb = u32(clamp((b + delta) * 255.0, 0.0, 255.0));
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
}
"#;

const VIGNETTE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    strength: f32,
    inner: f32,
    zone: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

const INV_SQRT2: f32 = 0.70710678118654752;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let half_w = f32(params.width) * 0.5;
    let half_h = f32(params.height) * 0.5;
    let dx = (f32(gid.x) + 0.5 - half_w) / half_w;
    let dy = (f32(gid.y) + 0.5 - half_h) / half_h;
    let d = sqrt(dx * dx + dy * dy) * INV_SQRT2;

    let t = clamp((d - params.inner) / params.zone, 0.0, 1.0);
    let t_smooth = t * t * (3.0 - 2.0 * t);
    let factor = 1.0 - params.strength * t_smooth;

    let px = input_pixels[i];
    let r = u32(clamp(f32(px & 0xffu) * factor, 0.0, 255.0));
    let g = u32(clamp(f32((px >> 8u) & 0xffu) * factor, 0.0, 255.0));
    let b = u32(clamp(f32((px >> 16u) & 0xffu) * factor, 0.0, 255.0));
    let a = px & 0xff000000u;
    output_pixels[i] = r | (g << 8u) | (b << 16u) | a;
}
"#;

const SHADOW_EXPOSURE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    ev: f32,
    falloff: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn srgb_to_linear(c: f32) -> f32 {
    if (c <= 0.04045) {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn linear_to_srgb(c: f32) -> f32 {
    if (c <= 0.0031308) {
        return 12.92 * c;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let weight = pow(clamp(1.0 - luma, 0.0, 1.0), params.falloff);
    let gain = exp2(params.ev * weight);

    let rl = srgb_to_linear(r) * gain;
    let gl = srgb_to_linear(g) * gain;
    let bl = srgb_to_linear(b) * gain;

    let nr = u32(clamp(linear_to_srgb(rl) * 255.0, 0.0, 255.0));
    let ng = u32(clamp(linear_to_srgb(gl) * 255.0, 0.0, 255.0));
    let nb = u32(clamp(linear_to_srgb(bl) * 255.0, 0.0, 255.0));
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
}
"#;

const SPLIT_TONE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    sh_r: f32,
    sh_g: f32,
    sh_b: f32,
    shadow_sat: f32,
    hi_r: f32,
    hi_g: f32,
    hi_b: f32,
    highlight_sat: f32,
    balance: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let luma_b = clamp(luma + params.balance, 0.0, 1.0);

    let shadow_w = (1.0 - luma_b) * (1.0 - luma_b) * params.shadow_sat;
    let highlight_w = luma_b * luma_b * params.highlight_sat;

    let nr = clamp(r + (params.sh_r - r) * shadow_w + (params.hi_r - r) * highlight_w, 0.0, 1.0);
    let ng = clamp(g + (params.sh_g - g) * shadow_w + (params.hi_g - g) * highlight_w, 0.0, 1.0);
    let nb = clamp(b + (params.sh_b - b) * shadow_w + (params.hi_b - b) * highlight_w, 0.0, 1.0);

    output_pixels[i] = u32(nr * 255.0 + 0.5) | (u32(ng * 255.0 + 0.5) << 8u)
        | (u32(nb * 255.0 + 0.5) << 16u) | a;
}
"#;

const BLACK_AND_WHITE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    mode: u32,
    _pad2: u32,
    _pad3: u32,
    _pad4: u32,
    rw: f32,
    gw: f32,
    bw: f32,
    _pad5: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    var gray: f32;
    if (params.mode == 0u) {
        gray = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    } else if (params.mode == 1u) {
        gray = (r + g + b) / 3.0;
    } else if (params.mode == 2u) {
        gray = 0.299 * r + 0.587 * g + 0.114 * b;
    } else {
        gray = params.rw * r + params.gw * g + params.bw * b;
    }
    gray = clamp(gray, 0.0, 1.0);
    let out = u32(gray * 255.0 + 0.5);
    output_pixels[i] = out | (out << 8u) | (out << 16u) | a;
}
"#;

const BLUR_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    kernel_radius: u32,
    sigma: f32,
    _pad: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main_h(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    var sum_r = 0.0;
    var sum_g = 0.0;
    var sum_b = 0.0;
    var sum_a = 0.0;
    var weight_sum = 0.0;

    let sigma2 = params.sigma * params.sigma;
    let r = i32(params.kernel_radius);
    for (var ki: i32 = -r; ki <= r; ki = ki + 1) {
        let sx = clamp(i32(gid.x) + ki, 0, i32(params.width) - 1);
        let src_px = input_pixels[gid.y * params.width + u32(sx)];
        let kv = exp(-0.5 * f32(ki * ki) / sigma2);
        sum_r += kv * f32(src_px & 0xffu);
        sum_g += kv * f32((src_px >> 8u) & 0xffu);
        sum_b += kv * f32((src_px >> 16u) & 0xffu);
        sum_a += kv * f32((src_px >> 24u) & 0xffu);
        weight_sum += kv;
    }

    let nr = u32(clamp(sum_r / weight_sum, 0.0, 255.0));
    let ng = u32(clamp(sum_g / weight_sum, 0.0, 255.0));
    let nb = u32(clamp(sum_b / weight_sum, 0.0, 255.0));
    let na = u32(clamp(sum_a / weight_sum, 0.0, 255.0));
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | (na << 24u);
}

@compute @workgroup_size(16, 16)
fn main_v(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    var sum_r = 0.0;
    var sum_g = 0.0;
    var sum_b = 0.0;
    var sum_a = 0.0;
    var weight_sum = 0.0;

    let sigma2 = params.sigma * params.sigma;
    let r = i32(params.kernel_radius);
    for (var ki: i32 = -r; ki <= r; ki = ki + 1) {
        let sy = clamp(i32(gid.y) + ki, 0, i32(params.height) - 1);
        let src_px = input_pixels[u32(sy) * params.width + gid.x];
        let kv = exp(-0.5 * f32(ki * ki) / sigma2);
        sum_r += kv * f32(src_px & 0xffu);
        sum_g += kv * f32((src_px >> 8u) & 0xffu);
        sum_b += kv * f32((src_px >> 16u) & 0xffu);
        sum_a += kv * f32((src_px >> 24u) & 0xffu);
        weight_sum += kv;
    }

    let nr = u32(clamp(sum_r / weight_sum, 0.0, 255.0));
    let ng = u32(clamp(sum_g / weight_sum, 0.0, 255.0));
    let nb = u32(clamp(sum_b / weight_sum, 0.0, 255.0));
    let na = u32(clamp(sum_a / weight_sum, 0.0, 255.0));
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | (na << 24u);
}
"#;

const COLOR_BALANCE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    cr0: f32,
    cr1: f32,
    cr2: f32,
    _pad2: f32,
    mg0: f32,
    mg1: f32,
    mg2: f32,
    _pad3: f32,
    yb0: f32,
    yb1: f32,
    yb2: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let sh = (1.0 - luma) * (1.0 - luma);
    let mt = 4.0 * luma * (1.0 - luma);
    let hl = luma * luma;

    let dr = (params.cr0 * sh + params.cr1 * mt + params.cr2 * hl) * 0.4;
    let dg = (params.mg0 * sh + params.mg1 * mt + params.mg2 * hl) * 0.4;
    let db = (params.yb0 * sh + params.yb1 * mt + params.yb2 * hl) * 0.4;

    let nr = u32(clamp((r + dr) * 255.0, 0.0, 255.0));
    let ng = u32(clamp((g + dg) * 255.0, 0.0, 255.0));
    let nb = u32(clamp((b + db) * 255.0, 0.0, 255.0));
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
}
"#;

const COLOR_SPACE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    m0: f32,
    m1: f32,
    m2: f32,
    _pad2: f32,
    m3: f32,
    m4: f32,
    m5: f32,
    _pad3: f32,
    m6: f32,
    m7: f32,
    m8: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn srgb_to_linear(c: f32) -> f32 {
    if (c <= 0.04045) {
        return c / 12.92;
    }
    return pow((c + 0.055) / 1.055, 2.4);
}

fn linear_to_srgb(c: f32) -> f32 {
    if (c <= 0.0031308) {
        return 12.92 * c;
    }
    return 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let rl = srgb_to_linear(r);
    let gl = srgb_to_linear(g);
    let bl = srgb_to_linear(b);

    let out_rl = clamp(params.m0 * rl + params.m1 * gl + params.m2 * bl, 0.0, 1.0);
    let out_gl = clamp(params.m3 * rl + params.m4 * gl + params.m5 * bl, 0.0, 1.0);
    let out_bl = clamp(params.m6 * rl + params.m7 * gl + params.m8 * bl, 0.0, 1.0);

    let nr = u32(clamp(linear_to_srgb(out_rl) * 255.0, 0.0, 255.0));
    let ng = u32(clamp(linear_to_srgb(out_gl) * 255.0, 0.0, 255.0));
    let nb = u32(clamp(linear_to_srgb(out_bl) * 255.0, 0.0, 255.0));
    output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
}
"#;

const DENOISE_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    radius: u32,
    sigma_r2: f32,
    sigma_s2: f32,
    _pad: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let cr = f32(px & 0xffu);
    let cg = f32((px >> 8u) & 0xffu);
    let cb = f32((px >> 16u) & 0xffu);
    let a = px & 0xff000000u;

    var sum_r = 0.0;
    var sum_g = 0.0;
    var sum_b = 0.0;
    var sum_w = 0.0;

    for (var dy: i32 = -i32(params.radius); dy <= i32(params.radius); dy = dy + 1) {
        for (var dx: i32 = -i32(params.radius); dx <= i32(params.radius); dx = dx + 1) {
            let nx = clamp(i32(gid.x) + dx, 0, i32(params.width) - 1);
            let ny = clamp(i32(gid.y) + dy, 0, i32(params.height) - 1);
            let npx = input_pixels[u32(ny) * params.width + u32(nx)];
            let nr = f32(npx & 0xffu);
            let ng = f32((npx >> 8u) & 0xffu);
            let nb = f32((npx >> 16u) & 0xffu);

            let spatial_d = f32(dx * dx + dy * dy);
            let s_w = exp(-spatial_d / params.sigma_s2);

            let dr = nr - cr;
            let dg = ng - cg;
            let db = nb - cb;
            let color_d = dr * dr + dg * dg + db * db;
            let r_w = exp(-color_d / params.sigma_r2);

            let w = s_w * r_w;
            sum_r += w * nr;
            sum_g += w * ng;
            sum_b += w * nb;
            sum_w += w;
        }
    }

    if (sum_w > 1e-9) {
        let out_r = u32(clamp(sum_r / sum_w, 0.0, 255.0));
        let out_g = u32(clamp(sum_g / sum_w, 0.0, 255.0));
        let out_b = u32(clamp(sum_b / sum_w, 0.0, 255.0));
        output_pixels[i] = out_r | (out_g << 8u) | (out_b << 16u) | a;
    } else {
        output_pixels[i] = px;
    }
}
"#;

const HSL_PANEL_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    hue: array<f32, 8>,
    sat: array<f32, 8>,
    lum: array<f32, 8>,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn hue_to_rgb(p: f32, q: f32, t_in: f32) -> f32 {
    var t = t_in;
    if (t < 0.0) { t = t + 1.0; }
    if (t > 1.0) { t = t - 1.0; }
    if (t < 1.0 / 6.0) { return p + (q - p) * 6.0 * t; }
    if (t < 0.5) { return q; }
    if (t < 2.0 / 3.0) { return p + (q - p) * (2.0 / 3.0 - t) * 6.0; }
    return p;
}

fn rgb_to_hsl(rgb: vec3<f32>) -> vec3<f32> {
    let max_c = max(max(rgb.r, rgb.g), rgb.b);
    let min_c = min(min(rgb.r, rgb.g), rgb.b);
    let l = (max_c + min_c) * 0.5;
    if (abs(max_c - min_c) < 1e-9) { return vec3<f32>(0.0, 0.0, l); }
    let d = max_c - min_c;
    let s = select(d / (max_c + min_c), d / (2.0 - max_c - min_c), l > 0.5);
    var h: f32;
    if (abs(max_c - rgb.r) < 1e-9) {
        h = (rgb.g - rgb.b) / d;
        if (rgb.g < rgb.b) { h = h + 6.0; }
    } else if (abs(max_c - rgb.g) < 1e-9) {
        h = (rgb.b - rgb.r) / d + 2.0;
    } else {
        h = (rgb.r - rgb.g) / d + 4.0;
    }
    return vec3<f32>(h / 6.0, s, l);
}

fn hsl_to_rgb(hsl: vec3<f32>) -> vec3<f32> {
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;
    if (s < 1e-9) { return vec3<f32>(l, l, l); }
    let q = select(l + s - l * s, l * (1.0 + s), l < 0.5);
    let p = 2.0 * l - q;
    return vec3<f32>(
        hue_to_rgb(p, q, h + 1.0 / 3.0),
        hue_to_rgb(p, q, h),
        hue_to_rgb(p, q, h - 1.0 / 3.0)
    );
}

fn unpack_rgb(px: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(px & 0xffu) / 255.0,
        f32((px >> 8u) & 0xffu) / 255.0,
        f32((px >> 16u) & 0xffu) / 255.0
    );
}

fn pack_rgba(rgb: vec3<f32>, alpha: u32) -> u32 {
    let scaled = clamp(rgb * 255.0, vec3<f32>(0.0), vec3<f32>(255.0));
    let r = u32(scaled.r);
    let g = u32(scaled.g);
    let b = u32(scaled.b);
    return r | (g << 8u) | (b << 16u) | alpha;
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let hsl = rgb_to_hsl(unpack_rgb(px));
    let h = hsl.x;
    let s = hsl.y;
    let l = hsl.z;

    let centres = array<f32, 8>(0.0, 0.125, 0.25, 0.375, 0.5, 0.625, 0.75, 0.875);
    let half_width = 0.125;

    var dh = 0.0;
    var ds = 0.0;
    var dl = 0.0;
    var w_sum = 0.0;

    for (var bi: i32 = 0; bi < 8; bi = bi + 1) {
        let centre = centres[bi];
        let raw_d = abs(h - centre);
        let d = select(raw_d, 1.0 - raw_d, raw_d > 0.5);
        let w = max(0.0, 1.0 - d / half_width);
        dh += w * params.hue[bi];
        ds += w * params.sat[bi];
        dl += w * params.lum[bi];
        w_sum += w;
    }

    if (w_sum < 1e-6) {
        output_pixels[i] = px;
        return;
    }

    let new_h = fract(h + dh / (360.0 * w_sum));
    let new_s = clamp(s + ds / w_sum, 0.0, 1.0);
    let new_l = clamp(l + dl / w_sum, 0.0, 1.0);
    let rgb = hsl_to_rgb(vec3<f32>(new_h, new_s, new_l));
    output_pixels[i] = pack_rgba(rgb, px & 0xff000000u);
}
"#;

const SHARPEN_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    luminance_only: u32,
    strength: f32,
    _pad: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

fn read_pixel(x: i32, y: i32) -> u32 {
    let cx = u32(clamp(x, 0, i32(params.width) - 1));
    let cy = u32(clamp(y, 0, i32(params.height) - 1));
    return input_pixels[cy * params.width + cx];
}

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let xi = i32(gid.x);
    let yi = i32(gid.y);

    let c_px = read_pixel(xi, yi);
    let t_px = read_pixel(xi, yi - 1);
    let b_px = read_pixel(xi, yi + 1);
    let l_px = read_pixel(xi - 1, yi);
    let r_px = read_pixel(xi + 1, yi);

    let a = c_px & 0xff000000u;
    let s = params.strength;

    if (params.luminance_only == 0u) {
        let c_r = f32(c_px & 0xffu);
        let c_g = f32((c_px >> 8u) & 0xffu);
        let c_b = f32((c_px >> 16u) & 0xffu);

        let t_r = f32(t_px & 0xffu);
        let t_g = f32((t_px >> 8u) & 0xffu);
        let t_b = f32((t_px >> 16u) & 0xffu);

        let b_r = f32(b_px & 0xffu);
        let b_g = f32((b_px >> 8u) & 0xffu);
        let b_b = f32((b_px >> 16u) & 0xffu);

        let l_r = f32(l_px & 0xffu);
        let l_g = f32((l_px >> 8u) & 0xffu);
        let l_b = f32((l_px >> 16u) & 0xffu);

        let r_r = f32(r_px & 0xffu);
        let r_g = f32((r_px >> 8u) & 0xffu);
        let r_b = f32((r_px >> 16u) & 0xffu);

        let nr = u32(clamp((1.0 + 4.0 * s) * c_r - s * (t_r + b_r + l_r + r_r), 0.0, 255.0));
        let ng = u32(clamp((1.0 + 4.0 * s) * c_g - s * (t_g + b_g + l_g + r_g), 0.0, 255.0));
        let nb = u32(clamp((1.0 + 4.0 * s) * c_b - s * (t_b + b_b + l_b + r_b), 0.0, 255.0));
        output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
    } else {
        let c_r = f32(c_px & 0xffu);
        let c_g = f32((c_px >> 8u) & 0xffu);
        let c_b = f32((c_px >> 16u) & 0xffu);
        let luma_c = 0.2126 * c_r + 0.7152 * c_g + 0.0722 * c_b;

        let t_r = f32(t_px & 0xffu);
        let t_g = f32((t_px >> 8u) & 0xffu);
        let t_b = f32((t_px >> 16u) & 0xffu);
        let luma_t = 0.2126 * t_r + 0.7152 * t_g + 0.0722 * t_b;

        let b_r = f32(b_px & 0xffu);
        let b_g = f32((b_px >> 8u) & 0xffu);
        let b_b = f32((b_px >> 16u) & 0xffu);
        let luma_b = 0.2126 * b_r + 0.7152 * b_g + 0.0722 * b_b;

        let l_r = f32(l_px & 0xffu);
        let l_g = f32((l_px >> 8u) & 0xffu);
        let l_b = f32((l_px >> 16u) & 0xffu);
        let luma_l = 0.2126 * l_r + 0.7152 * l_g + 0.0722 * l_b;

        let r_r = f32(r_px & 0xffu);
        let r_g = f32((r_px >> 8u) & 0xffu);
        let r_b = f32((r_px >> 16u) & 0xffu);
        let luma_r = 0.2126 * r_r + 0.7152 * r_g + 0.0722 * r_b;

        let sharpened_luma = clamp((1.0 + 4.0 * s) * luma_c - s * (luma_t + luma_b + luma_l + luma_r), 0.0, 255.0);
        let delta = sharpened_luma - luma_c;

        let nr = u32(clamp(c_r + delta, 0.0, 255.0));
        let ng = u32(clamp(c_g + delta, 0.0, 255.0));
        let nb = u32(clamp(c_b + delta, 0.0, 255.0));
        output_pixels[i] = nr | (ng << 8u) | (nb << 16u) | a;
    }
}
"#;

const NOISE_REDUCTION_NLM_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    luma_h2: f32,
    color_h2: f32,
    detail: f32,
    _pad2: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> denoised_ycc: array<vec4<f32>>;
@group(0) @binding(2) var<uniform> params: Params;

fn clamp_coord(v: i32, hi: u32) -> u32 {
    return u32(clamp(v, 0, i32(hi) - 1));
}

fn pixel_at(x: u32, y: u32) -> u32 {
    return input_pixels[y * params.width + x];
}

fn unpack_rgb(px: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(px & 0xffu),
        f32((px >> 8u) & 0xffu),
        f32((px >> 16u) & 0xffu)
    );
}

fn rgb_to_ycbcr(rgb: vec3<f32>) -> vec3<f32> {
    let y = 0.299 * rgb.r + 0.587 * rgb.g + 0.114 * rgb.b;
    let cb = -0.16874 * rgb.r - 0.33126 * rgb.g + 0.5 * rgb.b + 128.0;
    let cr = 0.5 * rgb.r - 0.41869 * rgb.g - 0.08131 * rgb.b + 128.0;
    return vec3<f32>(y, cb, cr);
}

fn ycbcr_at(x: u32, y: u32) -> vec3<f32> {
    return rgb_to_ycbcr(unpack_rgb(pixel_at(x, y)));
}

fn ycbcr_to_rgb(ycc: vec3<f32>) -> vec3<f32> {
    let y = ycc.x;
    let cb = ycc.y;
    let cr = ycc.z;
    return clamp(vec3<f32>(
        y + 1.402 * (cr - 128.0),
        y - 0.34414 * (cb - 128.0) - 0.71414 * (cr - 128.0),
        y + 1.772 * (cb - 128.0)
    ), vec3<f32>(0.0), vec3<f32>(255.0));
}

fn pack_rgba(rgb: vec3<f32>, alpha: u32) -> u32 {
    let r = u32(clamp(rgb.r, 0.0, 255.0));
    let g = u32(clamp(rgb.g, 0.0, 255.0));
    let b = u32(clamp(rgb.b, 0.0, 255.0));
    return r | (g << 8u) | (b << 16u) | alpha;
}

@compute @workgroup_size(8, 8)
fn nlm_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) {
        return;
    }

    let px = gid.x;
    let py = gid.y;
    let src = pixel_at(px, py);
    let patch_norm = 1.0 / 49.0;

    var sum_wy = 0.0;
    var sum_wc = 0.0;
    var acc_y = 0.0;
    var acc_cb = 0.0;
    var acc_cr = 0.0;

    let qy_lo = max(i32(py) - 7, 0);
    let qy_hi = min(i32(py) + 7, i32(params.height) - 1);
    let qx_lo = max(i32(px) - 7, 0);
    let qx_hi = min(i32(px) + 7, i32(params.width) - 1);

    for (var qyi = qy_lo; qyi <= qy_hi; qyi = qyi + 1) {
        for (var qxi = qx_lo; qxi <= qx_hi; qxi = qxi + 1) {
            let qx = u32(qxi);
            let qy = u32(qyi);
            var dist_y = 0.0;
            var dist_c = 0.0;

            for (var dy = -3; dy <= 3; dy = dy + 1) {
                for (var dx = -3; dx <= 3; dx = dx + 1) {
                    let pr = clamp_coord(i32(py) + dy, params.height);
                    let pc = clamp_coord(i32(px) + dx, params.width);
                    let qr = clamp_coord(i32(qy) + dy, params.height);
                    let qc = clamp_coord(i32(qx) + dx, params.width);

                    let p_ycc = ycbcr_at(pc, pr);
                    let q_ycc = ycbcr_at(qc, qr);
                    let dy_val = p_ycc.x - q_ycc.x;
                    dist_y = dist_y + dy_val * dy_val;

                    let dcb = p_ycc.y - q_ycc.y;
                    let dcr = p_ycc.z - q_ycc.z;
                    dist_c = dist_c + dcb * dcb + dcr * dcr;
                }
            }

            dist_y = dist_y * patch_norm;
            dist_c = dist_c * patch_norm;

            let wy = exp(-dist_y / max(params.luma_h2, 1e-9));
            let wc = exp(-dist_c / max(params.color_h2, 1e-9));
            let q_ycc = ycbcr_at(qx, qy);

            acc_y = acc_y + wy * q_ycc.x;
            sum_wy = sum_wy + wy;

            acc_cb = acc_cb + wc * q_ycc.y;
            acc_cr = acc_cr + wc * q_ycc.z;
            sum_wc = sum_wc + wc;
        }
    }

    let orig_ycc = ycbcr_at(px, py);
    let out_ycc = vec3<f32>(
        select(orig_ycc.x, acc_y / sum_wy, sum_wy > 1e-9),
        select(orig_ycc.y, acc_cb / sum_wc, sum_wc > 1e-9),
        select(orig_ycc.z, acc_cr / sum_wc, sum_wc > 1e-9)
    );

    denoised_ycc[py * params.width + px] = vec4<f32>(out_ycc, 0.0);
}

@group(0) @binding(0) var<storage, read> detail_input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read> detail_denoised_ycc: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(3) var<uniform> detail_params: Params;

fn detail_clamp_coord(v: i32, hi: u32) -> u32 {
    return u32(clamp(v, 0, i32(hi) - 1));
}

fn detail_pixel_at(x: u32, y: u32) -> u32 {
    return detail_input_pixels[y * detail_params.width + x];
}

fn detail_unpack_rgb(px: u32) -> vec3<f32> {
    return vec3<f32>(
        f32(px & 0xffu),
        f32((px >> 8u) & 0xffu),
        f32((px >> 16u) & 0xffu)
    );
}

fn detail_rgb_to_ycbcr(rgb: vec3<f32>) -> vec3<f32> {
    let y = 0.299 * rgb.r + 0.587 * rgb.g + 0.114 * rgb.b;
    let cb = -0.16874 * rgb.r - 0.33126 * rgb.g + 0.5 * rgb.b + 128.0;
    let cr = 0.5 * rgb.r - 0.41869 * rgb.g - 0.08131 * rgb.b + 128.0;
    return vec3<f32>(y, cb, cr);
}

fn detail_orig_ycc_at(x: u32, y: u32) -> vec3<f32> {
    return detail_rgb_to_ycbcr(detail_unpack_rgb(detail_pixel_at(x, y)));
}

fn denoised_at(x: u32, y: u32) -> vec3<f32> {
    return detail_denoised_ycc[y * detail_params.width + x].xyz;
}

fn denoised_y_at_i(r: i32, c: i32) -> f32 {
    let y = detail_clamp_coord(r, detail_params.height);
    let x = detail_clamp_coord(c, detail_params.width);
    return denoised_at(x, y).x;
}

fn detail_ycbcr_to_rgb(ycc: vec3<f32>) -> vec3<f32> {
    let y = ycc.x;
    let cb = ycc.y;
    let cr = ycc.z;
    return clamp(vec3<f32>(
        y + 1.402 * (cr - 128.0),
        y - 0.34414 * (cb - 128.0) - 0.71414 * (cr - 128.0),
        y + 1.772 * (cb - 128.0)
    ), vec3<f32>(0.0), vec3<f32>(255.0));
}

fn detail_pack_rgba(rgb: vec3<f32>, alpha: u32) -> u32 {
    let r = u32(clamp(rgb.r, 0.0, 255.0));
    let g = u32(clamp(rgb.g, 0.0, 255.0));
    let b = u32(clamp(rgb.b, 0.0, 255.0));
    return r | (g << 8u) | (b << 16u) | alpha;
}

@compute @workgroup_size(8, 8)
fn detail_main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= detail_params.width || gid.y >= detail_params.height) {
        return;
    }

    let px = gid.x;
    let py = gid.y;
    let r = i32(py);
    let c = i32(px);
    let gx = -denoised_y_at_i(r - 1, c - 1) + denoised_y_at_i(r - 1, c + 1)
        - 2.0 * denoised_y_at_i(r, c - 1) + 2.0 * denoised_y_at_i(r, c + 1)
        - denoised_y_at_i(r + 1, c - 1) + denoised_y_at_i(r + 1, c + 1);
    let gy = -denoised_y_at_i(r - 1, c - 1) - 2.0 * denoised_y_at_i(r - 1, c)
        - denoised_y_at_i(r - 1, c + 1) + denoised_y_at_i(r + 1, c - 1)
        + 2.0 * denoised_y_at_i(r + 1, c) + denoised_y_at_i(r + 1, c + 1);
    let grad = sqrt(gx * gx + gy * gy);
    let mask = clamp(grad / 128.0, 0.0, 1.0) * clamp(detail_params.detail, 0.0, 1.0);

    let orig_ycc = detail_orig_ycc_at(px, py);
    let out_ycc = denoised_at(px, py);
    let masked_ycc = out_ycc + mask * (orig_ycc - out_ycc);

    let rgb = detail_ycbcr_to_rgb(masked_ycc);
    output_pixels[py * detail_params.width + px] =
        detail_pack_rgba(rgb, detail_pixel_at(px, py) & 0xff000000u);
}
"#;

fn apply_faux_hdr(ctx: &GpuContext, op: &FauxHdrOp, image: GpuImage) -> Result<GpuImage, GpuError> {
    if op.strength < 1e-5 {
        return Ok(image);
    }
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab faux_hdr output"),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let params = FauxHdrParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        strength: op.strength,
        _pad2: 0.0,
        _pad3: 0.0,
        _pad4: 0.0,
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab faux_hdr params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_3binding(
        ctx,
        &ctx.faux_hdr.pipeline,
        &ctx.faux_hdr.bind_group_layout,
        "rasterlab faux_hdr",
        &image.buffer,
        &output,
        &params_buffer,
        params.width,
        params.height,
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

#[allow(clippy::too_many_arguments)]
fn encode_clarity_3binding(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    layout: &wgpu::BindGroupLayout,
    label: &str,
    b0: &wgpu::Buffer,
    b1: &wgpu::Buffer,
    b2: &wgpu::Buffer,
    width: u32,
    height: u32,
) {
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: b0.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: b1.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: b2.as_entire_binding(),
            },
        ],
    });
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some(label),
        timestamp_writes: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, &bg, &[]);
    pass.dispatch_workgroups(
        width.div_ceil(WORKGROUP_SIZE_X),
        height.div_ceil(WORKGROUP_SIZE_Y),
        1,
    );
}

#[allow(clippy::too_many_arguments)]
fn encode_clarity_4binding(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    pipeline: &wgpu::ComputePipeline,
    layout: &wgpu::BindGroupLayout,
    label: &str,
    b0: &wgpu::Buffer,
    b1: &wgpu::Buffer,
    b2: &wgpu::Buffer,
    b3: &wgpu::Buffer,
    width: u32,
    height: u32,
) {
    let bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: b0.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: b1.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: b2.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 3,
                resource: b3.as_entire_binding(),
            },
        ],
    });
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some(label),
        timestamp_writes: None,
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, &bg, &[]);
    pass.dispatch_workgroups(
        width.div_ceil(WORKGROUP_SIZE_X),
        height.div_ceil(WORKGROUP_SIZE_Y),
        1,
    );
}

fn apply_clarity_texture(
    ctx: &GpuContext,
    op: &ClarityTextureOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if op.clarity == 0.0 && op.texture == 0.0 {
        return Ok(image);
    }

    let w = image.width;
    let h = image.height;
    let min_dim = w.min(h);
    let pixel_count = w.saturating_mul(h);
    let rgba_byte_len = expected_rgba_len(w, h) as u64;
    let luma_byte_len = pixel_count as u64 * 4;

    let clarity_radius = ((min_dim as f32 * 0.03).round() as u32).max(2);
    let texture_radius = ((min_dim as f32 * 0.005).round() as u32).max(1);

    let do_clarity = op.clarity != 0.0;
    let do_texture = op.texture != 0.0;

    let luma_a = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab clarity luma_a"),
        size: luma_byte_len,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let luma_b = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab clarity luma_b"),
        size: luma_byte_len,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });

    // Intermediate RGBA buffer written by clarity pass.
    // Has COPY_SRC when clarity is the final (only) stage.
    let intermediate = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab clarity intermediate rgba"),
        size: rgba_byte_len,
        usage: if do_texture {
            wgpu::BufferUsages::STORAGE
        } else {
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC
        },
        mapped_at_creation: false,
    });

    // Final output buffer (needed only when texture pass is active).
    let output = if do_texture {
        Some(ctx.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("rasterlab clarity output rgba"),
            size: rgba_byte_len,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        }))
    } else {
        None
    };

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab clarity encoder"),
        });

    let luma_params_buf = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab clarity luma params"),
            contents: bytemuck::bytes_of(&ClarityLumaParams {
                width: w,
                height: h,
                pixel_count,
                _pad: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });

    if do_clarity {
        let blur_params_buf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("rasterlab clarity blur params"),
                contents: bytemuck::bytes_of(&ClarityBlurParams {
                    width: w,
                    height: h,
                    pixel_count,
                    radius: clarity_radius,
                }),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let detail_params_buf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("rasterlab clarity detail params"),
                contents: bytemuck::bytes_of(&ClarityDetailParams {
                    width: w,
                    height: h,
                    pixel_count,
                    midtone_weight: 1,
                    amount: op.clarity,
                    _pad1: 0.0,
                    _pad2: 0.0,
                    _pad3: 0.0,
                }),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        // Extract luma from input pixels → luma_a
        encode_clarity_3binding(
            &ctx.device,
            &mut encoder,
            &ctx.clarity_texture.extract_luma_pipeline,
            &ctx.clarity_texture.three_bind_layout,
            "clarity extract_luma",
            &image.buffer,
            &luma_a,
            &luma_params_buf,
            w,
            h,
        );

        // 3 passes of box blur (H then V), ping-pong luma_a ↔ luma_b
        // After 3 full H+V passes the result is back in luma_a
        for _ in 0..3 {
            encode_clarity_3binding(
                &ctx.device,
                &mut encoder,
                &ctx.clarity_texture.box_blur_h_pipeline,
                &ctx.clarity_texture.three_bind_layout,
                "clarity box_blur_h",
                &luma_a,
                &luma_b,
                &blur_params_buf,
                w,
                h,
            );
            encode_clarity_3binding(
                &ctx.device,
                &mut encoder,
                &ctx.clarity_texture.box_blur_v_pipeline,
                &ctx.clarity_texture.three_bind_layout,
                "clarity box_blur_v",
                &luma_b,
                &luma_a,
                &blur_params_buf,
                w,
                h,
            );
        }

        // Apply clarity detail: input rgba + blurred luma (luma_a) → intermediate
        encode_clarity_4binding(
            &ctx.device,
            &mut encoder,
            &ctx.clarity_texture.apply_detail_pipeline,
            &ctx.clarity_texture.four_bind_layout,
            "clarity apply_detail",
            &image.buffer,
            &intermediate,
            &detail_params_buf,
            &luma_a,
            w,
            h,
        );
    }

    if do_texture {
        let output_buf = output.as_ref().unwrap();
        let src = if do_clarity {
            &intermediate
        } else {
            &image.buffer
        };

        let blur_params_buf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("rasterlab texture blur params"),
                contents: bytemuck::bytes_of(&ClarityBlurParams {
                    width: w,
                    height: h,
                    pixel_count,
                    radius: texture_radius,
                }),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let detail_params_buf = ctx
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("rasterlab texture detail params"),
                contents: bytemuck::bytes_of(&ClarityDetailParams {
                    width: w,
                    height: h,
                    pixel_count,
                    midtone_weight: 0,
                    amount: op.texture,
                    _pad1: 0.0,
                    _pad2: 0.0,
                    _pad3: 0.0,
                }),
                usage: wgpu::BufferUsages::UNIFORM,
            });

        // Extract luma from post-clarity pixels → luma_a
        encode_clarity_3binding(
            &ctx.device,
            &mut encoder,
            &ctx.clarity_texture.extract_luma_pipeline,
            &ctx.clarity_texture.three_bind_layout,
            "texture extract_luma",
            src,
            &luma_a,
            &luma_params_buf,
            w,
            h,
        );

        for _ in 0..3 {
            encode_clarity_3binding(
                &ctx.device,
                &mut encoder,
                &ctx.clarity_texture.box_blur_h_pipeline,
                &ctx.clarity_texture.three_bind_layout,
                "texture box_blur_h",
                &luma_a,
                &luma_b,
                &blur_params_buf,
                w,
                h,
            );
            encode_clarity_3binding(
                &ctx.device,
                &mut encoder,
                &ctx.clarity_texture.box_blur_v_pipeline,
                &ctx.clarity_texture.three_bind_layout,
                "texture box_blur_v",
                &luma_b,
                &luma_a,
                &blur_params_buf,
                w,
                h,
            );
        }

        // Apply texture detail: src rgba + blurred luma → output
        encode_clarity_4binding(
            &ctx.device,
            &mut encoder,
            &ctx.clarity_texture.apply_detail_pipeline,
            &ctx.clarity_texture.four_bind_layout,
            "texture apply_detail",
            src,
            output_buf,
            &detail_params_buf,
            &luma_a,
            w,
            h,
        );
    }

    ctx.queue.submit(Some(encoder.finish()));
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GpuError::Poll(e.to_string()))?;

    let final_buf = if do_texture {
        output.unwrap()
    } else {
        intermediate
    };

    Ok(GpuImage {
        width: w,
        height: h,
        buffer: final_buf,
    })
}

const FAUX_HDR_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    strength: f32,
    _pad2: f32,
    _pad3: f32,
    _pad4: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let luma_over = min(luma * 2.0, 1.0);
    let luma_under = luma * 0.5;

    // well-exposedness: exp(-0.5 * ((luma - 0.5) / 0.35)^2)
    let inv_sigma2 = 1.0 / (2.0 * 0.35 * 0.35);
    let dv0 = luma_over - 0.5;
    let dv1 = luma - 0.5;
    let dv2 = luma_under - 0.5;
    let w0 = exp(-dv0 * dv0 * inv_sigma2);
    let w1 = exp(-dv1 * dv1 * inv_sigma2);
    let w2 = exp(-dv2 * dv2 * inv_sigma2);
    let wsum = w0 + w1 + w2 + 1e-6;

    let luma_fused = (w0 * luma_over + w1 * luma + w2 * luma_under) / wsum;

    var scale = 1.0;
    if (luma > 1e-6) {
        scale = min(luma_fused / luma, 4.0);
    }

    let s = params.strength;
    let nr = clamp(r + (r * scale - r) * s, 0.0, 1.0);
    let ng = clamp(g + (g * scale - g) * s, 0.0, 1.0);
    let nb = clamp(b + (b * scale - b) * s, 0.0, 1.0);

    output_pixels[i] = u32(nr * 255.0 + 0.5) | (u32(ng * 255.0 + 0.5) << 8u)
        | (u32(nb * 255.0 + 0.5) << 16u) | a;
}
"#;

const CLARITY_EXTRACT_LUMA_WGSL: &str = r#"
struct Params { width: u32, height: u32, pixel_count: u32, _pad: u32 };

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> luma: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }
    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    luma[i] = 0.2126 * r + 0.7152 * g + 0.0722 * b;
}
"#;

const CLARITY_BOX_BLUR_H_WGSL: &str = r#"
struct Params { width: u32, height: u32, pixel_count: u32, radius: u32 };

@group(0) @binding(0) var<storage, read> input_luma: array<f32>;
@group(0) @binding(1) var<storage, read_write> output_luma: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let r = params.radius;
    let x0 = u32(max(i32(gid.x) - i32(r), 0));
    let x1 = min(gid.x + r, params.width - 1u);
    var sum = 0.0;
    for (var x = x0; x <= x1; x += 1u) {
        sum += input_luma[gid.y * params.width + x];
    }
    output_luma[gid.y * params.width + gid.x] = sum / f32(x1 - x0 + 1u);
}
"#;

const CLARITY_BOX_BLUR_V_WGSL: &str = r#"
struct Params { width: u32, height: u32, pixel_count: u32, radius: u32 };

@group(0) @binding(0) var<storage, read> input_luma: array<f32>;
@group(0) @binding(1) var<storage, read_write> output_luma: array<f32>;
@group(0) @binding(2) var<uniform> params: Params;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let r = params.radius;
    let y0 = u32(max(i32(gid.y) - i32(r), 0));
    let y1 = min(gid.y + r, params.height - 1u);
    var sum = 0.0;
    for (var y = y0; y <= y1; y += 1u) {
        sum += input_luma[y * params.width + gid.x];
    }
    output_luma[gid.y * params.width + gid.x] = sum / f32(y1 - y0 + 1u);
}
"#;

const CLARITY_APPLY_DETAIL_WGSL: &str = r#"
struct Params {
    width: u32,
    height: u32,
    pixel_count: u32,
    midtone_weight: u32,
    amount: f32,
    _pad1: f32,
    _pad2: f32,
    _pad3: f32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> blurred_luma: array<f32>;

@compute @workgroup_size(16, 16)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    if (gid.x >= params.width || gid.y >= params.height) { return; }
    let i = gid.y * params.width + gid.x;
    if (i >= params.pixel_count) { return; }

    let px = input_pixels[i];
    let r = f32(px & 0xffu) / 255.0;
    let g = f32((px >> 8u) & 0xffu) / 255.0;
    let b = f32((px >> 16u) & 0xffu) / 255.0;
    let a = px & 0xff000000u;

    let l = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let detail = l - blurred_luma[i];
    let weight = select(1.0, 4.0 * l * (1.0 - l), params.midtone_weight != 0u);
    let boost = params.amount * detail * weight;

    let nr = clamp(r + boost, 0.0, 1.0);
    let ng = clamp(g + boost, 0.0, 1.0);
    let nb = clamp(b + boost, 0.0, 1.0);

    output_pixels[i] = u32(nr * 255.0 + 0.5) | (u32(ng * 255.0 + 0.5) << 8u)
        | (u32(nb * 255.0 + 0.5) << 16u) | a;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use rasterlab_core::traits::operation::Operation;

    async fn make_context() -> Option<GpuContext> {
        let instance = wgpu::Instance::default();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            })
            .await
            .ok()?;
        let limits = adapter.limits();
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("rasterlab gpu test device"),
                required_limits: limits.clone(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some(GpuContext::new(device, queue, limits))
    }

    fn test_image(width: u32, height: u32) -> Image {
        let mut image = Image::new(width, height);
        for (i, pixel) in image.data.chunks_mut(4).enumerate() {
            pixel[0] = (i * 3 % 256) as u8;
            pixel[1] = (i * 5 % 256) as u8;
            pixel[2] = (i * 7 % 256) as u8;
            pixel[3] = (31 + i * 11 % 225) as u8;
        }
        image
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn rgba8_upload_readback_exact() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let image = test_image(5, 3);
        let gpu = GpuImage::from_image(&ctx, &image).unwrap();
        let out = gpu.read_rgba8(&ctx).unwrap();
        assert_eq!(out, image.data);
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn brightness_contrast_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let cases = [
            (0.0, 0.0, 8, 8),
            (0.25, 0.0, 13, 17),
            (-0.25, 0.0, 13, 17),
            (0.0, 0.45, 19, 11),
            (0.0, -0.45, 19, 11),
            (0.15, -0.2, 257, 3),
        ];
        for (brightness, contrast, width, height) in cases {
            let src = test_image(width, height);
            let op = BrightnessContrastOp::new(brightness, contrast);
            let expected = op.apply(src.deep_clone()).unwrap();
            let gpu = GpuImage::from_image(&ctx, &src).unwrap();
            let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
            assert_eq!(actual.data, expected.data);
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn curves_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let cases = [
            vec![[0.0, 0.0], [1.0, 1.0]],
            vec![[0.0, 1.0], [1.0, 0.0]],
            vec![[0.0, 0.0], [0.35, 0.2], [0.7, 0.86], [1.0, 1.0]],
            vec![[0.0, 0.08], [0.18, 0.12], [0.62, 0.74], [1.0, 0.95]],
        ];
        for points in cases {
            let src = test_image(31, 17);
            let op = CurvesOp { points };
            let expected = op.apply(src.deep_clone()).unwrap();
            let gpu = GpuImage::from_image(&ctx, &src).unwrap();
            let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
            assert_eq!(actual.data, expected.data);
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn hue_shift_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        for degrees in [0.0, 30.0, -75.0, 120.0, 180.0, 270.0] {
            let src = test_image(31, 17);
            let op = HueShiftOp::new(degrees);
            let expected = op.apply(src.deep_clone()).unwrap();
            let gpu = GpuImage::from_image(&ctx, &src).unwrap();
            let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
            assert_eq!(actual.data, expected.data, "degrees={degrees}");
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn saturation_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        for saturation in [0.0, 0.35, 1.0, 1.75, 4.0] {
            let src = test_image(31, 17);
            let op = SaturationOp::new(saturation);
            let expected = op.apply(src.deep_clone()).unwrap();
            let gpu = GpuImage::from_image(&ctx, &src).unwrap();
            let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
            assert_eq!(actual.data, expected.data, "saturation={saturation}");
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn vibrance_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        for strength in [-1.0, -0.35, 0.0, 0.45, 1.0] {
            let src = test_image(31, 17);
            let op = VibranceOp::new(strength);
            let expected = op.apply(src.deep_clone()).unwrap();
            let gpu = GpuImage::from_image(&ctx, &src).unwrap();
            let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
            assert_eq!(actual.data, expected.data, "strength={strength}");
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn white_balance_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        for (temperature, tint) in [
            (0.0, 0.0),
            (0.5, 0.0),
            (-0.5, 0.0),
            (0.0, 0.5),
            (0.7, -0.4),
            (-1.0, 1.0),
        ] {
            let src = test_image(31, 17);
            let op = WhiteBalanceOp::new(temperature, tint);
            let expected = op.apply(src.deep_clone()).unwrap();
            let gpu = GpuImage::from_image(&ctx, &src).unwrap();
            let actual = apply_one(&ctx, &op, gpu).unwrap().into_image(&ctx).unwrap();
            assert_eq!(
                actual.data, expected.data,
                "temperature={temperature} tint={tint}"
            );
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn gpu_pipeline_chains_ops_with_single_readback() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let src = test_image(257, 129);
        let op_a = BrightnessContrastOp::new(0.12, -0.18);
        let op_b = BrightnessContrastOp::new(-0.08, 0.22);
        let expected = op_b.apply(op_a.apply(src.deep_clone()).unwrap()).unwrap();

        let mut pipeline = GpuPipeline::from_image(&ctx, &src).unwrap();
        pipeline.apply_op(&ctx, &op_a).unwrap();
        pipeline.apply_op(&ctx, &op_b).unwrap();
        assert_eq!(pipeline.op_count(), 2);
        let (actual, timings) = pipeline.into_image(&ctx).unwrap();

        assert_eq!(actual.data, expected.data);
        assert!(timings.upload > Default::default());
        assert!(timings.dispatch > Default::default());
        assert!(timings.readback > Default::default());
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn gpu_pipeline_chains_brightness_and_curves() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let src = test_image(257, 129);
        let op_a = BrightnessContrastOp::new(0.12, -0.18);
        let op_b = CurvesOp {
            points: vec![[0.0, 0.02], [0.3, 0.18], [0.74, 0.9], [1.0, 1.0]],
        };
        let expected = op_b.apply(op_a.apply(src.deep_clone()).unwrap()).unwrap();

        let mut pipeline = GpuPipeline::from_image(&ctx, &src).unwrap();
        pipeline.apply_op(&ctx, &op_a).unwrap();
        pipeline.apply_op(&ctx, &op_b).unwrap();
        assert_eq!(pipeline.op_count(), 2);
        let (actual, timings) = pipeline.into_image(&ctx).unwrap();

        assert_eq!(actual.data, expected.data);
        assert!(timings.upload > Default::default());
        assert!(timings.dispatch > Default::default());
        assert!(timings.readback > Default::default());
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn gpu_pipeline_chains_point_color_ops() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let src = test_image(257, 129);
        let op_a = BrightnessContrastOp::new(0.12, -0.18);
        let op_b = CurvesOp {
            points: vec![[0.0, 0.02], [0.3, 0.18], [0.74, 0.9], [1.0, 1.0]],
        };
        let op_c = HueShiftOp::new(47.0);
        let op_d = SaturationOp::new(1.65);
        let op_e = VibranceOp::new(0.48);
        let op_f = WhiteBalanceOp::new(0.32, -0.22);
        let expected = op_f
            .apply(
                op_e.apply(
                    op_d.apply(
                        op_c.apply(op_b.apply(op_a.apply(src.deep_clone()).unwrap()).unwrap())
                            .unwrap(),
                    )
                    .unwrap(),
                )
                .unwrap(),
            )
            .unwrap();

        let mut pipeline = GpuPipeline::from_image(&ctx, &src).unwrap();
        pipeline.apply_op(&ctx, &op_a).unwrap();
        pipeline.apply_op(&ctx, &op_b).unwrap();
        pipeline.apply_op(&ctx, &op_c).unwrap();
        pipeline.apply_op(&ctx, &op_d).unwrap();
        pipeline.apply_op(&ctx, &op_e).unwrap();
        pipeline.apply_op(&ctx, &op_f).unwrap();
        assert_eq!(pipeline.op_count(), 6);
        let (actual, timings) = pipeline.into_image(&ctx).unwrap();

        assert_eq!(actual.data, expected.data);
        assert!(timings.upload > Default::default());
        assert!(timings.dispatch > Default::default());
        assert!(timings.readback > Default::default());
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn large_image_dispatch_stays_within_wgpu_limits() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let src = Image::new(4096, 4096);
        let op = BrightnessContrastOp::new(0.0, 0.0);
        let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();

        assert_eq!(actual.width, src.width);
        assert_eq!(actual.height, src.height);
        assert_eq!(actual.data.len(), src.data.len());
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn noise_reduction_nlm_runs_on_gpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let src = test_image(32, 24);
        let op = NoiseReductionOp {
            method: NrMethod::NonLocalMeans,
            luma_strength: 0.5,
            color_strength: 0.5,
            detail_preservation: 0.0,
        };
        let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();

        assert_eq!(actual.width, src.width);
        assert_eq!(actual.height, src.height);
        assert_eq!(actual.data.len(), src.data.len());
        for (input, output) in src.data.chunks(4).zip(actual.data.chunks(4)) {
            assert_eq!(output[3], input[3]);
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn noise_reduction_nlm_roughly_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };

        let src = test_image(24, 18);
        let op = NoiseReductionOp {
            method: NrMethod::NonLocalMeans,
            luma_strength: 0.5,
            color_strength: 0.5,
            detail_preservation: 0.5,
        };
        let expected = op.apply(src.deep_clone()).unwrap();
        let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();

        let mut max_delta = 0u8;
        let mut sum_delta = 0u64;
        let mut count = 0u64;
        for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
            for channel in 0..3 {
                let delta = a[channel].abs_diff(b[channel]);
                max_delta = max_delta.max(delta);
                sum_delta += u64::from(delta);
                count += 1;
            }
            assert_eq!(a[3], b[3]);
        }
        let mean_delta = sum_delta as f64 / count as f64;
        assert!(
            mean_delta <= 3.0 && max_delta <= 16,
            "GPU NLM drifted too far from CPU: mean_delta={mean_delta:.2} max_delta={max_delta}"
        );
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn black_and_white_runs_on_gpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(32, 24);
        let op = BlackAndWhiteOp {
            mode: BwMode::Luminance,
        };
        let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        assert_eq!(out.width, src.width);
        assert_eq!(out.height, src.height);
        for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
            assert_eq!(o[3], i[3]);
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn black_and_white_roughly_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(24, 18);
        let op = BlackAndWhiteOp {
            mode: BwMode::Perceptual,
        };
        let expected = op.apply(src.deep_clone()).unwrap();
        let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        let mut max_delta = 0u8;
        for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
            for ch in 0..3 {
                max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
            }
            assert_eq!(a[3], b[3]);
        }
        assert!(max_delta <= 1, "black_and_white max_delta={max_delta}");
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn blur_runs_on_gpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        // Create a bright spot in a dark image
        let mut src = Image::new(32, 32);
        // Set most pixels dark
        for chunk in src.data.chunks_mut(4) {
            chunk[0] = 10;
            chunk[1] = 10;
            chunk[2] = 10;
            chunk[3] = 255;
        }
        // Bright centre pixel
        let cx = 16usize;
        let cy = 16usize;
        let idx = (cy * 32 + cx) * 4;
        src.data[idx] = 255;
        src.data[idx + 1] = 255;
        src.data[idx + 2] = 255;
        src.data[idx + 3] = 255;

        let op = BlurOp::new(2.0);
        let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        assert_eq!(out.width, src.width);
        assert_eq!(out.height, src.height);
        // The bright spot should be dimmed after blur
        assert!(
            out.data[idx] < 255,
            "bright centre should dim after blur, got {}",
            out.data[idx]
        );
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn color_balance_runs_on_gpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(32, 24);
        let op = ColorBalanceOp::new([0.5, 0.0, -0.5], [0.0, 0.3, 0.0], [-0.2, 0.0, 0.4]);
        let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        assert_eq!(out.width, src.width);
        assert_eq!(out.height, src.height);
        for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
            assert_eq!(o[3], i[3]);
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn color_balance_roughly_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(24, 18);
        let op = ColorBalanceOp::new([0.3, 0.0, -0.2], [0.0, 0.2, 0.0], [-0.1, 0.0, 0.3]);
        let expected = op.apply(src.deep_clone()).unwrap();
        let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        let mut max_delta = 0u8;
        for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
            for ch in 0..3 {
                max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
            }
            assert_eq!(a[3], b[3]);
        }
        assert!(max_delta <= 2, "color_balance max_delta={max_delta}");
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn color_space_runs_on_gpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(32, 24);
        let op = ColorSpaceOp {
            conversion: ColorSpaceConversion::SrgbToDisplayP3,
        };
        let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        assert_eq!(out.width, src.width);
        assert_eq!(out.height, src.height);
        for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
            assert_eq!(o[3], i[3]);
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn color_space_roughly_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(24, 18);
        let op = ColorSpaceOp {
            conversion: ColorSpaceConversion::SrgbToDisplayP3,
        };
        let expected = op.apply(src.deep_clone()).unwrap();
        let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        let mut max_delta = 0u8;
        for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
            for ch in 0..3 {
                max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
            }
            assert_eq!(a[3], b[3]);
        }
        assert!(max_delta <= 2, "color_space max_delta={max_delta}");
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn denoise_runs_on_gpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(32, 24);
        let op = DenoiseOp {
            strength: 0.3,
            radius: 2,
        };
        let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        assert_eq!(out.width, src.width);
        assert_eq!(out.height, src.height);
        for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
            assert_eq!(o[3], i[3]);
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn denoise_roughly_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(24, 18);
        let op = DenoiseOp {
            strength: 0.3,
            radius: 2,
        };
        let expected = op.apply(src.deep_clone()).unwrap();
        let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        let mut max_delta = 0u8;
        for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
            for ch in 0..3 {
                max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
            }
            assert_eq!(a[3], b[3]);
        }
        assert!(max_delta <= 3, "denoise max_delta={max_delta}");
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn hsl_panel_runs_on_gpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(32, 24);
        let op = HslPanelOp::new(
            [30.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.2, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0],
        );
        let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        assert_eq!(out.width, src.width);
        assert_eq!(out.height, src.height);
        for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
            assert_eq!(o[3], i[3]);
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn hsl_panel_roughly_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(24, 18);
        let op = HslPanelOp::new(
            [20.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            [0.0, 0.0, 0.05, 0.0, 0.0, 0.0, 0.0, 0.0],
        );
        let expected = op.apply(src.deep_clone()).unwrap();
        let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        let mut max_delta = 0u8;
        for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
            for ch in 0..3 {
                max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
            }
            assert_eq!(a[3], b[3]);
        }
        assert!(max_delta <= 2, "hsl_panel max_delta={max_delta}");
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn sharpen_runs_on_gpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(32, 24);
        let op = SharpenOp::new(1.0);
        let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        assert_eq!(out.width, src.width);
        assert_eq!(out.height, src.height);
        for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
            assert_eq!(o[3], i[3]);
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn sharpen_roughly_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(24, 18);
        let op = SharpenOp::new(1.0);
        let expected = op.apply(src.deep_clone()).unwrap();
        let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        let mut max_delta = 0u8;
        for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
            for ch in 0..3 {
                max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
            }
            assert_eq!(a[3], b[3]);
        }
        assert!(max_delta <= 2, "sharpen max_delta={max_delta}");
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn faux_hdr_runs_on_gpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(32, 24);
        let op = FauxHdrOp::new(0.8);
        let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        assert_eq!(out.width, src.width);
        assert_eq!(out.height, src.height);
        for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
            assert_eq!(o[3], i[3]);
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn faux_hdr_roughly_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(24, 18);
        let op = FauxHdrOp::new(0.8);
        let expected = op.apply(src.deep_clone()).unwrap();
        let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        let mut max_delta = 0u8;
        for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
            for ch in 0..3 {
                max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
            }
            assert_eq!(a[3], b[3]);
        }
        assert!(max_delta <= 1, "faux_hdr max_delta={max_delta}");
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn clarity_texture_runs_on_gpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(64, 48);
        let op = ClarityTextureOp::new(0.5, 0.3);
        let (out, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        assert_eq!(out.width, src.width);
        assert_eq!(out.height, src.height);
        for (i, o) in src.data.chunks(4).zip(out.data.chunks(4)) {
            assert_eq!(o[3], i[3]);
        }
    }

    #[test]
    #[ignore = "requires a working wgpu adapter"]
    fn clarity_texture_roughly_matches_cpu() {
        let Some(ctx) = pollster::block_on(make_context()) else {
            eprintln!("skipping: no wgpu adapter available");
            return;
        };
        let src = test_image(48, 36);
        let op = ClarityTextureOp::new(0.4, 0.0);
        let expected = op.apply(src.deep_clone()).unwrap();
        let (actual, _) = apply_one_to_image(&ctx, &op, &src).unwrap();
        let mut max_delta = 0u8;
        for (a, b) in actual.data.chunks(4).zip(expected.data.chunks(4)) {
            for ch in 0..3 {
                max_delta = max_delta.max(a[ch].abs_diff(b[ch]));
            }
            assert_eq!(a[3], b[3]);
        }
        assert!(max_delta <= 2, "clarity_texture max_delta={max_delta}");
    }
}
