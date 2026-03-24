use thiserror::Error;

/// All errors produced by the rasterlab-core engine.
#[derive(Debug, Error)]
pub enum RasterError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Decode error ({format}): {message}")]
    Decode { format: String, message: String },

    #[error("Encode error ({format}): {message}")]
    Encode { format: String, message: String },

    #[error("Unsupported image format: {0}")]
    UnsupportedFormat(String),

    #[error("Format '{0}' does not support encoding")]
    FormatNotEncodable(String),

    #[error("Invalid operation parameters: {0}")]
    InvalidParams(String),

    #[error("Image dimensions out of range: {0}")]
    DimensionsOutOfRange(String),

    #[error("Pipeline error: {0}")]
    Pipeline(String),

    #[error("Plugin error: {0}")]
    Plugin(String),

    #[error("Plugin API version mismatch: host expects {expected}, plugin reports {got}")]
    PluginApiVersionMismatch { expected: u32, got: u32 },

    #[error("Serialization error: {0}")]
    Serialization(String),
}

/// Convenience alias used throughout the crate.
pub type RasterResult<T> = Result<T, RasterError>;

impl RasterError {
    pub fn decode(format: impl Into<String>, message: impl Into<String>) -> Self {
        RasterError::Decode { format: format.into(), message: message.into() }
    }

    pub fn encode(format: impl Into<String>, message: impl Into<String>) -> Self {
        RasterError::Encode { format: format.into(), message: message.into() }
    }
}
