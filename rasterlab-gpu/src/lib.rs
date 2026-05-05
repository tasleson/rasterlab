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
    ops::{BrightnessContrastOp, CurvesOp, NoiseReductionOp, NrMethod, SaturationOp},
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
    saturation: Arc<SaturationKernel>,
    noise_reduction_nlm: Arc<NoiseReductionNlmKernel>,
}

impl GpuContext {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, limits: wgpu::Limits) -> Self {
        let brightness_contrast = Arc::new(BrightnessContrastKernel::new(&device));
        let curves = Arc::new(CurvesKernel::new(&device));
        let saturation = Arc::new(SaturationKernel::new(&device));
        let noise_reduction_nlm = Arc::new(NoiseReductionNlmKernel::new(&device));
        Self {
            device: Arc::new(device),
            queue: Arc::new(queue),
            limits,
            brightness_contrast,
            curves,
            saturation,
            noise_reduction_nlm,
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
        .and_then(|any| any.downcast_ref::<SaturationOp>())
        .is_some()
    {
        return true;
    }
    op.as_any()
        .and_then(|any| any.downcast_ref::<NoiseReductionOp>())
        .is_some_and(|op| op.method == NrMethod::NonLocalMeans)
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
    } else if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<SaturationOp>())
    {
        apply_saturation(ctx, op, image)
    } else if let Some(op) = op
        .as_any()
        .and_then(|any| any.downcast_ref::<NoiseReductionOp>())
        .filter(|op| op.method == NrMethod::NonLocalMeans)
    {
        apply_noise_reduction_nlm(ctx, op, image)
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

struct SaturationKernel {
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
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
        let op_c = SaturationOp::new(1.65);
        let expected = op_c
            .apply(op_b.apply(op_a.apply(src.deep_clone()).unwrap()).unwrap())
            .unwrap();

        let mut pipeline = GpuPipeline::from_image(&ctx, &src).unwrap();
        pipeline.apply_op(&ctx, &op_a).unwrap();
        pipeline.apply_op(&ctx, &op_b).unwrap();
        pipeline.apply_op(&ctx, &op_c).unwrap();
        assert_eq!(pipeline.op_count(), 3);
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
}
