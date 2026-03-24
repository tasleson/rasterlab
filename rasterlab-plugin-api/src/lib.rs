//! # RasterLab Plugin API
//!
//! Stable C ABI for RasterLab plugins.  This crate has **no external dependencies**
//! so plugin authors can link against it without pulling in the full workspace graph.
//!
//! ## Writing a plugin
//!
//! 1. Create a `cdylib` crate that depends only on this crate.
//! 2. Implement one or more `OperationVTable` instances.
//! 3. Export `rasterlab_plugin_init` returning a `*mut PluginVTable`.
//!
//! See `plugins/example-plugin` for a complete example.
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
pub const PLUGIN_API_VERSION: u32 = 1;

/// Symbol name that every plugin shared library must export.
pub const PLUGIN_INIT_SYMBOL: &[u8] = b"rasterlab_plugin_init\0";
