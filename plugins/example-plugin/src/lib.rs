//! Example RasterLab plugin: **Sepia Tone**
//!
//! Demonstrates the full plugin ABI:
//! - Exports `rasterlab_plugin_init` returning a `*mut PluginVTable`.
//! - Implements a single `OperationVTable` (sepia tone filter).
//! - Allocates its output with `alloc_cimage` and installs the matching
//!   deallocator in `PluginVTable::free_image_data`, so the buffer goes back to
//!   the allocator inside this library rather than the host's.
//!
//! Build with `cargo build -p example-plugin` to produce `libexample_plugin.so`.
//! The host loads it through `rasterlab_core::plugin_loader::PluginRegistry`;
//! there is no command-line flag for loading plugins.
//!
//! A plugin is trusted native code — see the `rasterlab_plugin_api` crate docs.

use std::ffi::c_char;

use rasterlab_plugin_api::{
    PLUGIN_API_VERSION,
    types::{CImage, COperationStatus, CPluginMetadata, rasterlab_free_image_data},
    vtable::{OperationVTable, PluginVTable, alloc_cimage},
};

// ---------------------------------------------------------------------------
// Operation: Sepia Tone
// ---------------------------------------------------------------------------

extern "C" fn sepia_apply(
    _op: *const OperationVTable,
    src: *const CImage,
    dst: *mut CImage,
) -> COperationStatus {
    if src.is_null() || dst.is_null() {
        return COperationStatus::InvalidParams;
    }

    // SAFETY: src is a valid CImage provided by the host.
    let src = unsafe { &*src };
    // Check the host's image the same way the host checks ours: the pointer,
    // the format tag, and the length against width × height × 4.
    if !src.is_consistent() {
        return COperationStatus::InvalidParams;
    }
    // SAFETY: is_consistent established that data covers data_len bytes.
    let src_data = unsafe { std::slice::from_raw_parts(src.data, src.data_len) };

    // Allocate the output.  alloc_cimage computes the length in u64, so an
    // impossible size fails here instead of wrapping into a short buffer.
    let Some(out) = alloc_cimage(src.width, src.height) else {
        return COperationStatus::AllocationFailed;
    };
    // SAFETY: alloc_cimage returned a buffer of exactly out.data_len bytes.
    let out_data = unsafe { std::slice::from_raw_parts_mut(out.data, out.data_len) };

    for (src_pixel, dst_pixel) in src_data.chunks_exact(4).zip(out_data.chunks_exact_mut(4)) {
        let (r, g, b) = (
            src_pixel[0] as f32,
            src_pixel[1] as f32,
            src_pixel[2] as f32,
        );

        let sr = (0.393 * r + 0.769 * g + 0.189 * b).min(255.0) as u8;
        let sg = (0.349 * r + 0.686 * g + 0.168 * b).min(255.0) as u8;
        let sb = (0.272 * r + 0.534 * g + 0.131 * b).min(255.0) as u8;

        dst_pixel[0] = sr;
        dst_pixel[1] = sg;
        dst_pixel[2] = sb;
        dst_pixel[3] = src_pixel[3]; // preserve alpha
    }

    // Write output image back.  The host now owns the right to have it freed,
    // which it exercises through PluginVTable::free_image_data below.
    // SAFETY: dst is a writable CImage supplied by the host.
    unsafe {
        *dst = out;
    }
    COperationStatus::Ok
}

extern "C" fn sepia_describe(_op: *const OperationVTable) -> *const c_char {
    c"Sepia tone".as_ptr()
}

extern "C" fn sepia_destroy(_op: *mut OperationVTable) {
    // Static vtable — nothing to free.
}

static SEPIA_VTABLE: OperationVTable = OperationVTable {
    name: c"sepia_tone".as_ptr(),
    describe: sepia_describe,
    apply: sepia_apply,
    destroy: sepia_destroy,
};

// ---------------------------------------------------------------------------
// Plugin vtable
// ---------------------------------------------------------------------------

extern "C" fn plugin_op_count() -> usize {
    1
}

extern "C" fn plugin_get_op(index: usize) -> *mut OperationVTable {
    if index == 0 {
        // Return a pointer to the static vtable.
        // SAFETY: static lifetime — pointer is always valid while the library is loaded.
        std::ptr::from_ref(&SEPIA_VTABLE).cast_mut()
    } else {
        std::ptr::null_mut()
    }
}

extern "C" fn plugin_destroy() {
    // Nothing to clean up for this stateless plugin.
}

static PLUGIN_VTABLE: PluginVTable = PluginVTable {
    api_version: PLUGIN_API_VERSION,
    metadata: CPluginMetadata {
        name: c"Sepia Tone Plugin".as_ptr(),
        version: c"1.0.0".as_ptr(),
        author: c"RasterLab Contributors".as_ptr(),
        description: c"Applies a classic sepia tone warm toning effect".as_ptr(),
    },
    operation_count: plugin_op_count,
    get_operation: plugin_get_op,
    // This crate's copy of the deallocator — compiled into *this* library, so
    // it releases the buffer through the same allocator alloc_cimage used.
    free_image_data: rasterlab_free_image_data,
    decoder_extensions: std::ptr::null(),
    destroy: plugin_destroy,
};

/// Entry point called by the plugin loader.
///
/// # Safety
/// Returns a pointer to a `'static` vtable — always valid for the library lifetime.
#[unsafe(no_mangle)]
pub extern "C" fn rasterlab_plugin_init() -> *mut PluginVTable {
    std::ptr::from_ref(&PLUGIN_VTABLE).cast_mut()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sepia_apply_produces_a_consistent_image_the_host_will_accept() {
        let mut pixels = [10u8, 120, 200, 255, 0, 0, 0, 64];
        let src = CImage {
            width: 2,
            height: 1,
            format: rasterlab_plugin_api::types::CPixelFormat::Rgba8,
            data: pixels.as_mut_ptr(),
            data_len: pixels.len(),
        };
        // SAFETY: a zeroed CImage is what the host passes in.
        let mut dst = unsafe { std::mem::zeroed::<CImage>() };

        // SAFETY: both images are valid for the duration of the call.
        let status = sepia_apply(std::ptr::null(), &src, &mut dst);
        assert_eq!(status, COperationStatus::Ok);
        assert!(dst.is_consistent());
        assert_eq!(dst.width, 2);
        assert_eq!(dst.height, 1);

        // SAFETY: dst is consistent, so data covers data_len bytes.
        let out = unsafe { std::slice::from_raw_parts(dst.data, dst.data_len) };
        assert!(out[0] > out[2], "sepia warms the red channel past the blue");
        assert_eq!(out[3], 255, "alpha is preserved");
        assert_eq!(out[7], 64);

        // SAFETY: freeing this library's allocation once, as the host would.
        unsafe { rasterlab_free_image_data(dst.data, dst.data_len) };
    }

    #[test]
    fn an_inconsistent_source_is_refused_rather_than_read() {
        let mut pixels = [0u8; 8];
        let src = CImage {
            width: 4, // claims 16 bytes; only 8 exist
            height: 1,
            format: rasterlab_plugin_api::types::CPixelFormat::Rgba8,
            data: pixels.as_mut_ptr(),
            data_len: pixels.len(),
        };
        // SAFETY: a zeroed CImage is what the host passes in.
        let mut dst = unsafe { std::mem::zeroed::<CImage>() };

        assert_eq!(
            sepia_apply(std::ptr::null(), &src, &mut dst),
            COperationStatus::InvalidParams
        );
        assert!(dst.data.is_null(), "nothing was allocated to leak");
    }
}
