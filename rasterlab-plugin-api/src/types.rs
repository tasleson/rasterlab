//! Shared data types that cross the C ABI boundary.
//!
//! All types in this module are `#[repr(C)]` and contain only POD fields
//! (integers, raw pointers, or other `repr(C)` structs).  No Rust types
//! (String, Vec, Box, …) may appear here.

use core::ffi::c_char;

/// Pixel format tag understood by both host and plugin.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CPixelFormat {
    /// 4 bytes per pixel: R, G, B, A (all u8, straight alpha).
    Rgba8 = 0,
}

/// A flat image buffer passed across the ABI boundary.
///
/// The host allocates this before calling an operation; the plugin fills
/// `out_data` with a freshly allocated buffer that the host later frees via
/// [`free_image_data`].
///
/// # Safety
/// `data` must point to `width * height * 4` bytes when `format == Rgba8`.
#[repr(C)]
pub struct CImage {
    pub width: u32,
    pub height: u32,
    pub format: CPixelFormat,
    /// Pointer to pixel bytes.  Owned by whoever allocated it.
    pub data: *mut u8,
    /// Byte length of the `data` buffer (`width * height * bytes_per_pixel`).
    pub data_len: usize,
}

/// Error codes returned by plugin operations.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum COperationStatus {
    Ok = 0,
    InvalidParams = -1,
    AllocationFailed = -2,
    InternalError = -3,
    ApiVersionMismatch = -4,
}

/// Free a `CImage.data` buffer that was allocated inside a plugin.
///
/// # Safety
/// Must only be called with a pointer that was returned by a plugin operation.
/// Must be called exactly once per allocation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn rasterlab_free_image_data(ptr: *mut u8, len: usize) {
    if !ptr.is_null() && len > 0 {
        // Reconstruct the Vec so Rust's allocator frees the memory correctly.
        // SAFETY: ptr was allocated by Vec<u8> with capacity=len in alloc_cimage.
        unsafe {
            let _ = Vec::from_raw_parts(ptr, len, len);
        }
    }
}

/// Null-terminated UTF-8 string metadata for a plugin.
#[repr(C)]
pub struct CPluginMetadata {
    // Safety: fields are pointers to static string literals (read-only, never mutated).
    /// Plugin display name (e.g. "Sepia Tone").
    pub name: *const c_char,
    /// SemVer string (e.g. "1.0.0").
    pub version: *const c_char,
    /// Author / vendor string.
    pub author: *const c_char,
    /// Short description shown in the plugin manager UI.
    pub description: *const c_char,
}

// SAFETY: CPluginMetadata only contains pointers to static string literals.
// They are never mutated, so sharing across threads is safe.
unsafe impl Send for CPluginMetadata {}
unsafe impl Sync for CPluginMetadata {}
