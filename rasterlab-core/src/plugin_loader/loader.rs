//! Dynamic plugin loader using `libloading`.
//!
//! Loads `.so` / `.dylib` / `.dll` files that export `rasterlab_plugin_init`.
//! Wraps the raw C vtable in [`DynPlugin`] which implements the safe [`Plugin`] trait.
//!
//! # What the checks here are for
//!
//! A plugin is trusted native code — it runs in-process with the host's full
//! privileges and could scribble over host memory directly without ever
//! touching this ABI (see the `rasterlab_plugin_api` crate docs).  Validating
//! what comes back through the vtable is therefore not a security boundary; it
//! is how an ordinary plugin bug — a size computed in the wrong integer width, a
//! forgotten output allocation — becomes an error in the edit stack instead of a
//! segfault or a silently truncated image.

use std::collections::HashSet;
use std::ffi::CStr;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use libloading::Library;
use rasterlab_plugin_api::{
    PLUGIN_API_VERSION, PLUGIN_INIT_SYMBOL,
    types::{CImage, COperationStatus},
    vtable::{MAX_OPERATIONS, OperationVTable, PluginInitFn, PluginVTable},
};

use crate::{
    error::{RasterError, RasterResult},
    image::Image,
    traits::{
        format_handler::FormatHandler,
        operation::Operation,
        plugin::{Plugin, PluginMetadata},
    },
};

/// Longest string the loader will accept from a plugin's metadata.  The strings
/// are read with `CStr::from_ptr`, which walks to the first NUL byte; the cap
/// rejects the result afterwards so an unterminated buffer cannot become a
/// multi-megabyte plugin name in the UI.
const MAX_METADATA_LEN: usize = 1024;

// ---------------------------------------------------------------------------
// Name interning
// ---------------------------------------------------------------------------

/// `Operation::name` returns `&'static str`, but a plugin's name bytes live in
/// the loaded library and die when it unloads.  Interning copies each distinct
/// name onto the heap once and leaks it, which makes the `'static` honest at the
/// cost of a few bytes per distinct plugin operation ever loaded.
fn intern(name: &str) -> &'static str {
    static NAMES: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut names = NAMES
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = names.get(name) {
        return existing;
    }
    let leaked: &'static str = Box::leak(name.to_owned().into_boxed_str());
    names.insert(leaked);
    leaked
}

/// Read a plugin-supplied C string, rejecting anything that is not short,
/// null-terminated UTF-8.  `None` for a null pointer.
///
/// # Safety
/// `ptr` must be null or point to a NUL-terminated string that stays valid for
/// the duration of the call.
unsafe fn read_c_string(ptr: *const std::ffi::c_char, field: &str) -> RasterResult<Option<String>> {
    if ptr.is_null() {
        return Ok(None);
    }
    // SAFETY: caller guarantees a valid NUL-terminated string.
    let bytes = unsafe { CStr::from_ptr(ptr) }.to_bytes();
    if bytes.len() > MAX_METADATA_LEN {
        return Err(RasterError::Plugin(format!(
            "Plugin {field} is {} bytes, over the {MAX_METADATA_LEN}-byte limit",
            bytes.len()
        )));
    }
    let text = std::str::from_utf8(bytes)
        .map_err(|e| RasterError::Plugin(format!("Plugin {field} is not valid UTF-8: {e}")))?;
    Ok(Some(text.to_owned()))
}

// ---------------------------------------------------------------------------
// DynPlugin: wraps a loaded library's PluginVTable
// ---------------------------------------------------------------------------

/// A plugin loaded from a shared library.
///
/// Keeps the `Library` alive so that function pointers remain valid.
pub struct DynPlugin {
    /// Must be kept alive — drop order: vtable first, then library.
    vtable: *mut PluginVTable,
    /// `None` for a vtable that is not backed by a loaded library (a static
    /// vtable compiled into the host, as the tests below use).
    lib: Option<Arc<Library>>,
    metadata: PluginMetadata,
}

// SAFETY: The vtable pointer is only accessed through &self / &mut self.
// The Library keeps the code alive; no mutable aliasing occurs.
unsafe impl Send for DynPlugin {}
unsafe impl Sync for DynPlugin {}

impl std::fmt::Debug for DynPlugin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynPlugin")
            .field("name", &self.metadata.name)
            .field("version", &self.metadata.version)
            .field("dynamically_loaded", &self.lib.is_some())
            .finish()
    }
}

impl Drop for DynPlugin {
    fn drop(&mut self) {
        // SAFETY: vtable is valid for the lifetime of lib.
        unsafe {
            if !self.vtable.is_null() {
                ((*self.vtable).destroy)();
            }
        }
    }
}

impl DynPlugin {
    /// Validate a `PluginVTable` and wrap it.
    ///
    /// Split out from [`PluginLoader::load`] so the checks can be exercised
    /// against a vtable built in-process, without a shared library on disk.
    ///
    /// # Safety
    /// `vtable` must be null or point to a `PluginVTable` that stays valid for
    /// as long as the returned plugin — which `lib`, when present, guarantees.
    pub unsafe fn from_vtable(
        vtable: *mut PluginVTable,
        lib: Option<Arc<Library>>,
    ) -> RasterResult<Self> {
        if vtable.is_null() {
            return Err(RasterError::Plugin(
                "rasterlab_plugin_init returned null".into(),
            ));
        }

        // ABI version first: every other field's layout depends on it.
        let got = unsafe { (*vtable).api_version };
        if got != PLUGIN_API_VERSION {
            return Err(RasterError::PluginApiVersionMismatch {
                expected: PLUGIN_API_VERSION,
                got,
            });
        }

        // SAFETY: the version matched, so the metadata field is laid out as this
        // build expects and its pointers follow the ABI contract.
        let metadata = unsafe {
            let m = &(*vtable).metadata;
            let name = read_c_string(m.name, "name")?
                .ok_or_else(|| RasterError::Plugin("Plugin metadata has no name".into()))?;
            if name.trim().is_empty() {
                return Err(RasterError::Plugin("Plugin name is empty".into()));
            }
            PluginMetadata {
                name,
                version: read_c_string(m.version, "version")?.unwrap_or_default(),
                author: read_c_string(m.author, "author")?.unwrap_or_default(),
                description: read_c_string(m.description, "description")?.unwrap_or_default(),
            }
        };

        Ok(Self {
            vtable,
            lib,
            metadata,
        })
    }
}

impl Plugin for DynPlugin {
    fn metadata(&self) -> PluginMetadata {
        self.metadata.clone()
    }

    fn operations(&self) -> Vec<Box<dyn Operation>> {
        // SAFETY: vtable is valid, count/get_operation follow the plugin API contract.
        unsafe {
            let count = ((*self.vtable).operation_count)().min(MAX_OPERATIONS);
            (0..count)
                .filter_map(|i| {
                    let op_ptr = ((*self.vtable).get_operation)(i);
                    if op_ptr.is_null() {
                        return None;
                    }
                    let name = read_c_string((*op_ptr).name, "operation name")
                        .ok()
                        .flatten()
                        .filter(|n| !n.trim().is_empty())
                        .map_or("unknown_plugin_operation", |n| intern(&n));
                    Some(Box::new(DynOperation {
                        guard: Arc::new(VtableGuard { ptr: op_ptr }),
                        free_image_data: (*self.vtable).free_image_data,
                        name,
                        _lib: self.lib.clone(),
                    }) as Box<dyn Operation>)
                })
                .collect()
        }
    }

    fn format_handlers(&self) -> Vec<Box<dyn FormatHandler>> {
        // Plugins may expose additional format handlers in future ABI versions.
        // For ABI v2 we only support operations.
        vec![]
    }
}

// ---------------------------------------------------------------------------
// DynOperation: wraps a single OperationVTable
// ---------------------------------------------------------------------------

/// Guards a plugin-allocated `OperationVTable` pointer, calling `destroy`
/// exactly once when the last `Arc<VtableGuard>` drops.  This allows
/// `DynOperation` to be safely cloned (via `clone_box`) for the render
/// thread without risking a double-free or use-after-free.
struct VtableGuard {
    ptr: *mut OperationVTable,
}

impl Drop for VtableGuard {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: ptr was returned by `get_operation` and is destroyed
            // exactly once here.
            unsafe {
                ((*self.ptr).destroy)(self.ptr);
            }
        }
    }
}

// SAFETY: The vtable pointer points to code in the loaded library and is
// only accessed via immutable function-pointer calls.  The Library Arc
// keeps the code alive.
unsafe impl Send for VtableGuard {}
unsafe impl Sync for VtableGuard {}

struct DynOperation {
    guard: Arc<VtableGuard>,
    /// The plugin's own deallocator, copied out of its `PluginVTable`.  Output
    /// buffers must go back to the allocator that made them, which lives in the
    /// plugin, not in this binary.
    free_image_data: unsafe extern "C" fn(*mut u8, usize),
    /// Interned copy of the plugin's operation name; see [`intern`].
    name: &'static str,
    _lib: Option<Arc<Library>>,
}

// SAFETY: DynOperation only holds an Arc (Send+Sync) and a Library Arc.
unsafe impl Send for DynOperation {}
unsafe impl Sync for DynOperation {}

// DynOperation is not round-trip serialisable (it wraps a live pointer from a
// loaded library).  We implement Serialize/Deserialize manually so that it can
// participate in typetag's registry: serialisation records the name; deserialisation
// always fails with a clear message instructing the user to reload the plugin.
impl serde::Serialize for DynOperation {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = s.serialize_map(Some(2))?;
        map.serialize_entry("type", "dyn_operation")?;
        map.serialize_entry("plugin_op_name", self.name())?;
        map.end()
    }
}

impl<'de> serde::Deserialize<'de> for DynOperation {
    fn deserialize<D: serde::Deserializer<'de>>(_d: D) -> Result<Self, D::Error> {
        Err(serde::de::Error::custom(
            "DynOperation cannot be deserialised; the plugin must be re-loaded first",
        ))
    }
}

#[typetag::serde(name = "dyn_operation")]
impl Operation for DynOperation {
    fn name(&self) -> &'static str {
        self.name
    }

    fn clone_box(&self) -> Box<dyn Operation> {
        Box::new(DynOperation {
            guard: Arc::clone(&self.guard),
            free_image_data: self.free_image_data,
            name: self.name,
            _lib: self._lib.clone(),
        })
    }

    fn apply(&self, image: Image) -> RasterResult<Image> {
        let vtable = self.guard.ptr;
        let src = CImage {
            width: image.width,
            height: image.height,
            format: rasterlab_plugin_api::types::CPixelFormat::Rgba8,
            data: image.data.as_ptr().cast_mut(), // plugin must not free this
            data_len: image.data.len(),
        };

        // SAFETY: `vtable` came from `get_operation` and the guard keeps it (and
        // the library behind it) alive.  `src` describes the buffer `image` owns
        // for the duration of the call; `dst` is a zeroed CImage the plugin
        // fills in, whose contents are validated below before being read.
        let dst = unsafe {
            let mut dst = std::mem::zeroed::<CImage>();
            let status = ((*vtable).apply)(vtable, &src, &mut dst);
            if status != COperationStatus::Ok {
                // On a failure status the plugin owns whatever is in `dst`;
                // the ABI requires it not to have allocated anything.
                return Err(RasterError::Plugin(format!(
                    "Plugin operation '{}' returned error {:?}",
                    self.name(),
                    status
                )));
            }
            dst
        };

        // Validate before touching a single pixel: `data_len` is what a copy
        // would read, and a plugin that computed its output size in wrapping u32
        // arithmetic reports a length far shorter than the dimensions beside it.
        if !dst.is_consistent() {
            // The allocation is still the plugin's to release, even though we
            // are rejecting what it describes.
            // SAFETY: `dst.data` is either null (a no-op for the plugin's free
            // function) or the buffer `apply` allocated, freed once here.
            unsafe { (self.free_image_data)(dst.data, dst.data_len) };
            return Err(RasterError::Plugin(format!(
                "Plugin operation '{}' returned an inconsistent image: {}×{}, format tag {}, {} bytes",
                self.name(),
                dst.width,
                dst.height,
                dst.format_tag(),
                dst.data_len,
            )));
        }

        // SAFETY: `is_consistent` established that `data` is non-null and
        // `data_len` matches `width × height × 4`.
        let out_data = unsafe { std::slice::from_raw_parts(dst.data, dst.data_len) }.to_vec();

        // Hand the buffer back to the allocator that made it, inside the plugin.
        // SAFETY: `dst.data` came from this plugin's `apply` and is freed once.
        unsafe { (self.free_image_data)(dst.data, dst.data_len) };

        Image::from_rgba8(dst.width, dst.height, out_data)
    }

    fn describe(&self) -> String {
        let vtable = self.guard.ptr;
        // SAFETY: the guard keeps the vtable and its library alive; `describe`
        // returns a string valid until the operation is destroyed.
        unsafe {
            let desc_ptr = ((*vtable).describe)(vtable);
            read_c_string(desc_ptr, "operation description")
                .ok()
                .flatten()
                .unwrap_or_else(|| self.name().to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// PluginLoader
// ---------------------------------------------------------------------------

/// Loads plugin shared libraries from disk.
///
/// Loading a plugin executes its initialiser with the host's privileges; see the
/// module docs.  The loader validates the ABI version and the metadata a plugin
/// reports, and every operation output is checked in
/// [`DynOperation::apply`], but a plugin is trusted code by construction.
pub struct PluginLoader;

impl PluginLoader {
    /// Load a plugin from `path`.
    ///
    /// # Errors
    /// - [`RasterError::Plugin`] if the library can't be opened, the init symbol
    ///   is absent, the returned vtable is null, or the metadata is unusable.
    /// - [`RasterError::PluginApiVersionMismatch`] if the plugin was built
    ///   against a different ABI version.
    pub fn load(path: &Path) -> RasterResult<Box<dyn Plugin>> {
        // SAFETY: We immediately check the ABI version and only call well-defined
        // extern "C" functions.  The Library is kept alive by the DynPlugin.
        let lib = unsafe {
            Library::new(path).map_err(|e| {
                RasterError::Plugin(format!("Cannot open '{}': {}", path.display(), e))
            })?
        };

        // SAFETY: the symbol is called with the signature the ABI documents; a
        // library exporting `rasterlab_plugin_init` with a different signature is
        // outside what any check here can detect.
        let vtable_ptr: *mut PluginVTable = unsafe {
            let init_fn: libloading::Symbol<PluginInitFn> =
                lib.get(PLUGIN_INIT_SYMBOL).map_err(|e| {
                    RasterError::Plugin(format!(
                        "'{}' does not export rasterlab_plugin_init: {}",
                        path.display(),
                        e
                    ))
                })?;
            init_fn()
        };

        // SAFETY: the vtable is owned by the library, which the DynPlugin keeps
        // alive through the Arc handed over here.
        let plugin = unsafe { DynPlugin::from_vtable(vtable_ptr, Some(Arc::new(lib))) }.map_err(
            |e| match e {
                RasterError::Plugin(message) => {
                    RasterError::Plugin(format!("'{}': {}", path.display(), message))
                }
                other => other,
            },
        )?;

        Ok(Box::new(plugin))
    }
}

#[cfg(test)]
mod tests;
