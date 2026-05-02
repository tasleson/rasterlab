//! # rasterlab-render
//!
//! Background render thread and pipeline execution extracted from the GUI crate.
//! No `egui`/`eframe` dependency — rendering can be tested headlessly.

pub fn init_rayon_pool() {
    rayon::ThreadPoolBuilder::new()
        .stack_size(32 * 1024 * 1024)
        .build_global()
        .expect("failed to build rayon thread pool");
}
