//! Function-pointer vtables that form the stable C ABI.
//!
//! The host never calls into plugin code except through these tables, and the
//! plugin never calls into host code at all.  This keeps the boundary minimal
//! and auditable.
//!
//! ## Allocation ownership
//!
//! Memory crosses this boundary in one direction only: a plugin allocates the
//! output image and the host consumes it.  The host copies those bytes into its
//! own [`Vec`] and then hands the buffer straight back through
//! [`PluginVTable::free_image_data`], so **every allocation is released by the
//! allocator that made it**.  Host and plugin are separate shared objects with
//! their own Rust allocator instances (and, on Windows, potentially their own C
//! runtimes); freeing a plugin's buffer with the host's allocator is undefined
//! behaviour even when both were built by the same compiler.
//!
//! The same reasoning rules out an exported free function: a `#[no_mangle]`
//! symbol would appear in both the host binary and the plugin, and ELF symbol
//! interposition would let either side's call bind to the other side's copy.
//! Routing through a function pointer in the plugin's own vtable is
//! unambiguous.

use crate::types::{CImage, COperationStatus, CPixelFormat, CPluginMetadata, rgba8_byte_len};
use core::ffi::c_char;

/// Vtable for a single image-processing operation exposed by a plugin.
///
/// A plugin may expose multiple operations; each gets its own `OperationVTable`.
///
/// # Safety
/// Every function-pointer field must be non-null.  Rust has no representation
/// for a null `extern "C" fn`, so the host cannot check this and a null entry is
/// undefined behaviour rather than a load error — see the crate-level notes on
/// plugins as trusted code.
#[repr(C)]
pub struct OperationVTable {
    /// Operation name shown in the edit stack (null-terminated UTF-8).
    /// Must remain valid for the lifetime of the loaded library.
    pub name: *const c_char,

    /// Human-readable description for the current parameter values.
    /// The returned pointer must remain valid until `destroy` is called.
    pub describe: unsafe extern "C" fn(op: *const OperationVTable) -> *const c_char,

    /// Apply the operation.
    ///
    /// # Parameters
    /// - `op`  - pointer to this vtable (allows stateful operations by casting to a larger struct)
    /// - `src` - source image (plugin must not free or mutate `src.data`)
    /// - `dst` - zeroed output image to fill; the plugin allocates `dst.data`
    ///   with [`alloc_cimage`] so that [`PluginVTable::free_image_data`] can
    ///   release it
    ///
    /// The plugin must leave `dst` internally consistent: `data_len` equal to
    /// `width * height * 4` and `format` a tag this ABI version defines.  The
    /// host validates that before reading a single pixel and rejects the
    /// operation otherwise.
    ///
    /// # Returns
    /// [`COperationStatus::Ok`] on success, otherwise an error code.  On any
    /// other status the host ignores `dst` entirely, so the plugin must not
    /// leave an allocation there.
    pub apply: unsafe extern "C" fn(
        op: *const OperationVTable,
        src: *const CImage,
        dst: *mut CImage,
    ) -> COperationStatus,

    /// Release any resources owned by this operation instance.
    /// Called exactly once when the host removes the operation from the pipeline.
    pub destroy: unsafe extern "C" fn(op: *mut OperationVTable),
}

/// Top-level vtable returned by `rasterlab_plugin_init`.
///
/// Plugins must keep this struct (and all strings / sub-vtables it references)
/// alive for the lifetime of the loaded library.
#[repr(C)]
pub struct PluginVTable {
    /// Must equal [`PLUGIN_API_VERSION`][crate::PLUGIN_API_VERSION].
    /// Checked immediately by the loader.
    pub api_version: u32,

    /// Plugin identity information.
    pub metadata: CPluginMetadata,

    /// Number of operations this plugin exposes.  The host refuses to enumerate
    /// more than [`MAX_OPERATIONS`].
    pub operation_count: unsafe extern "C" fn() -> usize,

    /// Return the vtable for operation `index` (0-based).
    /// Returns null if `index >= operation_count()`.
    pub get_operation: unsafe extern "C" fn(index: usize) -> *mut OperationVTable,

    /// Release a pixel buffer this plugin allocated for an
    /// [`OperationVTable::apply`] output.
    ///
    /// Set this to [`rasterlab_free_image_data`][crate::types::rasterlab_free_image_data]
    /// unless the plugin allocates output buffers some other way; either way the
    /// function must run *inside the plugin* so the buffer goes back to the
    /// allocator that produced it.  The host calls it exactly once per
    /// successful `apply`, including on the paths where it rejects the returned
    /// image as inconsistent, and passes back exactly the `data` pointer and
    /// `data_len` the plugin wrote into `dst` — so a plugin that publishes a
    /// length other than its allocation's is asked to free that same wrong
    /// length.
    pub free_image_data: unsafe extern "C" fn(ptr: *mut u8, len: usize),

    /// Optional: return a list of file extensions this plugin can decode
    /// (null-terminated array of null-terminated strings, or null).
    pub decoder_extensions: *const *const c_char,

    /// Free all resources held by this plugin.  Called when the library is unloaded.
    pub destroy: unsafe extern "C" fn(),
}

/// Ceiling on `operation_count()`.  A plugin exposing more than this is
/// reporting a garbage count, not a real operation set, and the host stops
/// rather than calling `get_operation` that many times.
pub const MAX_OPERATIONS: usize = 1024;

// SAFETY: OperationVTable and PluginVTable are designed to be used as global
// statics in plugin libraries.  The fn-pointer fields are not mutated after
// construction; raw-pointer fields point to static string literals.
unsafe impl Send for OperationVTable {}
unsafe impl Sync for OperationVTable {}
unsafe impl Send for PluginVTable {}
unsafe impl Sync for PluginVTable {}

/// Allocate a zeroed RGBA8 `CImage` for an operation's output.
///
/// Returns `None` when `width × height × 4` does not fit a `usize`, so a
/// nonsense size becomes a [`COperationStatus::AllocationFailed`] rather than a
/// wrapped length and an out-of-bounds write.
///
/// The buffer must be released by
/// [`rasterlab_free_image_data`][crate::types::rasterlab_free_image_data]
/// compiled into the *same* library as this call — which is what happens when
/// the host goes through [`PluginVTable::free_image_data`].
pub fn alloc_cimage(width: u32, height: u32) -> Option<CImage> {
    let len = rgba8_byte_len(width, height)?;
    let mut data = vec![0u8; len];
    let ptr = data.as_mut_ptr();
    core::mem::forget(data); // ownership transferred to CImage
    Some(CImage {
        width,
        height,
        format: CPixelFormat::Rgba8,
        data: ptr,
        data_len: len,
    })
}

/// Signature of the `rasterlab_plugin_init` export that every plugin must provide.
pub type PluginInitFn = unsafe extern "C" fn() -> *mut PluginVTable;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::rasterlab_free_image_data;

    #[test]
    fn alloc_cimage_rejects_sizes_that_do_not_fit() {
        // 2^64 bytes: the product overflows before any allocation is attempted.
        assert!(alloc_cimage(u32::MAX, u32::MAX).is_none());
    }

    #[test]
    fn alloc_cimage_produces_a_consistent_image() {
        let img = alloc_cimage(4, 3).expect("4x3 fits");
        assert!(img.is_consistent());
        assert_eq!(img.data_len, 48);
        // SAFETY: freeing the allocation this test just made, once.
        unsafe { rasterlab_free_image_data(img.data, img.data_len) };
    }

    #[test]
    fn a_zero_sized_image_allocates_nothing_and_is_not_consistent() {
        let img = alloc_cimage(0, 0).expect("0x0 is arithmetically fine");
        assert_eq!(img.data_len, 0);
        assert!(!img.is_consistent());
        // SAFETY: a no-op for a zero length, matching the allocation.
        unsafe { rasterlab_free_image_data(img.data, img.data_len) };
    }
}
