# LATTICE-Swarm Wiki

![LATTICE banner](../logo-banner.svg)

This wiki documents the user-facing Swarm repository: CLI, TUI, submodule layout and day-to-day development workflow.

## Quick Navigation

| Topic | Page |
| --- | --- |
| Quickstart | [getting-started/quickstart](getting-started/quickstart.md) |
| Installation | [getting-started/installation](getting-started/installation.md) |
| Submodule workflow | [development/submodules](development/submodules.md) |
| CLI reference | [reference/cli](reference/cli.md) |
| TUI reference | [reference/tui](reference/tui.md) |
| Runtime layout | [operations/runtime-layout](operations/runtime-layout.md) |
| Troubleshooting | [operations/troubleshooting](operations/troubleshooting.md) |

## Repository Role

Swarm is the entrypoint repository. It is the best repository to clone when using the full system locally, because it includes Runtime and Plugins as submodules.

Runtime and Swarm are the main maintenance repositories. Plugins is independently maintained but consumed by Swarm as a submodule.
