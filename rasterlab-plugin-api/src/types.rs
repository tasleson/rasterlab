//! Shared data types that cross the C ABI boundary.
//!
//! All types in this module are `#[repr(C)]` and contain only POD fields
//! (integers, raw pointers, or other `repr(C)` structs).  No Rust types
//! (String, Vec, Box, …) may appear here.

use core::ffi::c_char;

/// Bytes per pixel for [`CPixelFormat::Rgba8`].
pub const RGBA8_BYTES_PER_PIXEL: usize = 4;

/// Byte length of an RGBA8 buffer holding `width × height` pixels.
///
/// `None` when the product overflows.  `width * height * 4` computed in `u32`
/// wraps for dimensions as ordinary as 65536², which in a release build yields a
/// buffer far too small for the dimensions recorded beside it — the classic way
/// a plugin boundary turns a size mistake into an out-of-bounds write.  Both
/// sides of the ABI size their allocations through this function.
pub fn rgba8_byte_len(width: u32, height: u32) -> Option<usize> {
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    let bytes = pixels.checked_mul(RGBA8_BYTES_PER_PIXEL as u64)?;
    usize::try_from(bytes).ok()
}

/// Pixel format tag understood by both host and plugin.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CPixelFormat {
    /// 4 bytes per pixel: R, G, B, A (all u8, straight alpha).
    Rgba8 = 0,
}

impl CPixelFormat {
    /// Discriminant of [`CPixelFormat::Rgba8`], for comparing against a raw tag
    /// read out of a [`CImage`] the host did not fill in itself.
    pub const RGBA8_TAG: u32 = CPixelFormat::Rgba8 as u32;
}

/// A flat image buffer passed across the ABI boundary.
///
/// The host fills in `src` and passes a zeroed `dst`; the plugin fills `dst`
/// with a freshly allocated buffer, which the host later releases through
/// [`PluginVTable::free_image_data`][crate::vtable::PluginVTable::free_image_data].
///
/// # Safety
/// `data` must point to `rgba8_byte_len(width, height)` bytes when
/// `format == Rgba8`, and `data_len` must equal that length.  The host rejects
/// any returned image where the two disagree, but it cannot detect a `data_len`
/// that is larger than the allocation it describes — that is a plugin bug that
/// reads out of bounds.
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

impl CImage {
    /// The `format` field as a raw discriminant.
    ///
    /// Reading `format` as a `CPixelFormat` is undefined behaviour when the
    /// other side of the boundary wrote a tag this ABI version does not define,
    /// which is exactly the case a receiver needs to detect.  Every bit pattern
    /// is a valid `u32`, so reading it as one is well defined; compare the
    /// result against [`CPixelFormat::RGBA8_TAG`].
    pub fn format_tag(&self) -> u32 {
        // SAFETY: CPixelFormat is repr(u32), so the field is four initialised
        // bytes at this address regardless of which tag was written into it.
        unsafe { core::ptr::read_unaligned(core::ptr::from_ref(&self.format).cast::<u32>()) }
    }

    /// Whether `format`, `data`, and `data_len` agree with `width × height`.
    ///
    /// Both sides call this on an image the other side produced, before
    /// constructing a slice from `data`.
    pub fn is_consistent(&self) -> bool {
        !self.data.is_null()
            && self.format_tag() == CPixelFormat::RGBA8_TAG
            && rgba8_byte_len(self.width, self.height) == Some(self.data_len)
            && self.data_len > 0
    }
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

/// Free a `CImage.data` buffer that was allocated by [`alloc_cimage`].
///
/// This is the function a plugin installs in
/// [`PluginVTable::free_image_data`][crate::vtable::PluginVTable::free_image_data].
/// It is deliberately **not** `#[no_mangle]`: host and plugin each link their
/// own copy of this crate, so an exported symbol of this name would exist twice
/// in the process and either side's call could bind to the other's copy.  The
/// buffer must be released by the allocator that produced it, which means the
/// copy compiled into the plugin — reached only through the plugin's own vtable.
///
/// # Safety
/// Must only be called with a pointer returned by [`alloc_cimage`] *in the same
/// shared library as this function*, with the matching length, exactly once.
///
/// [`alloc_cimage`]: crate::vtable::alloc_cimage
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
    /// Plugin display name (e.g. "Sepia Tone").  Required: the host rejects a
    /// plugin whose name is null, empty, or not UTF-8.
    pub name: *const c_char,
    /// SemVer string (e.g. "1.0.0").  May be null.
    pub version: *const c_char,
    /// Author / vendor string.  May be null.
    pub author: *const c_char,
    /// Short description shown in the plugin manager UI.  May be null.
    pub description: *const c_char,
}

// SAFETY: CPluginMetadata only contains pointers to static string literals.
// They are never mutated, so sharing across threads is safe.
unsafe impl Send for CPluginMetadata {}
unsafe impl Sync for CPluginMetadata {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba8_byte_len_is_checked() {
        assert_eq!(rgba8_byte_len(4, 3), Some(48));
        assert_eq!(rgba8_byte_len(0, 0), Some(0));
        // 65536² pixels is the smallest square that wraps a u32 byte count.
        assert_eq!(rgba8_byte_len(65536, 65536), Some(17_179_869_184));
        assert_eq!(rgba8_byte_len(u32::MAX, u32::MAX), None);
    }

    #[test]
    fn format_tag_reports_what_was_written() {
        let mut pixel = [0u8; 4];
        let mut img = CImage {
            width: 1,
            height: 1,
            format: CPixelFormat::Rgba8,
            data: pixel.as_mut_ptr(),
            data_len: 4,
        };
        assert_eq!(img.format_tag(), CPixelFormat::RGBA8_TAG);
        assert!(img.is_consistent());

        // A tag this ABI version does not define, as a plugin could leave behind.
        // SAFETY: writing four bytes into a repr(u32) field.  The field is never
        // read as a CPixelFormat afterwards — only through format_tag().
        unsafe {
            core::ptr::write_unaligned(core::ptr::from_mut(&mut img.format).cast::<u32>(), 7);
        }
        assert_eq!(img.format_tag(), 7);
        assert!(!img.is_consistent());
    }

    #[test]
    fn consistency_requires_length_to_match_dimensions() {
        let mut data = [0u8; 48];
        let mut img = CImage {
            width: 4,
            height: 3,
            format: CPixelFormat::Rgba8,
            data: data.as_mut_ptr(),
            data_len: 48,
        };
        assert!(img.is_consistent());

        img.data_len = 47;
        assert!(!img.is_consistent());

        img.data_len = 48;
        img.height = 4;
        assert!(!img.is_consistent());

        img.height = 3;
        img.data = core::ptr::null_mut();
        assert!(!img.is_consistent());
    }
}
