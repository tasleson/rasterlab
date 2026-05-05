use thiserror::Error;

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
