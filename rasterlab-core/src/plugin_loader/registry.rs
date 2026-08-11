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
///
/// Registering a plugin means running its code in this process with the user's
/// full privileges; see [`PluginLoader`] and the `rasterlab_plugin_api` crate
/// docs for what the ABI does and does not guarantee.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{operation::Operation, plugin::Plugin};

    struct StaticPlugin;

    impl Plugin for StaticPlugin {
        fn metadata(&self) -> PluginMetadata {
            PluginMetadata {
                name: "Built-in".into(),
                version: "0.1.0".into(),
                author: "RasterLab".into(),
                description: "A plugin with no FFI behind it".into(),
            }
        }

        fn operations(&self) -> Vec<Box<dyn Operation>> {
            vec![]
        }
    }

    #[test]
    fn a_registered_plugin_shows_up_in_the_listing() {
        let registry = PluginRegistry::new();
        assert!(registry.list().is_empty());
        registry.register(Box::new(StaticPlugin));
        let listed = registry.list();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Built-in");
    }

    #[test]
    fn a_plugin_that_fails_to_load_is_not_registered() {
        let registry = PluginRegistry::new();
        let error = registry
            .load_plugin(Path::new("/nonexistent/rasterlab/libnothing.so"))
            .expect_err("no such library");
        assert!(error.to_string().contains("libnothing.so"), "{error}");
        assert!(registry.list().is_empty());
    }

    #[test]
    fn scanning_a_directory_reports_per_plugin_errors_without_aborting() {
        let dir = std::env::temp_dir().join("rasterlab-plugin-registry-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(dir.join("broken.so"), b"not an ELF file").expect("write junk");
        // Non-library files in the directory are ignored entirely.
        std::fs::write(dir.join("notes.txt"), b"ignore me").expect("write text");

        let errors = registry_errors(&dir);
        assert_eq!(errors.len(), 1, "only the .so is treated as a plugin");
        assert!(errors[0].0.ends_with("broken.so"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn registry_errors(dir: &Path) -> Vec<(std::path::PathBuf, crate::error::RasterError)> {
        PluginRegistry::new().load_directory(dir)
    }

    #[test]
    fn scanning_a_missing_directory_is_not_an_error() {
        let registry = PluginRegistry::new();
        assert!(
            registry
                .load_directory(Path::new("/nonexistent/rasterlab/plugins"))
                .is_empty()
        );
    }
}
