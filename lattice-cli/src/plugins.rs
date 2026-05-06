use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use lattice_plugin::registry::PluginRegistry;

pub(crate) fn build_plugin_registry(local_dir: Option<&Path>) -> Result<Arc<PluginRegistry>> {
    let registry = Arc::new(PluginRegistry::new());
    lattice_plugins::register_official_plugins(&registry)
        .map_err(|err| anyhow::anyhow!("Failed to register official plugins: {err}"))?;
    if let Some(dir) = local_dir {
        register_local_plugins(&registry, dir)?;
    }
    Ok(registry)
}

pub(crate) fn register_local_plugins(registry: &PluginRegistry, dir: &Path) -> Result<()> {
    let bundles = lattice_plugin::watcher::load_registry_bundles(dir, false)
        .map_err(|err| anyhow::anyhow!("Failed to load plugins from '{}': {err}", dir.display()))?;
    for bundle in bundles {
        registry
            .replace(bundle)
            .map_err(|err| anyhow::anyhow!("Failed to register local plugin: {err}"))?;
    }
    Ok(())
}

pub(crate) fn sorted_plugin_names(registry: &PluginRegistry) -> Vec<String> {
    let mut names = registry.names();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names
}
