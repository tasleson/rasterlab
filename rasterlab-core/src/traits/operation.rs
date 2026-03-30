use crate::{error::RasterResult, image::Image};

/// A single non-destructive editing step.
///
/// Operations are stored in an [`EditPipeline`][crate::pipeline::EditPipeline] and
/// applied sequentially to produce a rendered output.
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
///     fn clone_box(&self) -> Box<dyn Operation> { Box::new(self.clone()) }
///
///     fn apply(&self, mut image: Image) -> RasterResult<Image> {
///         image.data.chunks_mut(4).for_each(|p| {
///             p[0] = 255 - p[0];
///             p[1] = 255 - p[1];
///             p[2] = 255 - p[2];
///         });
///         Ok(image)
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

    /// Clone this operation into a new heap-allocated trait object.
    ///
    /// Used to send operations to the background render thread without a
    /// serde round-trip.  All built-in operations derive `Clone` and
    /// implement this as `Box::new(self.clone())`.
    fn clone_box(&self) -> Box<dyn Operation>;

    /// Apply this operation to `image` and return the result.
    ///
    /// Takes `image` by value so pixel-mapped ops can mutate in place without
    /// allocating a new buffer.  Ops that need both source and destination
    /// buffers (convolutions, spatial transforms) borrow `&image.data` while
    /// writing into a separately allocated output.  Should be deterministic.
    fn apply(&self, image: Image) -> RasterResult<Image>;

    /// Human-readable summary for display in the edit stack UI.
    fn describe(&self) -> String;
}
