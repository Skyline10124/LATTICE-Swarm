use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use lattice::agent::{
    default_tool_definitions,
    tool_registry::{ToolHandler, ToolRegistry},
};
use lattice::bus::AgentRegistry;
use lattice::plugin::registry::PluginRegistry;
use lattice::runtime::Runtime;

pub(crate) fn model_runtime(credentials: HashMap<String, String>) -> Runtime {
    Runtime::builder().credentials(credentials).build()
}

pub(crate) fn pipeline_runtime(
    agents_dir: Option<&str>,
    plugins_dir: Option<&str>,
    credentials: HashMap<String, String>,
) -> Result<(Runtime, PathBuf)> {
    let dir = crate::commands::safe_agents_dir(agents_dir).map_err(anyhow::Error::msg)?;
    let registry = AgentRegistry::load_dir(&dir)
        .map_err(|e| anyhow::anyhow!("Failed to load agents: {}", e))?;
    let plugin_dir = plugins_dir.map(PathBuf::from);
    let plugin_registry = Arc::new(PluginRegistry::new());
    lattice_plugins::register_official_plugins(&plugin_registry)
        .map_err(|err| anyhow::anyhow!("Failed to register official plugins: {err}"))?;
    let tool_registry = Arc::new(default_tool_registry());

    let mut builder = Runtime::builder()
        .name("swarm-pipeline")
        .credentials(credentials)
        .agent_registry(registry)
        .plugin_registry(plugin_registry.clone())
        .tool_registry(tool_registry);

    if let Some(plugin_dir) = plugin_dir.as_deref() {
        builder = builder
            .load_plugin_dir(plugin_dir.to_path_buf())?
            .watch_plugin_dir(plugin_dir.to_path_buf())?;
    }

    let runtime = builder.build();
    Ok((runtime, dir))
}

fn default_tool_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    for definition in default_tool_definitions() {
        let name = definition.name.clone();
        registry.register(
            &name,
            ToolHandler::Native(Arc::new(move |_| {
                Ok("execution delegated to Agent tool executor".into())
            })),
            definition,
        );
    }
    registry
}
