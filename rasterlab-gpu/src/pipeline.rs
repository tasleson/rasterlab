use std::time::Instant;

use rasterlab_core::{Image, image::ImageMetadata, traits::operation::Operation};

use crate::{context::GpuContext, error::GpuError, image::GpuImage, ops::apply_one};

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
