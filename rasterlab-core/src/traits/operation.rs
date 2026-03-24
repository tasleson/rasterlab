use crate::{error::RasterResult, image::Image};

/// A single non-destructive editing step.
///
/// Operations are stored in an [`EditPipeline`][crate::pipeline::EditPipeline] and
/// applied sequentially to produce a rendered output.  They never mutate their
/// input — [`apply`][Operation::apply] always returns a fresh `Image`.
///
/// # Implementing a custom operation
///
/// ```rust
/// use rasterlab_core::traits::operation::Operation;
/// use rasterlab_core::image::Image;
/// use rasterlab_core::error::RasterResult;
/// use serde::{Serialize, Deserialize};
///
/// #[derive(Debug, Clone, Serialize, Deserialize)]
/// pub struct InvertOp;
///
/// #[typetag::serde]
/// impl Operation for InvertOp {
///     fn name(&self) -> &'static str { "invert" }
///
///     fn apply(&self, image: &Image) -> RasterResult<Image> {
///         let mut out = image.deep_clone();
///         out.data.chunks_mut(4).for_each(|p| {
///             p[0] = 255 - p[0];
///             p[1] = 255 - p[1];
///             p[2] = 255 - p[2];
///         });
///         Ok(out)
///     }
///
///     fn describe(&self) -> String { "Invert colours".into() }
/// }
/// ```
///
/// The `#[typetag::serde]` attribute makes `Box<dyn Operation>` serialisable,
/// enabling round-trip of the full pipeline to/from JSON.
#[typetag::serde(tag = "type")]
pub trait Operation: Send + Sync {
    /// Short stable identifier used for serialisation (snake_case, no spaces).
    fn name(&self) -> &'static str;

    /// Apply this operation to `image` and return the result.
    ///
    /// Must not mutate `image`.  Should be deterministic.
    fn apply(&self, image: &Image) -> RasterResult<Image>;

    /// Human-readable summary for display in the edit stack UI.
    fn describe(&self) -> String;
}
