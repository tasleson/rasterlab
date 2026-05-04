//! GPU kernels for RasterLab operations.
//!
//! This crate intentionally stays below the GUI/rendering layer. It owns no
//! windows or egui textures; callers provide a `wgpu::Device` and `wgpu::Queue`.

use std::{
    sync::{Arc, mpsc},
    time::Instant,
};

use bytemuck::{Pod, Zeroable};
use rasterlab_core::{Image, ops::BrightnessContrastOp, traits::operation::Operation};
use thiserror::Error;
use wgpu::util::DeviceExt;

const WORKGROUP_SIZE: u32 = 256;

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
}

#[derive(Clone)]
pub struct GpuContext {
    device: Arc<wgpu::Device>,
    queue: Arc<wgpu::Queue>,
    limits: wgpu::Limits,
    brightness_contrast: Arc<BrightnessContrastKernel>,
}

impl GpuContext {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, limits: wgpu::Limits) -> Self {
        let brightness_contrast = Arc::new(BrightnessContrastKernel::new(&device));
        Self {
            device: Arc::new(device),
            queue: Arc::new(queue),
            limits,
            brightness_contrast,
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
    op.as_any()
        .and_then(|any| any.downcast_ref::<BrightnessContrastOp>())
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
    } else {
        Err(GpuError::UnsupportedOperation(op.name()))
    }
}

pub fn apply_one_to_image(
    ctx: &GpuContext,
    op: &dyn Operation,
    image: &Image,
) -> Result<(Image, GpuTimings), GpuError> {
    let start = Instant::now();
    let gpu_image = GpuImage::from_image(ctx, image)?;
    let upload = start.elapsed();

    let dispatch_start = Instant::now();
    let gpu_image = apply_one(ctx, op, gpu_image)?;
    let dispatch = dispatch_start.elapsed();

    let readback_start = Instant::now();
    let mut out = gpu_image.into_image(ctx)?;
    out.metadata = image.metadata.clone();
    let readback = readback_start.elapsed();

    Ok((
        out,
        GpuTimings {
            upload,
            dispatch,
            readback,
        },
    ))
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

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    pixel_count: u32,
    _pad: [u32; 3],
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
    let params = Params {
        pixel_count: image.width.saturating_mul(image.height),
        _pad: [0; 3],
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
        let groups = params.pixel_count.div_ceil(WORKGROUP_SIZE);
        pass.dispatch_workgroups(groups, 1, 1);
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
    pixel_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
};

@group(0) @binding(0) var<storage, read> input_pixels: array<u32>;
@group(0) @binding(1) var<storage, read_write> output_pixels: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read> lut: array<u32>;

fn channel(byte: u32) -> u32 {
    return lut[byte] & 0xffu;
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = gid.x;
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
}
