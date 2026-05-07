# Runtime Layout

Swarm is not the runtime implementation. It hosts the CLI/TUI and submodule pointers.

```text
Swarm CLI/TUI/main agent
  → Runtime crate
  → official Plugins
```

## Main Maintenance Repositories

- Runtime: execution internals, model calls, agent loop, plugin runtime and bus orchestration.
- Swarm: CLI/TUI, main coding agent, submodule pointers and user-facing workflows.
- Plugins: official typed plugins.

The legacy mono-repo at `~/lattice` is no longer maintained.

## Why Submodules

Submodules let Swarm pin known-good Runtime and Plugins commits while still allowing Runtime and Plugins to release independently.
