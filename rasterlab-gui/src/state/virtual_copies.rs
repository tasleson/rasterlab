//! Virtual copy management for RasterLab.
//!
//! A [`VirtualCopyStore`] holds an ordered list of named [`EditPipeline`]s,
//! all sharing the same source `Arc<Image>`.  Only the active copy is rendered
//! and displayed at any time; switching copies triggers a fresh render.

use std::sync::Arc;

use rasterlab_core::{
    error::RasterResult, image::Image, pipeline::EditPipeline, project::SavedCopy,
};

/// Manages the set of virtual copies for the currently open image.
///
/// Invariant: `copies` is always non-empty; `active < copies.len()`.
pub struct VirtualCopyStore {
    copies: Vec<(String, EditPipeline)>,
    active: usize,
}

impl VirtualCopyStore {
    /// Construct the initial store from the first (and only) pipeline.
    pub fn new(name: String, pipeline: EditPipeline) -> Self {
        Self {
            copies: vec![(name, pipeline)],
            active: 0,
        }
    }

    /// Create a new empty virtual copy that shares the source image.
    /// The new copy becomes active.
    pub fn add_copy(&mut self, name: String) {
        let source = Arc::clone(self.copies[self.active].1.source());
        let pipeline = EditPipeline::new_virtual_copy(source);
        self.copies.push((name, pipeline));
        self.active = self.copies.len() - 1;
    }

    /// Duplicate the active copy (same op list), making the duplicate active.
    pub fn duplicate_active(&mut self, name: String) -> RasterResult<()> {
        let source = Arc::clone(self.copies[self.active].1.source());
        let state = self.copies[self.active].1.save_state()?;
        let mut pipeline = EditPipeline::new_virtual_copy(source);
        pipeline.load_state(state)?;
        self.copies.push((name, pipeline));
        self.active = self.copies.len() - 1;
        Ok(())
    }

    /// Remove the copy at `index`.  Refuses (returns `false`) when only one
    /// copy remains.  Clamps or adjusts `active` as needed.
    pub fn remove(&mut self, index: usize) -> bool {
        if self.copies.len() <= 1 || index >= self.copies.len() {
            return false;
        }
        self.copies.remove(index);
        if self.active >= self.copies.len() {
            self.active = self.copies.len() - 1;
        } else if self.active > index {
            self.active -= 1;
        }
        true
    }

    /// Rename the copy at `index`.
    pub fn rename(&mut self, index: usize, name: String) {
        if index < self.copies.len() {
            self.copies[index].0 = name;
        }
    }

    /// Set the active copy by index.
    pub fn set_active(&mut self, index: usize) {
        if index < self.copies.len() {
            self.active = index;
        }
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active_name(&self) -> &str {
        &self.copies[self.active].0
    }

    pub fn active_pipeline(&self) -> &EditPipeline {
        &self.copies[self.active].1
    }

    pub fn active_pipeline_mut(&mut self) -> &mut EditPipeline {
        &mut self.copies[self.active].1
    }

    pub fn len(&self) -> usize {
        self.copies.len()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.copies.iter().map(|(n, _)| n.as_str())
    }

    /// The source image shared by all copies.
    pub fn source(&self) -> &Arc<Image> {
        self.copies[0].1.source()
    }

    /// Serialise all copies for project save.
    /// Returns `(copies, active_index)`.
    pub fn save_states(&self) -> RasterResult<(Vec<SavedCopy>, usize)> {
        let copies = self
            .copies
            .iter()
            .map(|(name, pipeline)| {
                pipeline.save_state().map(|pipeline_state| SavedCopy {
                    name: name.clone(),
                    pipeline_state,
                })
            })
            .collect::<RasterResult<Vec<_>>>()?;
        Ok((copies, self.active))
    }

    /// Reconstruct from saved data.  All pipelines share `source` via
    /// `Arc::clone` — zero pixel data is copied.
    pub fn load_from_saved(
        source: Arc<Image>,
        saved: Vec<SavedCopy>,
        active: usize,
    ) -> RasterResult<Self> {
        let copies = saved
            .into_iter()
            .map(|sc| {
                let mut pipeline = EditPipeline::new_virtual_copy(Arc::clone(&source));
                pipeline.load_state(sc.pipeline_state)?;
                Ok((sc.name, pipeline))
            })
            .collect::<RasterResult<Vec<_>>>()?;
        let active = active.min(copies.len().saturating_sub(1));
        Ok(Self { copies, active })
    }
}
