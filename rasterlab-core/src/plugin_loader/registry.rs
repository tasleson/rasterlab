use std::{path::Path, sync::RwLock};

use crate::{
    error::RasterResult,
    traits::plugin::{Plugin, PluginMetadata},
};

use super::loader::PluginLoader;

/// Thread-safe registry of loaded plugins.
///
/// The GUI's plugin manager panel uses this to enumerate active plugins and
/// expose their contributed operations in the tool palette.
#[derive(Default)]
pub struct PluginRegistry {
    plugins: RwLock<Vec<Box<dyn Plugin>>>,
}

impl PluginRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load and register a plugin from `path`.
    pub fn load_plugin(&self, path: &Path) -> RasterResult<()> {
        let plugin = PluginLoader::load(path)?;
        let mut guard = self.plugins.write().expect("PluginRegistry lock poisoned");
        guard.push(plugin);
        Ok(())
    }

    /// Register an already-constructed plugin (e.g. a built-in static plugin).
    pub fn register(&self, plugin: Box<dyn Plugin>) {
        let mut guard = self.plugins.write().expect("PluginRegistry lock poisoned");
        guard.push(plugin);
    }

    /// Metadata for all loaded plugins.
    pub fn list(&self) -> Vec<PluginMetadata> {
        let guard = self.plugins.read().expect("PluginRegistry lock poisoned");
        guard.iter().map(|p| p.metadata()).collect()
    }

    /// Load all `.so` / `.dylib` / `.dll` files in `dir`.
    ///
    /// Errors from individual plugins are logged and skipped; they do not abort
    /// the scan.
    pub fn load_directory(
        &self,
        dir: &Path,
    ) -> Vec<(std::path::PathBuf, crate::error::RasterError)> {
        let mut errors = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return errors;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let is_plugin = matches!(ext, "so" | "dylib" | "dll");
            if is_plugin && let Err(e) = self.load_plugin(&path) {
                errors.push((path, e));
            }
        }
        errors
    }
}
