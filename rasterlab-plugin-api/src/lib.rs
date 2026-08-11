//! # RasterLab Plugin API
//!
//! Stable C ABI for RasterLab plugins.  This crate has **no external dependencies**
//! so plugin authors can link against it without pulling in the full workspace graph.
//!
//! ## Trust model
//!
//! **A plugin is trusted native code.**  It is a shared library loaded into the
//! host process with `dlopen`, so it runs with the full privileges of the user
//! running RasterLab: it can read and write any file that user can, open
//! sockets, and corrupt any host memory it can reach.  Nothing in this ABI is a
//! security boundary — there is no sandbox, no seccomp filter, and no separate
//! address space, and the checks the loader performs are there to turn *honest
//! plugin bugs* into clean error messages, not to contain a hostile plugin.
//! Loading a plugin is exactly as consequential as running any other program.
//! Install plugins only from sources you would trust with an executable.
//!
//! Two consequences for plugin authors, since the host cannot enforce either:
//!
//! - **Do not let a panic cross the boundary.**  The vtable functions are
//!   `extern "C"`, so an unwind out of one aborts the whole process.  Catch
//!   panics at the boundary and return a [`COperationStatus`] instead.
//! - **The vtable's function-pointer fields must be non-null.**  Rust cannot
//!   represent a null `extern "C" fn`, so the host has nothing to check.
//!
//! ## Writing a plugin
//!
//! 1. Create a `cdylib` crate that depends only on this crate.
//! 2. Implement one or more [`OperationVTable`] instances.
//! 3. Export `rasterlab_plugin_init` returning a `*mut PluginVTable`.
//!
//! See `plugins/example-plugin` for a complete example.
//!
//! ## Memory ownership
//!
//! Host and plugin have separate allocators.  Buffers allocated by the plugin
//! are freed by the plugin, through [`PluginVTable::free_image_data`]; see the
//! [`vtable`] module docs for why this is a vtable entry rather than an exported
//! symbol.
//!
//! ## ABI stability
//!
//! [`PLUGIN_API_VERSION`] is bumped whenever the vtable layout changes.
//! The host rejects plugins whose `api_version` doesn't match.

pub mod types;
pub mod vtable;

pub use types::*;
pub use vtable::*;

/// Current ABI version.  Both host and plugin must agree on this value.
///
/// History:
/// - `1` — initial layout.
/// - `2` — added [`PluginVTable::free_image_data`], so plugin allocations are
///   released by the plugin's own allocator.
pub const PLUGIN_API_VERSION: u32 = 2;

/// Symbol name that every plugin shared library must export.
pub const PLUGIN_INIT_SYMBOL: &[u8] = b"rasterlab_plugin_init\0";
