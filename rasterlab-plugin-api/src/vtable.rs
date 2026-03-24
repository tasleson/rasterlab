//! Function-pointer vtables that form the stable C ABI.
//!
//! The host never calls into plugin code except through these tables,
//! and the plugin never calls into host code except through `rasterlab_free_image_data`.
//! This keeps the boundary minimal and auditable.

use crate::types::{CImage, COperationStatus, CPixelFormat, CPluginMetadata};
use core::ffi::c_char;

/// Vtable for a single image-processing operation exposed by a plugin.
///
/// A plugin may expose multiple operations; each gets its own `OperationVTable`.
#[repr(C)]
pub struct OperationVTable {
    /// Operation name shown in the edit stack (null-terminated UTF-8).
    pub name: *const c_char,

    /// Human-readable description for the current parameter values.
    /// The returned pointer must remain valid until `destroy` is called.
    pub describe: unsafe extern "C" fn(op: *const OperationVTable) -> *const c_char,

    /// Apply the operation.
    ///
    /// # Parameters
    /// - `op`  - pointer to this vtable (allows stateful operations by casting to a larger struct)
    /// - `src` - source image (plugin must not free `src.data`)
    /// - `dst` - output image to fill; plugin must allocate `dst.data` via the system allocator
    ///   so the host can free it with [`rasterlab_free_image_data`]
    ///
    /// # Returns
    /// [`COperationStatus::Ok`] on success, otherwise an error code.
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
    /// Must equal [`PLUGIN_API_VERSION`].  Checked immediately by the loader.
    pub api_version: u32,

    /// Plugin identity information.
    pub metadata: CPluginMetadata,

    /// Number of operations this plugin exposes.
    pub operation_count: unsafe extern "C" fn() -> usize,

    /// Return the vtable for operation `index` (0-based).
    /// Returns null if `index >= operation_count()`.
    pub get_operation: unsafe extern "C" fn(index: usize) -> *mut OperationVTable,

    /// Optional: return a list of file extensions this plugin can decode
    /// (null-terminated array of null-terminated strings, or null).
    pub decoder_extensions: *const *const c_char,

    /// Free all resources held by this plugin.  Called when the library is unloaded.
    pub destroy: unsafe extern "C" fn(),
}

// SAFETY: OperationVTable and PluginVTable are designed to be used as global
// statics in plugin libraries.  The fn-pointer fields are not mutated after
// construction; raw-pointer fields point to static string literals.
unsafe impl Send for OperationVTable {}
unsafe impl Sync for OperationVTable {}
unsafe impl Send for PluginVTable {}
unsafe impl Sync for PluginVTable {}

/// Convenience: build a minimal `CImage` with a freshly allocated pixel buffer.
///
/// # Safety
/// Caller is responsible for eventually calling `rasterlab_free_image_data` on `data`.
pub unsafe fn alloc_cimage(width: u32, height: u32) -> CImage {
    let len = (width * height * 4) as usize;
    let mut data = vec![0; len];
    let ptr = data.as_mut_ptr();
    core::mem::forget(data); // ownership transferred to CImage
    CImage {
        width,
        height,
        format: CPixelFormat::Rgba8,
        data: ptr,
        data_len: len,
    }
}

/// Signature of the `rasterlab_plugin_init` export that every plugin must provide.
pub type PluginInitFn = unsafe extern "C" fn() -> *mut PluginVTable;
