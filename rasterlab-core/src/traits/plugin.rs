use crate::traits::{format_handler::FormatHandler, operation::Operation};

/// Metadata identifying a loaded plugin.
#[derive(Debug, Clone)]
pub struct PluginMetadata {
    pub name:        String,
    pub version:     String,
    pub author:      String,
    pub description: String,
}

/// Safe Rust wrapper around a loaded plugin (static or dynamic).
///
/// The [`plugin_loader`][crate::plugin_loader] module wraps the raw C vtable in a
/// `DynPlugin` that implements this trait.  Built-in "plugins" (e.g. bundled
/// third-party filters) can implement this trait directly without any FFI.
pub trait Plugin: Send + Sync {
    /// Identity and description of this plugin.
    fn metadata(&self) -> PluginMetadata;

    /// Operations contributed by this plugin.
    ///
    /// Each call returns fresh `Box<dyn Operation>` instances ready to be
    /// added to a pipeline.
    fn operations(&self) -> Vec<Box<dyn Operation>>;

    /// Additional format handlers contributed by this plugin (may be empty).
    fn format_handlers(&self) -> Vec<Box<dyn FormatHandler>> {
        vec![]
    }
}
