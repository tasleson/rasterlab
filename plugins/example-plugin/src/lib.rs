//! Example RasterLab plugin: **Sepia Tone**
//!
//! Demonstrates the full plugin ABI:
//! - Exports `rasterlab_plugin_init` returning a `*mut PluginVTable`.
//! - Implements a single `OperationVTable` (sepia tone filter).
//!
//! Build with `cargo build -p example-plugin` to produce `libexample_plugin.so`.
//! Then load it in rasterlab with `--plugin path/to/libexample_plugin.so`.

use std::ffi::c_char;

use rasterlab_plugin_api::{
    PLUGIN_API_VERSION,
    types::{CImage, COperationStatus, CPluginMetadata},
    vtable::{OperationVTable, PluginVTable, alloc_cimage},
};

// ---------------------------------------------------------------------------
// Static string constants
// ---------------------------------------------------------------------------

macro_rules! cstr {
    ($s:expr) => {
        concat!($s, "\0").as_ptr() as *const c_char
    };
}

// ---------------------------------------------------------------------------
// Operation: Sepia Tone
// ---------------------------------------------------------------------------

#[allow(dead_code)]
extern "C" fn sepia_name(_op: *const OperationVTable) -> *const c_char {
    cstr!("Sepia tone")
}

extern "C" fn sepia_apply(
    _op: *const OperationVTable,
    src: *const CImage,
    dst: *mut CImage,
) -> COperationStatus {
    // SAFETY: src is a valid CImage provided by the host.
    let (w, h, src_data) = unsafe {
        let img = &*src;
        let data = std::slice::from_raw_parts(img.data, img.data_len);
        (img.width, img.height, data)
    };

    // Allocate output buffer
    let out = unsafe { alloc_cimage(w, h) };
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

    // Write output image back
    unsafe {
        *dst = out;
    }
    COperationStatus::Ok
}

extern "C" fn sepia_describe(_op: *const OperationVTable) -> *const c_char {
    cstr!("Sepia tone")
}

extern "C" fn sepia_destroy(_op: *mut OperationVTable) {
    // Static vtable — nothing to free.
}

static SEPIA_VTABLE: OperationVTable = OperationVTable {
    name: cstr!("sepia_tone") as *const c_char,
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
        &SEPIA_VTABLE as *const OperationVTable as *mut OperationVTable
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
        name: cstr!("Sepia Tone Plugin"),
        version: cstr!("1.0.0"),
        author: cstr!("RasterLab Contributors"),
        description: cstr!("Applies a classic sepia tone warm toning effect"),
    },
    operation_count: plugin_op_count,
    get_operation: plugin_get_op,
    decoder_extensions: std::ptr::null(),
    destroy: plugin_destroy,
};

/// Entry point called by the plugin loader.
///
/// # Safety
/// Returns a pointer to a `'static` vtable — always valid for the library lifetime.
#[unsafe(no_mangle)]
pub extern "C" fn rasterlab_plugin_init() -> *mut PluginVTable {
    &PLUGIN_VTABLE as *const PluginVTable as *mut PluginVTable
}
