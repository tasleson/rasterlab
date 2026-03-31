use std::sync::Arc;

use crate::image::Image;

/// Intermediate image cache for the rendering pipeline.
///
/// Stores the result of each operation step so that only operations after
/// the last valid cache entry need to be re-executed on re-render.
///
/// The generationeration counter is bumped on every invalidation, allowing callers
/// (e.g. the GUI render thread) to detect stale data by comparing the value
/// before and after a render.
pub struct RenderCache {
    steps: Vec<Option<Arc<Image>>>,
    generation: u64,
}

impl Default for RenderCache {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderCache {
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            generation: 0,
        }
    }

    /// Find the best cached starting point for rendering `ops[0..cursor]`.
    ///
    /// Returns `(start_op_index, starting_image)`.  When nothing is cached
    /// the source image is returned with index 0.
    pub fn best_start(&self, source: &Arc<Image>, cursor: usize) -> (usize, Arc<Image>) {
        for i in (0..cursor).rev() {
            if let Some(Some(img)) = self.steps.get(i) {
                return (i + 1, Arc::clone(img));
            }
        }
        (0, Arc::clone(source))
    }

    /// Like [`best_start`] but vacates the cache slot so the caller receives
    /// the sole `Arc` reference (refcount = 1).
    ///
    /// This lets the render thread take exclusive ownership —
    /// `Arc::try_unwrap` will then succeed and the first operation avoids a
    /// full `deep_clone`.  The vacated slot is refilled by [`store_batch`]
    /// once the render completes.
    pub fn take_start(&mut self, source: &Arc<Image>, cursor: usize) -> (usize, Arc<Image>) {
        for i in (0..cursor).rev() {
            if let Some(slot @ Some(_)) = self.steps.get_mut(i) {
                return (i + 1, slot.take().unwrap());
            }
        }
        (0, Arc::clone(source))
    }

    /// Store an intermediate result at `step_idx`.
    pub fn store(&mut self, step_idx: usize, image: Arc<Image>) {
        if self.steps.len() <= step_idx {
            self.steps.resize(step_idx + 1, None);
        }
        self.steps[step_idx] = Some(image);
    }

    /// Store a batch of intermediate results produced by the render thread.
    ///
    /// `images[k]` is the image state after op `start + k` was processed.
    pub fn store_batch(&mut self, start: usize, images: Vec<Arc<Image>>) {
        let needed = start + images.len();
        if self.steps.len() < needed {
            self.steps.resize(needed, None);
        }
        for (k, img) in images.into_iter().enumerate() {
            self.steps[start + k] = Some(img);
        }
    }

    /// Invalidate all entries at `from` and beyond, bump generationeration.
    pub fn invalidate_from(&mut self, from: usize) {
        if from < self.steps.len() {
            self.steps.truncate(from);
            self.generation += 1;
        }
    }

    /// Truncate to `len` entries (used when push_op discards redo history).
    pub fn truncate(&mut self, len: usize) {
        self.steps.truncate(len);
    }

    /// Clear all entries and bump generationeration.
    pub fn clear(&mut self) {
        self.steps.clear();
        self.generation += 1;
    }

    /// Generation counter for concurrent mutation detection.
    ///
    /// Incremented whenever cached entries are invalidated.  Callers can
    /// snapshot this value before kicking off a render and compare it on
    /// completion to detect intervening mutations.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}
