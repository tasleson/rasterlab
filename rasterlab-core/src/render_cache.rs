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

    /// Store sparse intermediate results produced by the render thread.
    ///
    /// Each tuple is `(relative_step, image)`, where `relative_step` is offset
    /// from `start`. This allows GPU runs to cache only readback boundaries
    /// instead of forcing a CPU readback after every GPU operation.
    pub fn store_sparse(&mut self, start: usize, images: Vec<(usize, Arc<Image>)>) {
        let Some(max_step) = images.iter().map(|(step, _)| start + *step).max() else {
            return;
        };
        if self.steps.len() <= max_step {
            self.steps.resize(max_step + 1, None);
        }
        for (step, img) in images {
            self.steps[start + step] = Some(img);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::Image;

    fn make_img() -> Arc<Image> {
        Arc::new(Image::new(1, 1))
    }

    fn make_source() -> Arc<Image> {
        make_img()
    }

    #[test]
    fn generation_starts_zero() {
        assert_eq!(RenderCache::new().generation(), 0);
    }

    #[test]
    fn empty_cache_returns_source_at_zero() {
        let cache = RenderCache::new();
        let src = make_source();
        let (idx, img) = cache.best_start(&src, 5);
        assert_eq!(idx, 0);
        assert!(Arc::ptr_eq(&img, &src));
    }

    #[test]
    fn store_and_best_start() {
        let mut cache = RenderCache::new();
        let src = make_source();
        let stored = make_img();
        cache.store(0, Arc::clone(&stored));
        let (idx, img) = cache.best_start(&src, 2);
        assert_eq!(idx, 1);
        assert!(Arc::ptr_eq(&img, &stored));
    }

    #[test]
    fn best_start_returns_closest() {
        let mut cache = RenderCache::new();
        let src = make_source();
        let img0 = make_img();
        let img2 = make_img();
        cache.store(0, Arc::clone(&img0));
        cache.store(2, Arc::clone(&img2));
        let (idx, img) = cache.best_start(&src, 5);
        assert_eq!(idx, 3);
        assert!(Arc::ptr_eq(&img, &img2));
    }

    #[test]
    fn invalidate_from_clears_and_bumps_gen() {
        let mut cache = RenderCache::new();
        let src = make_source();
        let img0 = make_img();
        cache.store(0, Arc::clone(&img0));
        cache.store(1, make_img());
        cache.store(2, make_img());
        let gen_before = cache.generation();
        cache.invalidate_from(1);
        assert!(cache.generation() > gen_before);
        // img at index 0 is still present
        let (idx, img) = cache.best_start(&src, 3);
        assert_eq!(idx, 1);
        assert!(Arc::ptr_eq(&img, &img0));
    }

    #[test]
    fn truncate_shrinks() {
        let mut cache = RenderCache::new();
        let src = make_source();
        let img0 = make_img();
        cache.store(0, Arc::clone(&img0));
        cache.store(1, make_img());
        cache.store(2, make_img());
        cache.truncate(1);
        let (idx, img) = cache.best_start(&src, 3);
        assert_eq!(idx, 1);
        assert!(Arc::ptr_eq(&img, &img0));
    }

    #[test]
    fn clear_empties_and_bumps_gen() {
        let mut cache = RenderCache::new();
        let src = make_source();
        cache.store(0, make_img());
        let gen_before = cache.generation();
        cache.clear();
        assert!(cache.generation() > gen_before);
        let (idx, _) = cache.best_start(&src, 3);
        assert_eq!(idx, 0);
    }

    #[test]
    fn take_start_vacates_slot() {
        let mut cache = RenderCache::new();
        let src = make_source();
        let stored = make_img();
        cache.store(0, Arc::clone(&stored));
        let (idx, img) = cache.take_start(&src, 2);
        assert_eq!(idx, 1);
        assert!(Arc::ptr_eq(&img, &stored));
        // Slot is now vacated — falls back to source
        let (idx2, img2) = cache.best_start(&src, 2);
        assert_eq!(idx2, 0);
        assert!(Arc::ptr_eq(&img2, &src));
    }

    #[test]
    fn store_batch_fills_correctly() {
        let mut cache = RenderCache::new();
        let src = make_source();
        let img_a = make_img();
        let img_b = make_img();
        cache.store_batch(1, vec![Arc::clone(&img_a), Arc::clone(&img_b)]);
        let (idx, img) = cache.best_start(&src, 4);
        assert_eq!(idx, 3);
        assert!(Arc::ptr_eq(&img, &img_b));
    }

    #[test]
    fn store_sparse_fills_at_right_positions() {
        let mut cache = RenderCache::new();
        let src = make_source();
        let img_a = make_img();
        let img_b = make_img();
        cache.store_sparse(1, vec![(0, Arc::clone(&img_a)), (2, Arc::clone(&img_b))]);
        let (idx, img) = cache.best_start(&src, 5);
        assert_eq!(idx, 4);
        assert!(Arc::ptr_eq(&img, &img_b));
    }
}
