//! Dynamic plugin loader using `libloading`.
//!
//! Loads `.so` / `.dylib` / `.dll` files that export `rasterlab_plugin_init`.
//! Wraps the raw C vtable in [`DynPlugin`] which implements the safe [`Plugin`] trait.

use std::ffi::CStr;
use std::path::Path;
use std::sync::Arc;

use libloading::Library;
use rasterlab_plugin_api::{
    PLUGIN_API_VERSION, PLUGIN_INIT_SYMBOL,
    types::{CImage, COperationStatus},
    vtable::{OperationVTable, PluginInitFn, PluginVTable},
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

// ---------------------------------------------------------------------------
// DynPlugin: wraps a loaded library's PluginVTable
// ---------------------------------------------------------------------------

/// A plugin loaded from a shared library.
///
/// Keeps the `Library` alive so that function pointers remain valid.
pub struct DynPlugin {
    /// Must be kept alive — drop order: vtable first, then library.
    vtable: *mut PluginVTable,
    _lib: Arc<Library>,
    metadata: PluginMetadata,
}

// SAFETY: The vtable pointer is only accessed through &self / &mut self.
// The Library keeps the code alive; no mutable aliasing occurs.
unsafe impl Send for DynPlugin {}
unsafe impl Sync for DynPlugin {}

impl Drop for DynPlugin {
    fn drop(&mut self) {
        // SAFETY: vtable is valid for the lifetime of _lib.
        unsafe {
            if !self.vtable.is_null() {
                ((*self.vtable).destroy)();
            }
        }
    }
}

impl Plugin for DynPlugin {
    fn metadata(&self) -> PluginMetadata {
        self.metadata.clone()
    }

    fn operations(&self) -> Vec<Box<dyn Operation>> {
        // SAFETY: vtable is valid, count/get_operation follow the plugin API contract.
        unsafe {
            let count = ((*self.vtable).operation_count)();
            (0..count)
                .filter_map(|i| {
                    let op_ptr = ((*self.vtable).get_operation)(i);
                    if op_ptr.is_null() {
                        None
                    } else {
                        Some(Box::new(DynOperation {
                            vtable: op_ptr,
                            _lib: Arc::clone(&self._lib),
                        }) as Box<dyn Operation>)
                    }
                })
                .collect()
        }
    }

    fn format_handlers(&self) -> Vec<Box<dyn FormatHandler>> {
        // Plugins may expose additional format handlers in future ABI versions.
        // For ABI v1 we only support operations.
        vec![]
    }
}

// ---------------------------------------------------------------------------
// DynOperation: wraps a single OperationVTable
// ---------------------------------------------------------------------------

struct DynOperation {
    vtable: *mut OperationVTable,
    _lib: Arc<Library>,
}

// SAFETY: same reasoning as DynPlugin.
unsafe impl Send for DynOperation {}
unsafe impl Sync for DynOperation {}

impl Drop for DynOperation {
    fn drop(&mut self) {
        unsafe {
            if !self.vtable.is_null() {
                ((*self.vtable).destroy)(self.vtable);
            }
        }
    }
}

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
        unsafe {
            if self.vtable.is_null() {
                return "unknown";
            }
            let ptr = (*self.vtable).name;
            if ptr.is_null() {
                return "unknown";
            }
            // SAFETY: plugin guarantees null-terminated UTF-8 static lifetime string
            CStr::from_ptr(ptr).to_str().unwrap_or("unknown")
        }
    }

    fn apply(&self, image: Image) -> RasterResult<Image> {
        unsafe {
            // Build CImage from our Image
            let src = CImage {
                width: image.width,
                height: image.height,
                format: rasterlab_plugin_api::types::CPixelFormat::Rgba8,
                data: image.data.as_ptr() as *mut u8, // plugin must not free this
                data_len: image.data.len(),
            };

            // Output CImage (plugin fills this)
            let mut dst = std::mem::zeroed::<CImage>();

            let status = ((*self.vtable).apply)(self.vtable, &src, &mut dst);

            if status != COperationStatus::Ok {
                return Err(RasterError::Plugin(format!(
                    "Plugin operation '{}' returned error {:?}",
                    self.name(),
                    status
                )));
            }

            if dst.data.is_null() || dst.data_len == 0 {
                return Err(RasterError::Plugin(
                    "Plugin returned empty image buffer".into(),
                ));
            }

            // Take ownership of the plugin-allocated buffer
            let out_data = std::slice::from_raw_parts(dst.data, dst.data_len).to_vec();
            // Free the plugin's allocation
            rasterlab_plugin_api::types::rasterlab_free_image_data(dst.data, dst.data_len);

            Image::from_rgba8(dst.width, dst.height, out_data)
        }
    }

    fn describe(&self) -> String {
        unsafe {
            if self.vtable.is_null() {
                return "Plugin operation".into();
            }
            let desc_ptr = ((*self.vtable).describe)(self.vtable);
            if desc_ptr.is_null() {
                return self.name().to_string();
            }
            CStr::from_ptr(desc_ptr)
                .to_str()
                .unwrap_or("Plugin operation")
                .to_owned()
        }
    }
}

// ---------------------------------------------------------------------------
// PluginLoader
// ---------------------------------------------------------------------------

/// Loads plugin shared libraries from disk.
pub struct PluginLoader;

impl PluginLoader {
    /// Load a plugin from `path`.
    ///
    /// # Errors
    /// - [`RasterError::Plugin`] if the library can't be opened, the init symbol
    ///   is absent, or the ABI version doesn't match.
    pub fn load(path: &Path) -> RasterResult<Box<dyn Plugin>> {
        // SAFETY: We immediately check the ABI version and only call well-defined
        // extern "C" functions.  The Library is kept alive by the DynPlugin.
        let lib = unsafe {
            Library::new(path).map_err(|e| {
                RasterError::Plugin(format!("Cannot open '{}': {}", path.display(), e))
            })?
        };

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

        if vtable_ptr.is_null() {
            return Err(RasterError::Plugin(
                "rasterlab_plugin_init returned null".into(),
            ));
        }

        // ABI version check
        let got = unsafe { (*vtable_ptr).api_version };
        if got != PLUGIN_API_VERSION {
            return Err(RasterError::PluginApiVersionMismatch {
                expected: PLUGIN_API_VERSION,
                got,
            });
        }

        // Extract metadata while the library is still loaded
        let metadata = unsafe {
            let m = &(*vtable_ptr).metadata;
            let s = |p: *const std::ffi::c_char| {
                if p.is_null() {
                    String::new()
                } else {
                    CStr::from_ptr(p).to_str().unwrap_or("").to_owned()
                }
            };
            PluginMetadata {
                name: s(m.name),
                version: s(m.version),
                author: s(m.author),
                description: s(m.description),
            }
        };

        Ok(Box::new(DynPlugin {
            vtable: vtable_ptr,
            _lib: Arc::new(lib),
            metadata,
        }))
    }
}
