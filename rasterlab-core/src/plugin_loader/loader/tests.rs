//! Loader and ABI tests driven by an in-process plugin.
//!
//! The fake plugin below is a real `PluginVTable` built with the same crate a
//! shared library would use, so these tests cover the vtable contract, the
//! validation the loader performs on what a plugin returns, and the ownership
//! rules for buffers that cross the boundary — without needing a `cdylib` on
//! disk, which would tie the test suite to a build layout and a target triple.
//! What they cannot cover is `dlopen` itself; that is exercised by
//! [`PluginLoader::load`]'s error paths at the bottom of this file.

use super::*;

use std::ffi::c_char;
use std::sync::atomic::{AtomicUsize, Ordering};

use rasterlab_plugin_api::types::{CPixelFormat, CPluginMetadata, rasterlab_free_image_data};
use rasterlab_plugin_api::vtable::alloc_cimage;

// ---------------------------------------------------------------------------
// A fake plugin, built the way a real one would be
// ---------------------------------------------------------------------------

/// What the fake operation should do on its next `apply`.  Selecting the
/// behaviour through a global forces the tests to run one at a time; see
/// [`serialised`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// Invert RGB, preserve alpha — a well-behaved operation.
    Invert,
    /// Report failure without allocating anything.
    Fail,
    /// Report success but leave `dst` zeroed.
    NullOutput,
    /// Allocate correctly, then publish dimensions the length no longer matches.
    MismatchedDimensions,
    /// Allocate correctly, then publish a pixel format tag this ABI has no name for.
    UnknownFormat,
}

static BEHAVIOUR: AtomicUsize = AtomicUsize::new(0);
/// Buffers released through the plugin's own deallocator.
static FREES: AtomicUsize = AtomicUsize::new(0);
/// `OperationVTable::destroy` calls.
static DESTROYS: AtomicUsize = AtomicUsize::new(0);
/// `PluginVTable::destroy` calls.
static PLUGIN_DESTROYS: AtomicUsize = AtomicUsize::new(0);

fn behaviour() -> Behaviour {
    match BEHAVIOUR.load(Ordering::SeqCst) {
        1 => Behaviour::Fail,
        2 => Behaviour::NullOutput,
        3 => Behaviour::MismatchedDimensions,
        4 => Behaviour::UnknownFormat,
        _ => Behaviour::Invert,
    }
}

/// Run `body` with the fake plugin set to `behaviour`, one test at a time.
///
/// The counters and the behaviour switch are process-global because the ABI
/// gives `get_operation` no context pointer to hang per-instance state off —
/// exactly the constraint a real plugin works under.
fn serialised<T>(behaviour: Behaviour, body: impl FnOnce() -> T) -> T {
    static LOCK: Mutex<()> = Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    BEHAVIOUR.store(behaviour as usize, Ordering::SeqCst);
    FREES.store(0, Ordering::SeqCst);
    DESTROYS.store(0, Ordering::SeqCst);
    PLUGIN_DESTROYS.store(0, Ordering::SeqCst);
    body()
}

extern "C" fn op_describe(_op: *const OperationVTable) -> *const c_char {
    c"Invert (fake plugin)".as_ptr()
}

extern "C" fn op_destroy(_op: *mut OperationVTable) {
    DESTROYS.fetch_add(1, Ordering::SeqCst);
}

extern "C" fn op_apply(
    _op: *const OperationVTable,
    src: *const CImage,
    dst: *mut CImage,
) -> COperationStatus {
    if behaviour() == Behaviour::Fail {
        return COperationStatus::InternalError;
    }

    // SAFETY: the host passes a valid, consistent source image.
    let src = unsafe { &*src };
    if !src.is_consistent() {
        return COperationStatus::InvalidParams;
    }
    // SAFETY: `is_consistent` checked data against width × height × 4.
    let src_data = unsafe { std::slice::from_raw_parts(src.data, src.data_len) };

    if behaviour() == Behaviour::NullOutput {
        return COperationStatus::Ok; // dst stays zeroed
    }

    let Some(mut out) = alloc_cimage(src.width, src.height) else {
        return COperationStatus::AllocationFailed;
    };
    // SAFETY: alloc_cimage returned a buffer of exactly data_len bytes.
    let out_data = unsafe { std::slice::from_raw_parts_mut(out.data, out.data_len) };
    for (s, d) in src_data.chunks_exact(4).zip(out_data.chunks_exact_mut(4)) {
        d[0] = 255 - s[0];
        d[1] = 255 - s[1];
        d[2] = 255 - s[2];
        d[3] = s[3];
    }

    match behaviour() {
        // The allocation stays correct; only the published geometry is wrong,
        // so the host still frees the right number of bytes when it rejects it.
        Behaviour::MismatchedDimensions => out.height += 1,
        Behaviour::UnknownFormat => {
            // SAFETY: writing a u32 into a repr(u32) field.  Nothing reads it
            // back as a CPixelFormat — the host goes through format_tag().
            unsafe {
                std::ptr::write_unaligned(std::ptr::from_mut(&mut out.format).cast::<u32>(), 99)
            }
        }
        _ => {}
    }

    // SAFETY: the host gave us a writable CImage to fill in.
    unsafe { *dst = out };
    COperationStatus::Ok
}

static OP_VTABLE: OperationVTable = OperationVTable {
    name: c"fake_invert".as_ptr(),
    describe: op_describe,
    apply: op_apply,
    destroy: op_destroy,
};

extern "C" fn plugin_op_count() -> usize {
    1
}

/// Claims an absurd operation count, to check the host's ceiling.
extern "C" fn plugin_op_count_absurd() -> usize {
    usize::MAX
}

extern "C" fn plugin_get_op(index: usize) -> *mut OperationVTable {
    if index == 0 {
        std::ptr::from_ref(&OP_VTABLE).cast_mut()
    } else {
        std::ptr::null_mut()
    }
}

extern "C" fn plugin_destroy() {
    PLUGIN_DESTROYS.fetch_add(1, Ordering::SeqCst);
}

extern "C" fn free_image_data(ptr: *mut u8, len: usize) {
    FREES.fetch_add(1, Ordering::SeqCst);
    // SAFETY: the host hands back exactly what alloc_cimage produced, once.
    unsafe { rasterlab_free_image_data(ptr, len) };
}

fn fake_vtable() -> PluginVTable {
    PluginVTable {
        api_version: PLUGIN_API_VERSION,
        metadata: CPluginMetadata {
            name: c"Fake Plugin".as_ptr(),
            version: c"1.2.3".as_ptr(),
            author: c"Tests".as_ptr(),
            description: c"In-process plugin for loader tests".as_ptr(),
        },
        operation_count: plugin_op_count,
        get_operation: plugin_get_op,
        free_image_data,
        decoder_extensions: std::ptr::null(),
        destroy: plugin_destroy,
    }
}

/// Wrap a vtable the way `PluginLoader::load` would, minus the shared library.
///
/// # Safety
/// `vtable` must outlive the returned plugin.  Every caller keeps it in a local
/// declared before the plugin, so it is dropped after.
unsafe fn wrap(vtable: &mut PluginVTable) -> RasterResult<DynPlugin> {
    unsafe { DynPlugin::from_vtable(std::ptr::from_mut(vtable), None) }
}

fn test_image() -> Image {
    Image::from_rgba8(2, 1, vec![10, 20, 30, 255, 200, 210, 220, 128]).expect("2×1 RGBA8")
}

// ---------------------------------------------------------------------------
// ABI shape
// ---------------------------------------------------------------------------

#[test]
fn the_abi_version_matches_what_the_api_crate_publishes() {
    // A plugin built against the api crate and this host must agree, so the
    // constant may only move together with the vtable layout.
    assert_eq!(PLUGIN_API_VERSION, 2);
}

#[test]
fn vtable_fields_sit_where_the_c_layout_puts_them() {
    use std::mem::offset_of;

    // repr(C), so every field follows the previous one in declaration order at
    // its natural alignment.  A reordering here silently breaks every plugin
    // built against a previous build of the crate without changing the version.
    assert_eq!(offset_of!(CImage, width), 0);
    assert_eq!(offset_of!(CImage, height), 4);
    assert_eq!(offset_of!(CImage, format), 8);
    assert_eq!(
        offset_of!(CImage, data),
        12usize.next_multiple_of(align_of::<*mut u8>())
    );
    assert_eq!(size_of::<CPixelFormat>(), 4);
    assert_eq!(size_of::<COperationStatus>(), 4);

    assert_eq!(offset_of!(PluginVTable, api_version), 0);
    assert!(offset_of!(PluginVTable, metadata) < offset_of!(PluginVTable, operation_count));
    assert!(offset_of!(PluginVTable, operation_count) < offset_of!(PluginVTable, get_operation));
    assert!(offset_of!(PluginVTable, get_operation) < offset_of!(PluginVTable, free_image_data));
    assert!(offset_of!(PluginVTable, free_image_data) < offset_of!(PluginVTable, destroy));

    assert_eq!(offset_of!(OperationVTable, name), 0);
    assert_eq!(size_of::<OperationVTable>(), 4 * size_of::<*const u8>());
}

// ---------------------------------------------------------------------------
// Loading and metadata validation
// ---------------------------------------------------------------------------

#[test]
fn a_null_vtable_is_rejected() {
    // SAFETY: from_vtable documents null as a handled input.
    let error =
        unsafe { DynPlugin::from_vtable(std::ptr::null_mut(), None) }.expect_err("null vtable");
    assert!(matches!(error, RasterError::Plugin(_)), "{error}");
}

#[test]
fn a_mismatched_abi_version_is_rejected_before_anything_else_is_read() {
    serialised(Behaviour::Invert, || {
        let mut vtable = fake_vtable();
        vtable.api_version = PLUGIN_API_VERSION + 1;
        // A name that would fail validation, to prove the version check runs first.
        vtable.metadata.name = std::ptr::null();

        // SAFETY: vtable outlives the (failed) plugin.
        let error = unsafe { wrap(&mut vtable) }.expect_err("version mismatch");
        assert!(
            matches!(
                error,
                RasterError::PluginApiVersionMismatch { expected, got }
                    if expected == PLUGIN_API_VERSION && got == PLUGIN_API_VERSION + 1
            ),
            "{error}"
        );
    });
}

#[test]
fn metadata_is_read_across_the_boundary() {
    serialised(Behaviour::Invert, || {
        let mut vtable = fake_vtable();
        // SAFETY: vtable outlives plugin — declared first, dropped last.
        let plugin = unsafe { wrap(&mut vtable) }.expect("valid plugin");
        let metadata = plugin.metadata();
        assert_eq!(metadata.name, "Fake Plugin");
        assert_eq!(metadata.version, "1.2.3");
        assert_eq!(metadata.author, "Tests");
        assert_eq!(metadata.description, "In-process plugin for loader tests");
        drop(plugin);
        assert_eq!(PLUGIN_DESTROYS.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn optional_metadata_fields_may_be_null() {
    serialised(Behaviour::Invert, || {
        let mut vtable = fake_vtable();
        vtable.metadata.version = std::ptr::null();
        vtable.metadata.author = std::ptr::null();
        vtable.metadata.description = std::ptr::null();
        // SAFETY: vtable outlives plugin.
        let plugin = unsafe { wrap(&mut vtable) }.expect("name alone is enough");
        let metadata = plugin.metadata();
        assert_eq!(metadata.name, "Fake Plugin");
        assert!(metadata.version.is_empty());
        assert!(metadata.author.is_empty());
        assert!(metadata.description.is_empty());
    });
}

#[test]
fn a_plugin_without_a_usable_name_is_rejected() {
    serialised(Behaviour::Invert, || {
        for name in [c"".as_ptr(), c"   ".as_ptr(), std::ptr::null()] {
            let mut vtable = fake_vtable();
            vtable.metadata.name = name;
            // SAFETY: vtable outlives the (failed) plugin.
            let error = unsafe { wrap(&mut vtable) }.expect_err("unusable name");
            assert!(matches!(error, RasterError::Plugin(_)), "{error}");
        }
    });
}

#[test]
fn non_utf8_metadata_is_rejected_rather_than_silently_blanked() {
    serialised(Behaviour::Invert, || {
        let mut vtable = fake_vtable();
        // 0xFF is not a valid UTF-8 lead byte.
        vtable.metadata.name = c"caf\xff".as_ptr();
        // SAFETY: vtable outlives the (failed) plugin.
        let error = unsafe { wrap(&mut vtable) }.expect_err("invalid UTF-8 name");
        assert!(
            error.to_string().contains("UTF-8"),
            "expected a UTF-8 complaint, got {error}"
        );
    });
}

// ---------------------------------------------------------------------------
// Operation enumeration
// ---------------------------------------------------------------------------

#[test]
fn operations_are_enumerated_and_named() {
    serialised(Behaviour::Invert, || {
        let mut vtable = fake_vtable();
        // SAFETY: vtable outlives plugin.
        let plugin = unsafe { wrap(&mut vtable) }.expect("valid plugin");
        let ops = plugin.operations();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].name(), "fake_invert");
        assert_eq!(ops[0].describe(), "Invert (fake plugin)");
    });
}

#[test]
fn an_absurd_operation_count_is_capped_instead_of_iterated() {
    serialised(Behaviour::Invert, || {
        let mut vtable = fake_vtable();
        vtable.operation_count = plugin_op_count_absurd;
        // SAFETY: vtable outlives plugin.
        let plugin = unsafe { wrap(&mut vtable) }.expect("valid plugin");
        // Terminates, and keeps only the operations that actually exist.
        assert_eq!(plugin.operations().len(), 1);
    });
}

#[test]
fn a_name_that_outlives_its_library_is_interned_not_borrowed() {
    // Operation::name is &'static str; two lookups of the same plugin name must
    // hand back the same interned allocation rather than a pointer into a
    // library that can be unloaded.
    let first = intern("plugin_op");
    let second = intern(&String::from("plugin_op"));
    assert_eq!(first, "plugin_op");
    assert!(std::ptr::eq(first, second));
}

#[test]
fn destroy_runs_once_however_many_clones_the_render_thread_made() {
    serialised(Behaviour::Invert, || {
        let mut vtable = fake_vtable();
        // SAFETY: vtable outlives plugin.
        let plugin = unsafe { wrap(&mut vtable) }.expect("valid plugin");
        let ops = plugin.operations();
        let clone = ops[0].clone_box();
        let clone_again = clone.clone_box();
        assert_eq!(DESTROYS.load(Ordering::SeqCst), 0);
        drop(ops);
        drop(clone);
        assert_eq!(DESTROYS.load(Ordering::SeqCst), 0, "still one live handle");
        drop(clone_again);
        assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);
    });
}

// ---------------------------------------------------------------------------
// apply(): output validation and buffer ownership
// ---------------------------------------------------------------------------

#[test]
fn a_well_behaved_operation_round_trips_pixels_and_frees_its_buffer() {
    serialised(Behaviour::Invert, || {
        let mut vtable = fake_vtable();
        // SAFETY: vtable outlives plugin.
        let plugin = unsafe { wrap(&mut vtable) }.expect("valid plugin");
        let ops = plugin.operations();
        let out = ops[0].apply(test_image()).expect("apply succeeds");
        assert_eq!(out.width, 2);
        assert_eq!(out.height, 1);
        assert_eq!(out.data, vec![245, 235, 225, 255, 55, 45, 35, 128]);
        assert_eq!(
            FREES.load(Ordering::SeqCst),
            1,
            "the output buffer goes back to the plugin that allocated it"
        );
    });
}

#[test]
fn a_failure_status_becomes_an_error_and_frees_nothing() {
    serialised(Behaviour::Fail, || {
        let mut vtable = fake_vtable();
        // SAFETY: vtable outlives plugin.
        let plugin = unsafe { wrap(&mut vtable) }.expect("valid plugin");
        let ops = plugin.operations();
        let error = ops[0].apply(test_image()).expect_err("plugin failed");
        assert!(
            error.to_string().contains("InternalError"),
            "expected the plugin's status code, got {error}"
        );
        assert_eq!(FREES.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn a_success_with_no_output_buffer_is_an_error() {
    serialised(Behaviour::NullOutput, || {
        let mut vtable = fake_vtable();
        // SAFETY: vtable outlives plugin.
        let plugin = unsafe { wrap(&mut vtable) }.expect("valid plugin");
        let ops = plugin.operations();
        let error = ops[0]
            .apply(test_image())
            .expect_err("nothing was produced");
        assert!(matches!(error, RasterError::Plugin(_)), "{error}");
    });
}

#[test]
fn an_output_whose_length_contradicts_its_dimensions_is_rejected_and_still_freed() {
    serialised(Behaviour::MismatchedDimensions, || {
        let mut vtable = fake_vtable();
        // SAFETY: vtable outlives plugin.
        let plugin = unsafe { wrap(&mut vtable) }.expect("valid plugin");
        let ops = plugin.operations();
        let error = ops[0]
            .apply(test_image())
            .expect_err("8 bytes cannot be a 2×2 image");
        assert!(
            error.to_string().contains("inconsistent"),
            "expected an inconsistency complaint, got {error}"
        );
        assert_eq!(
            FREES.load(Ordering::SeqCst),
            1,
            "rejecting the image must not leak the plugin's allocation"
        );
    });
}

#[test]
fn an_output_with_an_unknown_pixel_format_is_rejected_and_still_freed() {
    serialised(Behaviour::UnknownFormat, || {
        let mut vtable = fake_vtable();
        // SAFETY: vtable outlives plugin.
        let plugin = unsafe { wrap(&mut vtable) }.expect("valid plugin");
        let ops = plugin.operations();
        let error = ops[0]
            .apply(test_image())
            .expect_err("format tag 99 has no meaning in ABI v2");
        assert!(
            error.to_string().contains("format tag 99"),
            "expected the offending tag to be reported, got {error}"
        );
        assert_eq!(FREES.load(Ordering::SeqCst), 1);
    });
}

// ---------------------------------------------------------------------------
// PluginLoader: the parts that need a file on disk
// ---------------------------------------------------------------------------

#[test]
fn loading_a_path_that_is_not_a_library_reports_which_path_failed() {
    let missing = Path::new("/nonexistent/rasterlab/libnothing.so");
    let error = PluginLoader::load(missing).err().expect("no such library");
    assert!(
        error.to_string().contains("libnothing.so"),
        "expected the path in the message, got {error}"
    );
}

#[test]
fn loading_a_file_that_is_not_a_shared_object_is_an_error_not_a_crash() {
    let dir = std::env::temp_dir().join("rasterlab-plugin-loader-test");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("not-a-plugin.so");
    std::fs::write(&path, b"this is not an ELF file").expect("write junk");

    let error = PluginLoader::load(&path)
        .err()
        .expect("junk is not loadable");
    assert!(matches!(error, RasterError::Plugin(_)), "{error}");

    let _ = std::fs::remove_file(&path);
}
