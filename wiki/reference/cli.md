# CLI Reference

The CLI binary is `lattice`.

Common commands:

```sh
lattice run <prompt> [--model <name>] [--provider <name>]
lattice run --pipeline <agent> <prompt>
lattice resolve <model> [--trace] [--provider <name>] [--json]
lattice models [--auth]
lattice doctor
lattice bus [--json]
lattice config init
lattice config get <key>
lattice config set <key> <value>
lattice sessions list
lattice sessions show <id>
lattice sessions rm <id>
lattice stats
lattice debug <model> [--prompt <text>] [--resolve-only]
lattice validate [--dir <path>]
lattice list agents
lattice new agent <name>
lattice tui [--model <name>]
```

## Sessions

Sessions preserve user, assistant, system and tool messages, including tool call metadata. They are stored in the user's config directory and can be resumed with `--continue` or `--session`.

## Pipelines

Pipeline mode loads Runtime agent profiles and official plugins registered from the `LATTICE-Plugins` submodule. Local manifest plugins can be added with the plugin directory option.
