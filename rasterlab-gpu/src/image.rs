use std::sync::mpsc;

use rasterlab_core::Image;
use wgpu::util::DeviceExt;

use crate::{common::expected_rgba_len, context::GpuContext, error::GpuError};

pub struct GpuImage {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) buffer: wgpu::Buffer,
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
