use bytemuck::{Pod, Zeroable};
use rasterlab_core::{
    Image,
    ops::{
        BlackAndWhiteOp, BlurOp, BrightnessContrastOp, BwMode, ClarityTextureOp, ColorBalanceOp,
        ColorSpaceConversion, ColorSpaceOp, CurvesOp, DenoiseOp, FauxHdrOp, HighlightsShadowsOp,
        HslPanelOp, HueShiftOp, IntensifyHdrOp, LevelsOp, NoiseReductionOp, NrMethod, SaturationOp,
        SepiaOp, ShadowExposureOp, SharpenOp, SplitToneOp, VibranceOp, VignetteOp, WhiteBalanceOp,
    },
    traits::operation::Operation,
};
use wgpu::util::DeviceExt;

use crate::{
    common::{BOX_BLUR_WORKGROUP_SIZE, NLM_WORKGROUP_SIZE, WORKGROUP_SIZE, expected_rgba_len},
    context::GpuContext,
    error::GpuError,
    image::GpuImage,
    pipeline::{GpuPipeline, GpuTimings},
};

/// Number of `SupportedGpuOp` variants.  Bump when adding a kernel: the
/// dispatch test asserts it against its sample op per variant, so every new
/// kernel gets checked for an `Operation::as_any` override.
#[cfg(test)]
pub(crate) const SUPPORTED_GPU_OP_COUNT: usize = 23;

enum SupportedGpuOp<'a> {
    BrightnessContrast(&'a BrightnessContrastOp),
    Curves(&'a CurvesOp),
    HueShift(&'a HueShiftOp),
    Saturation(&'a SaturationOp),
    Vibrance(&'a VibranceOp),
    WhiteBalance(&'a WhiteBalanceOp),
    NoiseReductionNlm(&'a NoiseReductionOp),
    Sepia(&'a SepiaOp),
    Levels(&'a LevelsOp),
    HighlightsShadows(&'a HighlightsShadowsOp),
    Vignette(&'a VignetteOp),
    ShadowExposure(&'a ShadowExposureOp),
    SplitTone(&'a SplitToneOp),
    BlackAndWhite(&'a BlackAndWhiteOp),
    Blur(&'a BlurOp),
    ColorBalance(&'a ColorBalanceOp),
    ColorSpace(&'a ColorSpaceOp),
    Denoise(&'a DenoiseOp),
    HslPanel(&'a HslPanelOp),
    Sharpen(&'a SharpenOp),
    FauxHdr(&'a FauxHdrOp),
    ClarityTexture(&'a ClarityTextureOp),
    IntensifyHdr(&'a IntensifyHdrOp),
}

fn classify(op: &dyn Operation) -> Option<SupportedGpuOp<'_>> {
    let any = op.as_any()?;

    if let Some(op) = any.downcast_ref::<BrightnessContrastOp>() {
        Some(SupportedGpuOp::BrightnessContrast(op))
    } else if let Some(op) = any.downcast_ref::<CurvesOp>() {
        Some(SupportedGpuOp::Curves(op))
    } else if let Some(op) = any.downcast_ref::<HueShiftOp>() {
        Some(SupportedGpuOp::HueShift(op))
    } else if let Some(op) = any.downcast_ref::<SaturationOp>() {
        Some(SupportedGpuOp::Saturation(op))
    } else if let Some(op) = any.downcast_ref::<VibranceOp>() {
        Some(SupportedGpuOp::Vibrance(op))
    } else if let Some(op) = any.downcast_ref::<WhiteBalanceOp>() {
        Some(SupportedGpuOp::WhiteBalance(op))
    } else if let Some(op) = any
        .downcast_ref::<NoiseReductionOp>()
        .filter(|op| op.method == NrMethod::NonLocalMeans)
    {
        Some(SupportedGpuOp::NoiseReductionNlm(op))
    } else if let Some(op) = any.downcast_ref::<SepiaOp>() {
        Some(SupportedGpuOp::Sepia(op))
    } else if let Some(op) = any.downcast_ref::<LevelsOp>() {
        Some(SupportedGpuOp::Levels(op))
    } else if let Some(op) = any.downcast_ref::<HighlightsShadowsOp>() {
        Some(SupportedGpuOp::HighlightsShadows(op))
    } else if let Some(op) = any.downcast_ref::<VignetteOp>() {
        Some(SupportedGpuOp::Vignette(op))
    } else if let Some(op) = any.downcast_ref::<ShadowExposureOp>() {
        Some(SupportedGpuOp::ShadowExposure(op))
    } else if let Some(op) = any.downcast_ref::<SplitToneOp>() {
        Some(SupportedGpuOp::SplitTone(op))
    } else if let Some(op) = any.downcast_ref::<BlackAndWhiteOp>() {
        Some(SupportedGpuOp::BlackAndWhite(op))
    } else if let Some(op) = any.downcast_ref::<BlurOp>() {
        Some(SupportedGpuOp::Blur(op))
    } else if let Some(op) = any.downcast_ref::<ColorBalanceOp>() {
        Some(SupportedGpuOp::ColorBalance(op))
    } else if let Some(op) = any.downcast_ref::<ColorSpaceOp>() {
        Some(SupportedGpuOp::ColorSpace(op))
    } else if let Some(op) = any.downcast_ref::<DenoiseOp>() {
        Some(SupportedGpuOp::Denoise(op))
    } else if let Some(op) = any.downcast_ref::<HslPanelOp>() {
        Some(SupportedGpuOp::HslPanel(op))
    } else if let Some(op) = any.downcast_ref::<SharpenOp>() {
        Some(SupportedGpuOp::Sharpen(op))
    } else if let Some(op) = any.downcast_ref::<FauxHdrOp>() {
        Some(SupportedGpuOp::FauxHdr(op))
    } else if let Some(op) = any.downcast_ref::<ClarityTextureOp>() {
        Some(SupportedGpuOp::ClarityTexture(op))
    } else {
        any.downcast_ref::<IntensifyHdrOp>()
            .map(SupportedGpuOp::IntensifyHdr)
    }
}

pub fn supports(op: &dyn Operation) -> bool {
    classify(op).is_some()
}

pub fn apply_one(
    ctx: &GpuContext,
    op: &dyn Operation,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    match classify(op) {
        Some(SupportedGpuOp::BrightnessContrast(op)) => apply_brightness_contrast(ctx, op, image),
        Some(SupportedGpuOp::Curves(op)) => apply_curves(ctx, op, image),
        Some(SupportedGpuOp::HueShift(op)) => apply_hue_shift(ctx, op, image),
        Some(SupportedGpuOp::Saturation(op)) => apply_saturation(ctx, op, image),
        Some(SupportedGpuOp::Vibrance(op)) => apply_vibrance(ctx, op, image),
        Some(SupportedGpuOp::WhiteBalance(op)) => apply_white_balance(ctx, op, image),
        Some(SupportedGpuOp::NoiseReductionNlm(op)) => apply_noise_reduction_nlm(ctx, op, image),
        Some(SupportedGpuOp::Sepia(op)) => apply_sepia(ctx, op, image),
        Some(SupportedGpuOp::Levels(op)) => apply_levels(ctx, op, image),
        Some(SupportedGpuOp::HighlightsShadows(op)) => apply_highlights_shadows(ctx, op, image),
        Some(SupportedGpuOp::Vignette(op)) => apply_vignette(ctx, op, image),
        Some(SupportedGpuOp::ShadowExposure(op)) => apply_shadow_exposure(ctx, op, image),
        Some(SupportedGpuOp::SplitTone(op)) => apply_split_tone(ctx, op, image),
        Some(SupportedGpuOp::BlackAndWhite(op)) => apply_black_and_white(ctx, op, image),
        Some(SupportedGpuOp::Blur(op)) => apply_blur(ctx, op, image),
        Some(SupportedGpuOp::ColorBalance(op)) => apply_color_balance(ctx, op, image),
        Some(SupportedGpuOp::ColorSpace(op)) => apply_color_space(ctx, op, image),
        Some(SupportedGpuOp::Denoise(op)) => apply_denoise(ctx, op, image),
        Some(SupportedGpuOp::HslPanel(op)) => apply_hsl_panel(ctx, op, image),
        Some(SupportedGpuOp::Sharpen(op)) => apply_sharpen(ctx, op, image),
        Some(SupportedGpuOp::FauxHdr(op)) => apply_faux_hdr(ctx, op, image),
        Some(SupportedGpuOp::ClarityTexture(op)) => apply_clarity_texture(ctx, op, image),
        Some(SupportedGpuOp::IntensifyHdr(op)) => apply_intensify_hdr(ctx, op, image),
        None => Err(GpuError::UnsupportedOperation(op.name())),
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
    let output = output_buffer(ctx, "rasterlab brightness_contrast output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(
            &ctx.brightness_contrast.pipeline,
            &ctx.brightness_contrast.bind_group_layout,
        ),
        "rasterlab brightness_contrast",
        [&image.buffer, &output, &params_buffer, &lut_buffer],
        [image.width, image.height],
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_curves(ctx: &GpuContext, op: &CurvesOp, image: GpuImage) -> Result<GpuImage, GpuError> {
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = output_buffer(ctx, "rasterlab curves output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(&ctx.curves.pipeline, &ctx.curves.bind_group_layout),
        "rasterlab curves",
        [&image.buffer, &output, &params_buffer, &lut_buffer],
        [image.width, image.height],
    )?;
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
    let output = output_buffer(ctx, "rasterlab hue_shift output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(&ctx.hue_shift.pipeline, &ctx.hue_shift.bind_group_layout),
        "rasterlab hue_shift",
        [&image.buffer, &output, &params_buffer],
        [image.width, image.height],
    )?;
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
    let output = output_buffer(ctx, "rasterlab saturation output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(&ctx.saturation.pipeline, &ctx.saturation.bind_group_layout),
        "rasterlab saturation",
        [&image.buffer, &output, &params_buffer],
        [image.width, image.height],
    )?;
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
    let output = output_buffer(ctx, "rasterlab vibrance output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(&ctx.vibrance.pipeline, &ctx.vibrance.bind_group_layout),
        "rasterlab vibrance",
        [&image.buffer, &output, &params_buffer],
        [image.width, image.height],
    )?;
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
    let output = output_buffer(ctx, "rasterlab white_balance output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(
            &ctx.white_balance.pipeline,
            &ctx.white_balance.bind_group_layout,
        ),
        "rasterlab white_balance",
        [&image.buffer, &output, &params_buffer],
        [image.width, image.height],
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

/// A compute pipeline and the two things a dispatch has to know about it: the
/// layout its bind group must match, and the `@workgroup_size` its shader
/// declares.
///
/// The workgroup size travels with the pipeline because sizing a dispatch from
/// the wrong one silently leaves the tail of the image unprocessed instead of
/// failing — the two NLM kernels are 8×8 where everything else is 16×16.
struct Kernel<'a> {
    pipeline: &'a wgpu::ComputePipeline,
    layout: &'a wgpu::BindGroupLayout,
    workgroup: [u32; 2],
}

impl<'a> Kernel<'a> {
    /// A kernel from a shader declaring the usual `@workgroup_size(16, 16)`.
    fn new(pipeline: &'a wgpu::ComputePipeline, layout: &'a wgpu::BindGroupLayout) -> Self {
        Self {
            pipeline,
            layout,
            workgroup: WORKGROUP_SIZE,
        }
    }

    /// A kernel whose shader declares some other `@workgroup_size`.
    fn with_workgroup(
        pipeline: &'a wgpu::ComputePipeline,
        layout: &'a wgpu::BindGroupLayout,
        workgroup: [u32; 2],
    ) -> Self {
        Self {
            pipeline,
            layout,
            workgroup,
        }
    }
}

// Buffers are supplied in shader binding order, starting at binding zero.
fn encode_compute<const N: usize>(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    kernel: Kernel<'_>,
    label: &str,
    buffers: [&wgpu::Buffer; N],
    dimensions: [u32; 2],
) {
    let entries = std::array::from_fn::<_, N, _>(|binding| wgpu::BindGroupEntry {
        binding: binding as u32,
        resource: buffers[binding].as_entire_binding(),
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: kernel.layout,
        entries: &entries,
    });
    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
        label: Some(label),
        timestamp_writes: None,
    });
    pass.set_pipeline(kernel.pipeline);
    pass.set_bind_group(0, &bind_group, &[]);
    pass.dispatch_workgroups(
        dimensions[0].div_ceil(kernel.workgroup[0]),
        dimensions[1].div_ceil(kernel.workgroup[1]),
        1,
    );
}

fn dispatch_compute<const N: usize>(
    ctx: &GpuContext,
    kernel: Kernel<'_>,
    label: &str,
    buffers: [&wgpu::Buffer; N],
    dimensions: [u32; 2],
) -> Result<(), GpuError> {
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    encode_compute(
        &ctx.device,
        &mut encoder,
        kernel,
        label,
        buffers,
        dimensions,
    );
    submit_and_wait(ctx, encoder)
}

/// Hand the encoded passes to the queue and block until the GPU is done with
/// them, so the caller can read the output buffer back.
fn submit_and_wait(ctx: &GpuContext, encoder: wgpu::CommandEncoder) -> Result<(), GpuError> {
    ctx.queue.submit(Some(encoder.finish()));
    ctx.device
        .poll(wgpu::PollType::wait_indefinitely())
        .map_err(|e| GpuError::Poll(e.to_string()))?;
    Ok(())
}

/// A buffer an op writes its result into: readable back to the CPU, and usable
/// as the input of whatever op runs next.
fn output_buffer(ctx: &GpuContext, label: &str, byte_len: u64) -> wgpu::Buffer {
    ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: byte_len,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    })
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
    let output = output_buffer(ctx, "rasterlab sepia output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(&ctx.sepia.pipeline, &ctx.sepia.bind_group_layout),
        "rasterlab sepia",
        [&image.buffer, &output, &params_buffer],
        [params.width, params.height],
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn apply_levels(ctx: &GpuContext, op: &LevelsOp, image: GpuImage) -> Result<GpuImage, GpuError> {
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = output_buffer(ctx, "rasterlab levels output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(&ctx.levels.pipeline, &ctx.levels.bind_group_layout),
        "rasterlab levels",
        [&image.buffer, &output, &params_buffer, &lut_buffer],
        [image.width, image.height],
    )?;
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
    let output = output_buffer(ctx, "rasterlab highlights_shadows output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(
            &ctx.highlights_shadows.pipeline,
            &ctx.highlights_shadows.bind_group_layout,
        ),
        "rasterlab highlights_shadows",
        [&image.buffer, &output, &params_buffer],
        [params.width, params.height],
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
    let output = output_buffer(ctx, "rasterlab vignette output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(&ctx.vignette.pipeline, &ctx.vignette.bind_group_layout),
        "rasterlab vignette",
        [&image.buffer, &output, &params_buffer],
        [params.width, params.height],
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
    let output = output_buffer(ctx, "rasterlab shadow_exposure output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(
            &ctx.shadow_exposure.pipeline,
            &ctx.shadow_exposure.bind_group_layout,
        ),
        "rasterlab shadow_exposure",
        [&image.buffer, &output, &params_buffer],
        [params.width, params.height],
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
    let output = output_buffer(ctx, "rasterlab split_tone output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(&ctx.split_tone.pipeline, &ctx.split_tone.bind_group_layout),
        "rasterlab split_tone",
        [&image.buffer, &output, &params_buffer],
        [params.width, params.height],
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
    let output = output_buffer(ctx, "rasterlab black_and_white output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(
            &ctx.black_and_white.pipeline,
            &ctx.black_and_white.bind_group_layout,
        ),
        "rasterlab black_and_white",
        [&image.buffer, &output, &params_buffer],
        [params.width, params.height],
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
    let output = output_buffer(ctx, "rasterlab blur output", byte_len);
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

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab blur"),
        });
    // Separable: the horizontal pass writes the intermediate the vertical one
    // reads, and both share the one params buffer.
    encode_compute(
        &ctx.device,
        &mut encoder,
        Kernel::new(&ctx.blur.h_pipeline, &ctx.blur.bind_group_layout),
        "rasterlab blur h",
        [&image.buffer, &intermediate, &params_buffer],
        [params.width, params.height],
    );
    encode_compute(
        &ctx.device,
        &mut encoder,
        Kernel::new(&ctx.blur.v_pipeline, &ctx.blur.bind_group_layout),
        "rasterlab blur v",
        [&intermediate, &output, &params_buffer],
        [params.width, params.height],
    );
    submit_and_wait(ctx, encoder)?;

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
    let output = output_buffer(ctx, "rasterlab color_balance output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(
            &ctx.color_balance.pipeline,
            &ctx.color_balance.bind_group_layout,
        ),
        "rasterlab color_balance",
        [&image.buffer, &output, &params_buffer],
        [params.width, params.height],
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
    let output = output_buffer(ctx, "rasterlab color_space output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(
            &ctx.color_space.pipeline,
            &ctx.color_space.bind_group_layout,
        ),
        "rasterlab color_space",
        [&image.buffer, &output, &params_buffer],
        [params.width, params.height],
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
    let output = output_buffer(ctx, "rasterlab denoise output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(&ctx.denoise.pipeline, &ctx.denoise.bind_group_layout),
        "rasterlab denoise",
        [&image.buffer, &output, &params_buffer],
        [params.width, params.height],
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
    let output = output_buffer(ctx, "rasterlab hsl_panel output", byte_len);
    let params = HslPanelParams {
        width: image.width,
        height: image.height,
        pixel_count: image.width.saturating_mul(image.height),
        _pad: 0,
        hue: pack_hsl_panel_values(op.hue),
        sat: pack_hsl_panel_values(op.saturation),
        lum: pack_hsl_panel_values(op.luminance),
    };
    let params_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab hsl_panel params"),
            contents: bytemuck::bytes_of(&params),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    dispatch_compute(
        ctx,
        Kernel::new(&ctx.hsl_panel.pipeline, &ctx.hsl_panel.bind_group_layout),
        "rasterlab hsl_panel",
        [&image.buffer, &output, &params_buffer],
        [params.width, params.height],
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

fn pack_hsl_panel_values(values: [f32; 8]) -> [[f32; 4]; 2] {
    [
        [values[0], values[1], values[2], values[3]],
        [values[4], values[5], values[6], values[7]],
    ]
}

fn apply_sharpen(ctx: &GpuContext, op: &SharpenOp, image: GpuImage) -> Result<GpuImage, GpuError> {
    if op.strength <= 0.0 {
        return Ok(image);
    }
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = output_buffer(ctx, "rasterlab sharpen output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(&ctx.sharpen.pipeline, &ctx.sharpen.bind_group_layout),
        "rasterlab sharpen",
        [&image.buffer, &output, &params_buffer],
        [params.width, params.height],
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
    let output = output_buffer(ctx, "rasterlab noise_reduction_nlm output", byte_len);
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
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab noise_reduction_nlm"),
        });
    // The first pass denoises into Y'CbCr; the second reads the original
    // alongside it to put back the detail the first pass removed.
    encode_compute(
        &ctx.device,
        &mut encoder,
        Kernel::with_workgroup(
            &ctx.noise_reduction_nlm.nlm_pipeline,
            &ctx.noise_reduction_nlm.nlm_bind_group_layout,
            NLM_WORKGROUP_SIZE,
        ),
        "rasterlab noise_reduction_nlm",
        [&image.buffer, &denoised_ycc, &params_buffer],
        [image.width, image.height],
    );
    encode_compute(
        &ctx.device,
        &mut encoder,
        Kernel::with_workgroup(
            &ctx.noise_reduction_nlm.detail_pipeline,
            &ctx.noise_reduction_nlm.detail_bind_group_layout,
            NLM_WORKGROUP_SIZE,
        ),
        "rasterlab noise_reduction_detail",
        [&image.buffer, &denoised_ycc, &output, &params_buffer],
        [image.width, image.height],
    );
    submit_and_wait(ctx, encoder)?;

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
    hue: [[f32; 4]; 2],
    sat: [[f32; 4]; 2],
    lum: [[f32; 4]; 2],
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

fn apply_faux_hdr(ctx: &GpuContext, op: &FauxHdrOp, image: GpuImage) -> Result<GpuImage, GpuError> {
    if op.strength < 1e-5 {
        return Ok(image);
    }
    let byte_len = expected_rgba_len(image.width, image.height) as u64;
    let output = output_buffer(ctx, "rasterlab faux_hdr output", byte_len);
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
    dispatch_compute(
        ctx,
        Kernel::new(&ctx.faux_hdr.pipeline, &ctx.faux_hdr.bind_group_layout),
        "rasterlab faux_hdr",
        [&image.buffer, &output, &params_buffer],
        [params.width, params.height],
    )?;
    Ok(GpuImage {
        width: image.width,
        height: image.height,
        buffer: output,
    })
}

/// One unsharp-mask stage of Clarity/Texture: blur the source's luma, then add
/// back a weighted share of what the blur removed.
///
/// Clarity and Texture are the same three passes over the same buffers; they
/// differ only in how wide the blur is and whether the result is weighted
/// towards midtones.
struct UnsharpStage<'a> {
    /// Prefixes the debug labels, so a capture says which stage a pass is.
    name: &'a str,
    src: &'a wgpu::Buffer,
    dst: &'a wgpu::Buffer,
    radius: u32,
    midtone_weight: u32,
    amount: f32,
}

/// The luma buffers the blur ping-pongs between, and the extent they cover.
/// Both stages reuse the one set: the second overwrites what the first left.
struct LumaScratch<'a> {
    a: &'a wgpu::Buffer,
    b: &'a wgpu::Buffer,
    params: &'a wgpu::Buffer,
}

/// Box blur repeated this many times approximates a Gaussian closely enough
/// for an unsharp mask, and each pass is separable.  An odd count would leave
/// the result in the wrong buffer of the ping-pong.
const BOX_BLUR_PASSES: usize = 3;

/// Extract `src`'s luma and blur it into `scratch.a`, which is where every
/// caller's final per-pixel pass then reads the base layer from.
///
/// Shared by Clarity/Texture and Intensify HDR: both want the same three
/// separable box-blur passes the CPU ops run, over the same ping-pong pair,
/// and differ only in what they do with the result.
fn encode_blurred_luma(
    ctx: &GpuContext,
    encoder: &mut wgpu::CommandEncoder,
    name: &str,
    src: &wgpu::Buffer,
    radius: u32,
    scratch: &LumaScratch<'_>,
    dimensions: [u32; 2],
) {
    let [w, h] = dimensions;
    let blur_params = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(&format!("rasterlab {name} blur params")),
            contents: bytemuck::bytes_of(&ClarityBlurParams {
                width: w,
                height: h,
                pixel_count: w.saturating_mul(h),
                radius,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });

    encode_compute(
        &ctx.device,
        encoder,
        Kernel::new(
            &ctx.clarity_texture.extract_luma_pipeline,
            &ctx.clarity_texture.three_bind_layout,
        ),
        &format!("{name} extract_luma"),
        [src, scratch.a, scratch.params],
        dimensions,
    );

    // Each iteration is a horizontal pass into `b` and a vertical one back
    // into `a`, so the blurred luma ends up in `a` however many run.
    //
    // Both blur kernels slide a window along a whole line, so they dispatch
    // one invocation per line — rows for the horizontal pass, columns for the
    // vertical one — not one per pixel like everything else here.
    for _ in 0..BOX_BLUR_PASSES {
        encode_compute(
            &ctx.device,
            encoder,
            Kernel::with_workgroup(
                &ctx.clarity_texture.box_blur_h_pipeline,
                &ctx.clarity_texture.three_bind_layout,
                BOX_BLUR_WORKGROUP_SIZE,
            ),
            &format!("{name} box_blur_h"),
            [scratch.a, scratch.b, &blur_params],
            [h, 1],
        );
        encode_compute(
            &ctx.device,
            encoder,
            Kernel::with_workgroup(
                &ctx.clarity_texture.box_blur_v_pipeline,
                &ctx.clarity_texture.three_bind_layout,
                BOX_BLUR_WORKGROUP_SIZE,
            ),
            &format!("{name} box_blur_v"),
            [scratch.b, scratch.a, &blur_params],
            [w, 1],
        );
    }
}

fn encode_unsharp_stage(
    ctx: &GpuContext,
    encoder: &mut wgpu::CommandEncoder,
    stage: UnsharpStage<'_>,
    scratch: &LumaScratch<'_>,
    dimensions: [u32; 2],
) {
    let [w, h] = dimensions;
    let pixel_count = w.saturating_mul(h);
    let name = stage.name;

    encode_blurred_luma(
        ctx,
        encoder,
        name,
        stage.src,
        stage.radius,
        scratch,
        dimensions,
    );

    let detail_params = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some(&format!("rasterlab {name} detail params")),
            contents: bytemuck::bytes_of(&ClarityDetailParams {
                width: w,
                height: h,
                pixel_count,
                midtone_weight: stage.midtone_weight,
                amount: stage.amount,
                _pad1: 0.0,
                _pad2: 0.0,
                _pad3: 0.0,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });

    encode_compute(
        &ctx.device,
        encoder,
        Kernel::new(
            &ctx.clarity_texture.apply_detail_pipeline,
            &ctx.clarity_texture.four_bind_layout,
        ),
        &format!("{name} apply_detail"),
        [stage.src, stage.dst, &detail_params, scratch.a],
        dimensions,
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
        Some(output_buffer(
            ctx,
            "rasterlab clarity output rgba",
            rgba_byte_len,
        ))
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
    let scratch = LumaScratch {
        a: &luma_a,
        b: &luma_b,
        params: &luma_params_buf,
    };

    if do_clarity {
        encode_unsharp_stage(
            ctx,
            &mut encoder,
            UnsharpStage {
                name: "clarity",
                src: &image.buffer,
                dst: &intermediate,
                radius: clarity_radius,
                midtone_weight: 1,
                amount: op.clarity,
            },
            &scratch,
            [w, h],
        );
    }

    if do_texture {
        encode_unsharp_stage(
            ctx,
            &mut encoder,
            UnsharpStage {
                name: "texture",
                // Texture sharpens what Clarity already produced, when it ran.
                src: if do_clarity {
                    &intermediate
                } else {
                    &image.buffer
                },
                dst: output
                    .as_ref()
                    .expect("do_texture implies an output buffer"),
                radius: texture_radius,
                midtone_weight: 0,
                amount: op.texture,
            },
            &scratch,
            [w, h],
        );
    }

    submit_and_wait(ctx, encoder)?;

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

// Intensify HDR's tuning constants.  The CPU op keeps its own private copies
// in `rasterlab-core/src/ops/intensify_hdr.rs`, where they are documented and
// where the measurements behind them live; these mirror them so the shader
// and the reference stay the same effect.  The parity tests in this crate are
// what catch them drifting apart.
const INTENSIFY_TONE_POINTS: [[f32; 2]; 14] = [
    [0.000, 0.000],
    [0.016, 0.000],
    [0.031, 0.082],
    [0.063, 0.216],
    [0.078, 0.267],
    [0.125, 0.282],
    [0.251, 0.352],
    [0.376, 0.384],
    [0.502, 0.404],
    [0.627, 0.486],
    [0.753, 0.627],
    [0.878, 0.780],
    [0.941, 0.875],
    [1.000, 1.000],
];
const INTENSIFY_BASE_RADIUS_FRAC: f32 = 0.03;
const INTENSIFY_DETAIL_GAIN: f32 = 1.77;
const INTENSIFY_SATURATION: f32 = 1.78;
const INTENSIFY_RATIO_FLOOR: f32 = 1.0;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct IntensifyHdrParams {
    width: u32,
    height: u32,
    pixel_count: u32,
    _pad: u32,
    amount: f32,
    detail_gain: f32,
    saturation: f32,
    ratio_floor: f32,
}

/// Intensify HDR: a fixed compressive tone curve on a large-radius blurred
/// base, the detail the blur removed put back over it with gain, and a plain
/// saturation multiply — then blended back over the source.
///
/// The base layer is Clarity's luma-and-box-blur passes verbatim, so only the
/// final per-pixel kernel is specific to this op.
fn apply_intensify_hdr(
    ctx: &GpuContext,
    op: &IntensifyHdrOp,
    image: GpuImage,
) -> Result<GpuImage, GpuError> {
    if op.amount <= f32::EPSILON {
        return Ok(image);
    }

    let w = image.width;
    let h = image.height;
    let pixel_count = w.saturating_mul(h);
    let rgba_byte_len = expected_rgba_len(w, h) as u64;
    let luma_byte_len = pixel_count as u64 * 4;
    let radius = ((INTENSIFY_BASE_RADIUS_FRAC * w.min(h) as f32).round() as u32).max(1);

    let luma_a = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab intensify_hdr luma_a"),
        size: luma_byte_len,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let luma_b = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("rasterlab intensify_hdr luma_b"),
        size: luma_byte_len,
        usage: wgpu::BufferUsages::STORAGE,
        mapped_at_creation: false,
    });
    let output = output_buffer(ctx, "rasterlab intensify_hdr output", rgba_byte_len);

    let luma_params = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab intensify_hdr luma params"),
            contents: bytemuck::bytes_of(&ClarityLumaParams {
                width: w,
                height: h,
                pixel_count,
                _pad: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let apply_params = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab intensify_hdr apply params"),
            contents: bytemuck::bytes_of(&IntensifyHdrParams {
                width: w,
                height: h,
                pixel_count,
                _pad: 0,
                amount: op.amount.clamp(0.0, 1.0),
                detail_gain: INTENSIFY_DETAIL_GAIN,
                saturation: INTENSIFY_SATURATION,
                ratio_floor: INTENSIFY_RATIO_FLOOR,
            }),
            usage: wgpu::BufferUsages::UNIFORM,
        });
    let tone_lut: [u32; 256] = CurvesOp::build_lut(&INTENSIFY_TONE_POINTS).map(u32::from);
    let tone_lut_buffer = ctx
        .device
        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("rasterlab intensify_hdr tone lut"),
            contents: bytemuck::cast_slice(&tone_lut),
            usage: wgpu::BufferUsages::STORAGE,
        });

    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("rasterlab intensify_hdr encoder"),
        });
    let scratch = LumaScratch {
        a: &luma_a,
        b: &luma_b,
        params: &luma_params,
    };
    encode_blurred_luma(
        ctx,
        &mut encoder,
        "intensify_hdr",
        &image.buffer,
        radius,
        &scratch,
        [w, h],
    );
    encode_compute(
        &ctx.device,
        &mut encoder,
        Kernel::new(
            &ctx.intensify_hdr.apply_pipeline,
            &ctx.intensify_hdr.five_bind_layout,
        ),
        "intensify_hdr apply",
        [
            &image.buffer,
            &output,
            &apply_params,
            &luma_a,
            &tone_lut_buffer,
        ],
        [w, h],
    );
    submit_and_wait(ctx, encoder)?;

    Ok(GpuImage {
        width: w,
        height: h,
        buffer: output,
    })
}
