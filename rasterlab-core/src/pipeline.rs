use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::{
    error::{RasterError, RasterResult},
    image::Image,
    traits::operation::Operation,
};

/// An entry in the edit stack: an operation plus its metadata.
#[derive(Serialize, Deserialize)]
pub struct EditEntry {
    /// Stable unique ID within this pipeline (monotonically increasing).
    pub id: u64,
    /// Whether the operation is active.  Disabled ops are skipped during render.
    pub enabled: bool,
    /// The operation itself (polymorphically serialisable via typetag).
    pub operation: Box<dyn Operation>,
}

impl std::fmt::Debug for EditEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditEntry")
            .field("id", &self.id)
            .field("enabled", &self.enabled)
            .field("op", &self.operation.name())
            .finish()
    }
}

/// Serialisable snapshot of a pipeline (used for save/load).
#[derive(Debug, Serialize, Deserialize)]
pub struct PipelineState {
    /// Serialised edit entries (each includes operation type + parameters).
    pub entries: Vec<serde_json::Value>,
    /// Cursor position at save time (undo history depth).
    pub cursor: usize,
}

/// The non-destructive editing pipeline.
///
/// Stores the original source image plus an ordered list of [`EditEntry`] items.
/// Rendering walks `ops[0..cursor]`, applying each enabled operation in sequence.
///
/// ## Undo / Redo
///
/// `cursor` acts as the undo point:
/// - `push_op` appends after the cursor and advances it (truncates redo history).
/// - `undo` decrements cursor without removing ops.
/// - `redo` increments cursor.
///
/// ## Caching
///
/// The last rendered `(cursor, Arc<Image>)` pair is memoised.  Any mutation that
/// affects the active range invalidates the cache so the next `render()` recomputes.
pub struct EditPipeline {
    source: Arc<Image>,
    ops: Vec<EditEntry>,
    cursor: usize,
    /// `Some((cursor, rendered))` when the cache is valid.
    cache: Option<(usize, Arc<Image>)>,
    next_id: u64,
}

impl EditPipeline {
    /// Create a new pipeline with `source` as the immutable base image.
    pub fn new(source: Image) -> Self {
        Self {
            source: Arc::new(source),
            ops: Vec::new(),
            cursor: 0,
            cache: None,
            next_id: 1,
        }
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Append an operation after the current cursor, truncating any redo history.
    pub fn push_op(&mut self, operation: Box<dyn Operation>) {
        self.ops.truncate(self.cursor);
        let id = self.next_id;
        self.next_id += 1;
        self.ops.push(EditEntry {
            id,
            enabled: true,
            operation,
        });
        self.cursor = self.ops.len();
        self.cache = None;
    }

    /// Remove the operation at `index`.  Returns `false` if out of range.
    pub fn remove_op(&mut self, index: usize) -> bool {
        if index >= self.ops.len() {
            return false;
        }
        self.ops.remove(index);
        if self.cursor > index {
            self.cursor = self.cursor.saturating_sub(1);
        }
        self.invalidate_cache_from(index);
        true
    }

    /// Move operation from `from` to `to`.  Returns `false` if either index is out of range.
    pub fn reorder_op(&mut self, from: usize, to: usize) -> bool {
        if from >= self.ops.len() || to >= self.ops.len() {
            return false;
        }
        let entry = self.ops.remove(from);
        self.ops.insert(to, entry);
        self.invalidate_cache_from(from.min(to));
        true
    }

    /// Toggle the `enabled` flag of the operation at `index`.
    pub fn toggle_op(&mut self, index: usize) -> bool {
        if let Some(entry) = self.ops.get_mut(index) {
            entry.enabled = !entry.enabled;
            if index < self.cursor {
                self.cache = None;
            }
            true
        } else {
            false
        }
    }

    // -----------------------------------------------------------------------
    // Undo / Redo
    // -----------------------------------------------------------------------

    /// Move the cursor one step back (undo).  Returns `false` if at the beginning.
    pub fn undo(&mut self) -> bool {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.cache = None;
            true
        } else {
            false
        }
    }

    /// Move the cursor one step forward (redo).  Returns `false` if at the end.
    pub fn redo(&mut self) -> bool {
        if self.cursor < self.ops.len() {
            self.cursor += 1;
            self.cache = None;
            true
        } else {
            false
        }
    }

    // -----------------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------------

    /// Render the image by applying `ops[0..cursor]` to the source.
    ///
    /// Uses the internal cache when the cursor hasn't changed.
    pub fn render(&mut self) -> RasterResult<Arc<Image>> {
        // Cache hit
        if let Some((c, ref img)) = self.cache
            && c == self.cursor
        {
            return Ok(Arc::clone(img));
        }

        let mut current = self.source.deep_clone();
        for entry in &self.ops[..self.cursor] {
            if entry.enabled {
                current = entry.operation.apply(&current).map_err(|e| {
                    RasterError::Pipeline(format!(
                        "Operation '{}' failed: {}",
                        entry.operation.name(),
                        e
                    ))
                })?;
            }
        }

        let result = Arc::new(current);
        self.cache = Some((self.cursor, Arc::clone(&result)));
        Ok(result)
    }

    /// Render at a reduced scale for fast preview.
    ///
    /// `scale` must be in `(0.0, 1.0]`.  A scale of `0.25` renders at 25% of
    /// the source resolution, which is much faster for live feedback.
    ///
    /// Note: the result is not cached separately from the full-res render.
    pub fn render_preview(&mut self, scale: f32) -> RasterResult<Arc<Image>> {
        let scale = scale.clamp(0.01, 1.0);
        if (scale - 1.0).abs() < f32::EPSILON {
            return self.render();
        }

        let full = self.render()?;
        let pw = ((full.width as f32 * scale) as u32).max(1);
        let ph = ((full.height as f32 * scale) as u32).max(1);

        let mut preview = Image::new(pw, ph);
        let x_ratio = full.width as f32 / pw as f32;
        let y_ratio = full.height as f32 / ph as f32;

        for py in 0..ph {
            for px in 0..pw {
                let sx = px as f32 * x_ratio;
                let sy = py as f32 * y_ratio;
                preview.set_pixel(px, py, full.sample_bilinear(sx, sy));
            }
        }
        Ok(Arc::new(preview))
    }

    // -----------------------------------------------------------------------
    // Persistence
    // -----------------------------------------------------------------------

    /// Serialise the current pipeline to a JSON value for saving.
    pub fn save_state(&self) -> RasterResult<PipelineState> {
        let entries = self
            .ops
            .iter()
            .map(|e| serde_json::to_value(e).map_err(|e| RasterError::Serialization(e.to_string())))
            .collect::<RasterResult<Vec<_>>>()?;
        Ok(PipelineState {
            entries,
            cursor: self.cursor,
        })
    }

    /// Replace the current pipeline contents with a deserialised state.
    pub fn load_state(&mut self, state: PipelineState) -> RasterResult<()> {
        let entries: Vec<EditEntry> = state
            .entries
            .into_iter()
            .map(|v| {
                serde_json::from_value(v).map_err(|e| RasterError::Serialization(e.to_string()))
            })
            .collect::<RasterResult<Vec<_>>>()?;
        self.ops = entries;
        self.cursor = state.cursor.min(self.ops.len());
        self.cache = None;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Accessors
    // -----------------------------------------------------------------------

    pub fn ops(&self) -> &[EditEntry] {
        &self.ops
    }
    pub fn cursor(&self) -> usize {
        self.cursor
    }
    pub fn source(&self) -> &Arc<Image> {
        &self.source
    }

    pub fn can_undo(&self) -> bool {
        self.cursor > 0
    }
    pub fn can_redo(&self) -> bool {
        self.cursor < self.ops.len()
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn invalidate_cache_from(&mut self, from_index: usize) {
        if let Some((cached_cursor, _)) = self.cache
            && cached_cursor > from_index
        {
            self.cache = None;
        }
    }
}
