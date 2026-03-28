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
/// ## Step Cache
///
/// `step_cache[i]` holds the rendered image after processing `ops[0..=i]`
/// (disabled entries leave the image unchanged but still occupy a slot).
/// Any mutation affecting `ops[0..i]` calls `invalidate_steps_from(i)`, which
/// truncates the cache vector and bumps `step_cache_gen`.
///
/// The generation counter lets the GUI render thread detect whether the cache
/// was dirtied between the moment it took a snapshot and the moment it tries
/// to write new entries back, preventing stale data from being stored.
pub struct EditPipeline {
    source: Arc<Image>,
    ops: Vec<EditEntry>,
    cursor: usize,
    next_id: u64,
    /// Per-step intermediate result cache.
    /// `step_cache[i]` = image after ops[0..=i] (disabled ops leave image unchanged).
    step_cache: Vec<Option<Arc<Image>>>,
    /// Bumped on every invalidation so callers can detect concurrent mutations.
    step_cache_gen: u64,
}

impl EditPipeline {
    /// Create a new pipeline with `source` as the immutable base image.
    pub fn new(source: Image) -> Self {
        Self {
            source: Arc::new(source),
            ops: Vec::new(),
            cursor: 0,
            next_id: 1,
            step_cache: Vec::new(),
            step_cache_gen: 0,
        }
    }

    // -----------------------------------------------------------------------
    // Mutation
    // -----------------------------------------------------------------------

    /// Append an operation after the current cursor, truncating any redo history.
    pub fn push_op(&mut self, operation: Box<dyn Operation>) {
        // Discard redo history and its cached images.
        self.ops.truncate(self.cursor);
        self.step_cache.truncate(self.cursor);
        let id = self.next_id;
        self.next_id += 1;
        self.ops.push(EditEntry {
            id,
            enabled: true,
            operation,
        });
        self.cursor = self.ops.len();
        // step_cache has no entry for the new cursor-1 position yet — correct.
        // Entries [0..old_cursor] remain valid as the start for the new op.
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
        self.invalidate_steps_from(index);
        true
    }

    /// Move operation from `from` to `to`.  Returns `false` if either index is out of range.
    pub fn reorder_op(&mut self, from: usize, to: usize) -> bool {
        if from >= self.ops.len() || to >= self.ops.len() {
            return false;
        }
        let entry = self.ops.remove(from);
        self.ops.insert(to, entry);
        self.invalidate_steps_from(from.min(to));
        true
    }

    /// Toggle the `enabled` flag of the operation at `index`.
    pub fn toggle_op(&mut self, index: usize) -> bool {
        if let Some(entry) = self.ops.get_mut(index) {
            entry.enabled = !entry.enabled;
            if index < self.cursor {
                self.invalidate_steps_from(index);
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
    ///
    /// The step cache is **not** invalidated — cached images before the new
    /// cursor remain valid and will be reused on the next render.
    pub fn undo(&mut self) -> bool {
        if self.cursor > 0 {
            self.cursor -= 1;
            true
        } else {
            false
        }
    }

    /// Move the cursor one step forward (redo).  Returns `false` if at the end.
    pub fn redo(&mut self) -> bool {
        if self.cursor < self.ops.len() {
            self.cursor += 1;
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
    /// Starts from the best available cached intermediate result, so only
    /// operations after the last valid cache entry are re-executed.
    pub fn render(&mut self) -> RasterResult<Arc<Image>> {
        let (start_idx, mut current) = self.best_cached_start();

        if start_idx == self.cursor {
            return Ok(current);
        }

        for (k, entry) in self.ops[start_idx..self.cursor].iter().enumerate() {
            if entry.enabled {
                let img = match Arc::try_unwrap(current) {
                    Ok(img) => img,
                    Err(arc) => arc.as_ref().deep_clone(),
                };
                current = Arc::new(entry.operation.apply(img).map_err(|e| {
                    RasterError::Pipeline(format!(
                        "Operation '{}' failed: {}",
                        entry.operation.name(),
                        e
                    ))
                })?);
            }
            let op_idx = start_idx + k;
            if self.step_cache.len() <= op_idx {
                self.step_cache.resize(op_idx + 1, None);
            }
            self.step_cache[op_idx] = Some(Arc::clone(&current));
        }

        Ok(current)
    }

    /// Render at a reduced scale for fast preview.
    ///
    /// `scale` must be in `(0.0, 1.0]`.  A scale of `0.25` renders at 25% of
    /// the source resolution, which is much faster for live feedback.
    ///
    /// Note: the result is not stored in the step cache.
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
    // Step-cache management (called by GUI render thread coordination)
    // -----------------------------------------------------------------------

    /// Returns `(start_op_index, starting_image)`.
    ///
    /// `start_op_index` is the index of the first op that still needs to be
    /// applied; `starting_image` is the cached result after `ops[0..start_op_index]`.
    ///
    /// Returns `(cursor, cached_image)` when the full render is already cached,
    /// or `(0, source)` when nothing is cached.
    pub fn best_cached_start(&self) -> (usize, Arc<Image>) {
        for i in (0..self.cursor).rev() {
            if let Some(Some(img)) = self.step_cache.get(i) {
                return (i + 1, Arc::clone(img));
            }
        }
        (0, Arc::clone(&self.source))
    }

    /// Store a batch of intermediate results produced by the background render thread.
    ///
    /// `start_index` is the op position where the render began.  `images[k]`
    /// is the image state after op `start_index + k` was processed (unchanged
    /// if that op is disabled).
    ///
    /// Callers must guard this with a [`step_cache_gen`](Self::step_cache_gen)
    /// check to avoid writing stale data when a pipeline mutation occurred
    /// while the render was in flight.
    pub fn store_steps(&mut self, start_index: usize, images: Vec<Arc<Image>>) {
        let needed = start_index + images.len();
        if self.step_cache.len() < needed {
            self.step_cache.resize(needed, None);
        }
        for (k, img) in images.into_iter().enumerate() {
            self.step_cache[start_index + k] = Some(img);
        }
    }

    /// Generation counter for the step cache.
    ///
    /// Incremented whenever cached entries are invalidated.  Callers can
    /// snapshot this value before kicking off a render and compare it on
    /// completion to detect intervening mutations.
    pub fn step_cache_gen(&self) -> u64 {
        self.step_cache_gen
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
        self.step_cache.clear();
        self.step_cache_gen += 1;
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

    /// Discard all step-cache entries at `op_index` and beyond, and bump the
    /// generation counter so in-flight renders know their data is stale.
    fn invalidate_steps_from(&mut self, op_index: usize) {
        if op_index < self.step_cache.len() {
            self.step_cache.truncate(op_index);
            self.step_cache_gen += 1;
        }
    }
}
