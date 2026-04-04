//! Cooperative cancellation for long-running operations.
//!
//! A single process-global flag, because only one render thread runs at a
//! time (the GUI's `loading` gate enforces this).  Long-running ops in
//! `rasterlab-core::ops` poll [`is_requested`] at convenient checkpoints and
//! abort with [`crate::RasterError::Cancelled`] when it is set.
//!
//! Usage from the host:
//! 1. Call [`reset`] before spawning the render thread.
//! 2. Call [`request`] from any thread to ask the render to abort.
//! 3. Treat `RasterError::Cancelled` as a clean, non-fatal outcome.

use std::sync::atomic::{AtomicBool, Ordering};

static CANCEL_REQUESTED: AtomicBool = AtomicBool::new(false);

/// Clear any pending cancel request.  Call this at the start of each render.
pub fn reset() {
    CANCEL_REQUESTED.store(false, Ordering::Relaxed);
}

/// Ask the currently running operation to abort.  Safe to call from any thread.
pub fn request() {
    CANCEL_REQUESTED.store(true, Ordering::Relaxed);
}

/// Returns `true` if a cancel has been requested since the last [`reset`].
#[inline]
pub fn is_requested() -> bool {
    CANCEL_REQUESTED.load(Ordering::Relaxed)
}
